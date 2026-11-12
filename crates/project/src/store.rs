//! The index's store: `index.db` in the project cache, an SQLite database written with Turso (SQLite in Rust). A row
//! per file the index read - its stamp, when it was recorded, the hash of its content - and for an `node.toml` what
//! the index needs of it without parsing it again: its type, uid and variant names, its references (a table of
//! their own), its error, and its text, which an asset parses only when something asks for its fields. Opening reads
//! the rows; a watcher batch rewrites only its own, in one transaction.
//!
//! Only the process holding the project lock opens it (`lock.rs`): Turso does not share a database with other
//! processes.

use anyhow::{Context, Result};
use pollster::block_on;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use turso::Value;

/// Raised whenever the tables change shape; a store of another version is emptied.
const VERSION: i64 = 7;
/// Rows a write must touch to be checkpointed at once.
const CHECKPOINT_AFTER: usize = 1000;

/// A modification time as seconds and nanoseconds since the epoch.
pub type Stamp = (u64, u32);

/// What the index needs of an `node.toml` without parsing it again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declaration {
    /// What of its stack says what it makes.
    pub shape: String,
    /// The uid it names itself, as declarations did before changesets.
    pub uid: Option<String>,
    /// Its `variants`' names, in order.
    pub variants: Vec<String>,
    /// Its references: (slot or field, uid, target path, the output of the asset it names - empty for the asset).
    pub references: Vec<(String, String, String, String)>,
    /// Why it cannot be parsed.
    pub error: Option<String>,
    pub text: Arc<str>,
}

/// What the index knows of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub size: u64,
    pub modified: Stamp,
    /// When it was read: a record is trusted only for a file last modified before (git's "racily clean" case).
    pub recorded: Stamp,
    pub sha1: String,
    pub declaration: Option<Declaration>,
}

pub struct Store {
    _db: turso::Database,
    conn: turso::Connection,
    /// What is to be written and is not yet (`write_later`).
    later: std::sync::Mutex<Later>,
}

/// What waits to be written: each file's record, or (none) that it is gone; and since when the first of them waits.
type Later = (HashMap<PathBuf, Option<Record>>, Option<std::time::Instant>);

/// How long what is to be written waits for more before whoever holds the store, asked while it has nothing else
/// to do (`Store::tick`), writes it; and how long at most, where nobody asks.
const LATER: std::time::Duration = std::time::Duration::from_secs(2);
const AT_MOST: std::time::Duration = std::time::Duration::from_secs(30);

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

fn int(value: &Value) -> i64 {
    value.as_integer().copied().unwrap_or(0)
}

fn text(value: &Value) -> Option<String> {
    value.as_text().cloned()
}

fn stamp(secs: &Value, nanos: &Value) -> Stamp {
    (u64::try_from(int(secs)).unwrap_or(0), u32::try_from(int(nanos)).unwrap_or(0))
}

