//! The store: retained outputs, kept by content in the project cache (`.cache/store`). A value is kept under its hash
//! (`values/<hash>.toml`, which says its type and holds or names its contents), its bytes as blobs by their own hashes
//! (`blobs/<aa>/<sha1>`), shared by every value holding them - an import made twice of one source is kept once. An
//! output's trace (`traces/<key>.toml`) names its value and what it read, under the key of what makes it. Like the rest
//! of the cache, any of it may be deleted at the cost of time only; a value is written before the trace naming it, so
//! a trace never names what is not there.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use toml::Table;

use super::exec::Reads;
use super::{File, Image, Imported, Value};
use crate::cache::{Cache, write_atomic};
use crate::extract::sha1_hex;

const DIR: &str = "store";

/// An output's trace: its value's hash, and what it read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    pub value: String,
    #[serde(default)]
    pub reads: Reads,
}

/// A blob as a value names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub sha1: String,
    pub size: u64,
}

/// A kept value as its `values/<hash>.toml` describes it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Kept {
    File { name: String, blob: Blob },
    Native {
        files: BTreeMap<String, Blob>,
        /// The files' order as made.
        #[serde(default)]
        order: Vec<String>,
        #[serde(default)]
        settings: Table,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    Image { width: usize, height: usize, blob: Blob },
    Table { table: Table },
}

#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The project's store.
    ///
    /// # Errors
    /// Its folder cannot be made.
    pub fn open(cache: &Cache) -> Result<Store> {
        Store::at(cache.path(DIR))
    }

    /// A store in a folder of its own.
    ///
    /// # Errors
    /// The folder cannot be made.
    pub fn at(dir: PathBuf) -> Result<Store> {
        for sub in ["values", "blobs", "traces"] {
            let path = dir.join(sub);
            std::fs::create_dir_all(&path).with_context(|| path.display().to_string())?;
        }
        Ok(Store { dir })
    }

    /// Where a blob is kept.
    #[must_use]
    pub fn blob_path(&self, sha1: &str) -> PathBuf {
        self.dir.join("blobs").join(sha1.get(..2).unwrap_or("00")).join(sha1)
    }

    fn put_blob(&self, bytes: &[u8]) -> Result<Blob> {
        let sha1 = sha1_hex(bytes);
        let path = self.blob_path(&sha1);
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
            }
            write_atomic(&path, bytes)?;
        }
        Ok(Blob { sha1, size: bytes.len() as u64 })
    }

    fn read_blob(&self, blob: &Blob) -> Result<Vec<u8>> {
        let path = self.blob_path(&blob.sha1);
        std::fs::read(&path).with_context(|| path.display().to_string())
    }

    fn value_path(&self, hash: &str) -> PathBuf {
        self.dir.join("values").join(format!("{hash}.toml"))
    }

    /// Keep a value under its hash: its blobs, then its description.
    ///
    /// # Errors
    /// It cannot be written.
    pub fn put(&self, hash: &str, value: &Value) -> Result<()> {
        let path = self.value_path(hash);
        if path.exists() {
            return Ok(());
        }
        let kept = match value {
            Value::File(file) => Kept::File { name: file.name.clone(), blob: self.put_blob(&file.bytes)? },
            Value::Native(native) => {
                let mut files = BTreeMap::new();
                for (name, bytes) in &native.files {
                    files.insert(name.clone(), self.put_blob(bytes)?);
                }
                let order = native.files.iter().map(|(name, _)| name.clone()).collect();
                Kept::Native { files, order, settings: native.settings.clone(), notes: native.notes.clone() }
            }
            Value::Image(image) => Kept::Image { width: image.width, height: image.height, blob: self.put_blob(&image.rgba)? },
            Value::Table(table) => Kept::Table { table: table.clone() },
        };
        write_atomic(&path, toml::to_string(&kept)?.as_bytes())
    }

    /// Whether a value is kept.
    #[must_use]
    pub fn has(&self, hash: &str) -> bool {
        self.value_path(hash).exists()
    }

    /// A kept value's description, without reading its blobs: a native form's files are then read where they are kept
    /// (`blob_path`).
    #[must_use]
    pub fn describe(&self, hash: &str) -> Option<Kept> {
        toml::from_str(&std::fs::read_to_string(self.value_path(hash)).ok()?).ok()
    }

    /// A kept value, read whole.
    ///
    /// # Errors
    /// It is not kept, or a blob of it cannot be read.
    pub fn get(&self, hash: &str) -> Result<Value> {
        let kept = self.describe(hash).with_context(|| format!("{hash}: not kept"))?;
        Ok(match kept {
            Kept::File { name, blob } => Value::File(File { name, bytes: self.read_blob(&blob)? }),
            Kept::Native { files, order, settings, notes } => {
                let mut made = Vec::with_capacity(files.len());
                for name in order.iter().filter(|n| files.contains_key(*n)).chain(files.keys().filter(|n| !order.contains(n))) {
                    made.push((name.clone(), self.read_blob(&files[name])?));
                }
                Value::Native(Imported { files: made, settings, notes })
            }
            Kept::Image { width, height, blob } => Value::Image(Image { width, height, rgba: self.read_blob(&blob)? }),
            Kept::Table { table } => Value::Table(table),
        })
    }

    fn trace_path(&self, key: &str) -> PathBuf {
        self.dir.join("traces").join(format!("{key}.toml"))
    }

    /// The trace kept under a key.
    #[must_use]
    pub fn trace(&self, key: &str) -> Option<Trace> {
        toml::from_str(&std::fs::read_to_string(self.trace_path(key)).ok()?).ok()
    }

    /// Keep a trace under a key.
    ///
    /// # Errors
    /// It cannot be written.
    pub fn put_trace(&self, key: &str, trace: &Trace) -> Result<()> {
        write_atomic(&self.trace_path(key), toml::to_string(trace)?.as_bytes())
    }

    /// Remove the traces not under the keys `live`, then the values no trace left names, then the blobs no value left
    /// names: what was swept, as (traces, values, blobs).
    #[must_use = "what was swept is the sweep's report"]
    pub fn sweep(&self, live: &HashSet<String>) -> (usize, usize, usize) {
        let stem = |e: &std::fs::DirEntry| e.path().file_stem().and_then(|s| s.to_str()).map(str::to_string);
        let entries = |sub: &str| std::fs::read_dir(self.dir.join(sub)).into_iter().flatten().flatten();
        let mut swept = (0, 0, 0);
        let mut values = HashSet::new();
        for entry in entries("traces") {
            let Some(key) = stem(&entry) else { continue };
            if live.contains(&key) && let Some(trace) = self.trace(&key) {
                values.insert(trace.value);
            } else if std::fs::remove_file(entry.path()).is_ok() {
                swept.0 += 1;
            }
        }
        let mut blobs = HashSet::new();
        for entry in entries("values") {
            let Some(hash) = stem(&entry) else { continue };
            if values.contains(&hash) && let Some(kept) = self.describe(&hash) {
                match kept {
                    Kept::File { blob, .. } | Kept::Image { blob, .. } => {
                        blobs.insert(blob.sha1);
                    }
                    Kept::Native { files, .. } => blobs.extend(files.into_values().map(|b| b.sha1)),
                    Kept::Table { .. } => {}
                }
            } else if std::fs::remove_file(entry.path()).is_ok() {
                swept.1 += 1;
            }
        }
        for dir in entries("blobs") {
            for entry in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
                let name = entry.file_name();
                if !name.to_str().is_some_and(|n| blobs.contains(n)) && std::fs::remove_file(entry.path()).is_ok() {
                    swept.2 += 1;
                }
            }
        }
        swept
    }
}