fn wide(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl Store {
    /// The store at `path`, made if missing, emptied if another version wrote it.
    ///
    /// # Errors
    /// The database cannot be opened or its tables made.
    pub fn open(path: &Path) -> Result<Store> {
        let name = path.to_str().with_context(|| format!("{}: not a UTF-8 path", path.display()))?;
        // The project lock keeps every other process out of the cache, so the database's own lock adds nothing; on
        // Windows it is a byte-range lock, which a WSL folder's share refuses, so it is turned off there (turso has
        // only this switch for it).
        #[cfg(windows)]
        // SAFETY: setting the environment is safe on Windows, with other threads running too.
        unsafe {
            std::env::set_var("LIMBO_DISABLE_FILE_LOCK", "1");
        }
        let db = block_on(turso::Builder::new_local(name).build()).with_context(|| name.to_string())?;
        let conn = db.connect()?;
        let store = Store { _db: db, conn, later: std::sync::Mutex::default() };
        let mut mode = block_on(store.conn.query("PRAGMA journal_mode = WAL", ()))?;
        while block_on(mode.next())?.is_some() {}
        drop(mode);
        if store.version()? != VERSION {
            block_on(store.conn.execute_batch(
                "DROP TABLE IF EXISTS file; DROP TABLE IF EXISTS reference;
                 CREATE TABLE file (path TEXT PRIMARY KEY, size INTEGER NOT NULL, secs INTEGER NOT NULL,
                     nanos INTEGER NOT NULL, recorded_secs INTEGER NOT NULL, recorded_nanos INTEGER NOT NULL,
                     sha1 TEXT NOT NULL, declaration INTEGER NOT NULL, node_type TEXT, uid TEXT, variants TEXT,
                     error TEXT, text TEXT);
                 CREATE TABLE reference (path TEXT NOT NULL, slot TEXT NOT NULL, uid TEXT NOT NULL,
                     target TEXT NOT NULL, output TEXT NOT NULL);
                 CREATE INDEX reference_path ON reference (path);",
            ))?;
            let mut set = block_on(store.conn.query(&format!("PRAGMA user_version = {VERSION}"), ()))?;
            while block_on(set.next())?.is_some() {}
        }
        Ok(store)
    }

    fn version(&self) -> Result<i64> {
        let mut rows = block_on(self.conn.query("PRAGMA user_version", ()))?;
        let version = block_on(rows.next())?.map(|r| r.get_value(0)).transpose()?.map_or(0, |v| int(&v));
        while block_on(rows.next())?.is_some() {}
        Ok(version)
    }

    /// Every record, by the file's path.
    ///
    /// # Errors
    /// The database cannot be read.
    pub fn load(&self) -> Result<HashMap<PathBuf, Record>> {
        let mut references: HashMap<String, Vec<(String, String, String, String)>> = HashMap::new();
        let mut rows = block_on(self.conn.query("SELECT path, slot, uid, target, output FROM reference", ()))?;
        while let Some(row) = block_on(rows.next())? {
            let v: Vec<Value> = (0..5).map(|i| row.get_value(i)).collect::<turso::Result<_>>()?;
            let field = |i: usize| text(&v[i]).unwrap_or_default();
            references.entry(field(0)).or_default().push((field(1), field(2), field(3), field(4)));
        }
        let mut out = HashMap::new();
        let mut rows = block_on(self.conn.query(
            "SELECT path, size, secs, nanos, recorded_secs, recorded_nanos, sha1, declaration, node_type, uid,
                 variants, error, text FROM file",
            (),
        ))?;
        while let Some(row) = block_on(rows.next())? {
            let v: Vec<Value> = (0..13).map(|i| row.get_value(i)).collect::<turso::Result<_>>()?;
            let path = text(&v[0]).unwrap_or_default();
            let declaration = (int(&v[7]) != 0).then(|| Declaration {
                shape: text(&v[8]).unwrap_or_default(),
                uid: text(&v[9]),
                variants: text(&v[10]).map_or_else(Vec::new, |s| s.lines().map(str::to_string).collect()),
                references: references.remove(&path).unwrap_or_default(),
                error: text(&v[11]),
                text: text(&v[12]).unwrap_or_default().into(),
            });
            out.insert(
                PathBuf::from(&path),
                Record {
                    size: u64::try_from(int(&v[1])).unwrap_or(0),
                    modified: stamp(&v[2], &v[3]),
                    recorded: stamp(&v[4], &v[5]),
                    sha1: text(&v[6]).unwrap_or_default(),
                    declaration,
                },
            );
        }
        Ok(out)
    }

    /// Write records and forget files in a moment, with what else comes to be written by then: one transaction for
    /// all of it, where each would be one of its own. The store is what the next opening is spared reading again, so
    /// what was not written when a process ends without a word is read again then, and nothing else.
    ///
    /// # Errors
    /// What waited long enough could not be written.
    pub fn write_later<'a>(
        &self,
        records: impl IntoIterator<Item = (&'a Path, &'a Record)>,
        gone: impl IntoIterator<Item = &'a Path>,
    ) -> Result<()> {
        let due = {
            let mut later = self.later.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            later.0.extend(records.into_iter().map(|(path, record)| (path.to_path_buf(), Some(record.clone()))));
            later.0.extend(gone.into_iter().map(|path| (path.to_path_buf(), None)));
            let since = *later.1.get_or_insert_with(std::time::Instant::now);
            !later.0.is_empty() && since.elapsed() > AT_MOST
        };
        if due { self.flush() } else { Ok(()) }
    }

    /// Write what has waited a moment (`write_later`): asked by whoever holds the store between the things it does,
    /// so that the writing is on the path of none of them.
    ///
    /// # Errors
    /// The database cannot be written.
    pub fn tick(&self) -> Result<()> {
        let due = {
            let later = self.later.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            later.1.is_some_and(|since| since.elapsed() > LATER)
        };
        if due { self.flush() } else { Ok(()) }
    }

    /// Write what waits to be (`write_later`).
    ///
    /// # Errors
    /// The database cannot be written.
    pub fn flush(&self) -> Result<()> {
        let (waiting, _) = std::mem::take(&mut *self.later.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        if waiting.is_empty() {
            return Ok(());
        }
        let records = waiting.iter().filter_map(|(path, record)| Some((path.as_path(), record.as_ref()?)));
        let gone = waiting.iter().filter(|(_, record)| record.is_none()).map(|(path, _)| path.as_path());
        self.write(records, gone)
    }

    /// Write records and drop the files gone, in one transaction.
    ///
    /// # Errors
    /// The database cannot be written.
    pub fn write<'a>(
        &self,
        records: impl IntoIterator<Item = (&'a Path, &'a Record)>,
        gone: impl IntoIterator<Item = &'a Path>,
    ) -> Result<()> {
        block_on(self.conn.execute("BEGIN", ()))?;
        let result = self.write_in_transaction(records, gone);
        block_on(self.conn.execute(if result.is_ok() { "COMMIT" } else { "ROLLBACK" }, ()))?;
        let written = result?;
        // A large write (a cold build, a re-extraction) is moved into the database at once, so the next opening
        // does not replay it from the log.
        if written > CHECKPOINT_AFTER {
            let mut rows = block_on(self.conn.query("PRAGMA wal_checkpoint(TRUNCATE)", ()))?;
            while block_on(rows.next())?.is_some() {}
        }
        Ok(())
    }

    fn write_in_transaction<'a>(
        &self,
        records: impl IntoIterator<Item = (&'a Path, &'a Record)>,
        gone: impl IntoIterator<Item = &'a Path>,
    ) -> Result<usize> {
        let mut written = 0;
        let mut drop_file = block_on(self.conn.prepare("DELETE FROM file WHERE path = ?1"))?;
        let mut drop_references = block_on(self.conn.prepare("DELETE FROM reference WHERE path = ?1"))?;
        let mut put = block_on(self.conn.prepare(
            "INSERT INTO file (path, size, secs, nanos, recorded_secs, recorded_nanos, sha1, declaration, node_type,
                 uid, variants, error, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT (path) DO UPDATE SET size = excluded.size, secs = excluded.secs, nanos = excluded.nanos,
                 recorded_secs = excluded.recorded_secs, recorded_nanos = excluded.recorded_nanos,
                 sha1 = excluded.sha1, declaration = excluded.declaration, node_type = excluded.node_type,
                 uid = excluded.uid, variants = excluded.variants, error = excluded.error, text = excluded.text",
        ))?;
        let mut reference = block_on(
            self.conn.prepare("INSERT INTO reference (path, slot, uid, target, output) VALUES (?1, ?2, ?3, ?4, ?5)"),
        )?;
        for path in gone {
            written += 1;
            let path = path.to_string_lossy();
            block_on(drop_file.execute((path.as_ref(),)))?;
            block_on(drop_references.execute((path.as_ref(),)))?;
        }
        for (path, r) in records {
            written += 1;
            let path = path.to_string_lossy();
            let d = r.declaration.as_ref();
            block_on(drop_references.execute((path.as_ref(),)))?;
            block_on(put.execute((
                path.as_ref(),
                wide(r.size),
                wide(r.modified.0),
                i64::from(r.modified.1),
                wide(r.recorded.0),
                i64::from(r.recorded.1),
                r.sha1.as_str(),
                i64::from(d.is_some()),
                d.map(|d| d.shape.clone()),
                d.and_then(|d| d.uid.clone()),
                d.map(|d| d.variants.join("\n")),
                d.and_then(|d| d.error.clone()),
                d.map(|d| d.text.to_string()),
            )))?;
            for (slot, uid, target, output) in d.map_or(&[][..], |d| d.references.as_slice()) {
                let row = (path.as_ref(), slot.as_str(), uid.as_str(), target.as_str(), output.as_str());
                block_on(reference.execute(row))?;
            }
        }
        Ok(written)
    }
}
