//! A layer's changeset: `<layer>.toml` beside the layer's directory in `.layers/`, the layer's net effect on the layers
//! below it. `[entries]` lists what the layer creates - every asset, folder and plain file it owns, by uid; `[ops]`
//! what it does to entries from below, one record per uid: an override (its own version, with the hash of the version
//! below it was made from), a move, a delete. Nothing is implicit: an entry on disk the changeset does not list is
//! unlisted, and the index inserts a create for it where the layer can be written.
//!
//! ```toml
//! read_only = true                     # only its maker writes it (a project's nodes: its tools)
//! ignore = ["*.swp"]                   # never listed
//!
//! [entries]
//! "uid://k2..." = "stages/akropolis/rooms/plaza/props/neon_sign"
//!
//! [ops]
//! "uid://e8rx12suodf6" = { override = "sha1:..." }
//! "uid://1x9..." = { to = "stages/akropolis/rooms/plaza/views/old" }
//! "uid://7q2..." = { delete = true }
//! ```

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::Path;

use crate::cache::write_atomic;

/// What a layer does to an entry from below.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Its own version, at the entry's resolved path in the layer; `base` is the hash of the version below it was
    /// made from, and `to` a move with it.
    Override {
        base: String,
        to: Option<String>,
    },
    Move {
        to: String,
    },
    Delete,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changeset {
    pub read_only: bool,
    pub ignore: Vec<String>,
    /// What the layer creates: uid -> path.
    pub entries: BTreeMap<String, String>,
    /// What it does to entries from below, by uid.
    pub ops: BTreeMap<String, Op>,
}

fn string(value: &toml::Value, what: &str) -> Result<String> {
    value.as_str().map(str::to_string).with_context(|| format!("{what}: not a string"))
}

impl Changeset {
    /// The changeset at `path`; an empty one where there is none yet.
    ///
    /// # Errors
    /// It cannot be read, or is not a changeset.
    pub fn load(path: &Path) -> Result<Changeset> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Changeset::default()),
            Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
        };
        Changeset::parse(&text).with_context(|| format!("{}", path.display()))
    }

    /// A changeset's text.
    ///
    /// # Errors
    /// It is not TOML, or not a changeset's shape.
    pub fn parse(text: &str) -> Result<Changeset> {
        let table: toml::Table = toml::from_str(text)?;
        let mut changeset = Changeset {
            read_only: table.get("read_only").and_then(toml::Value::as_bool).unwrap_or(false),
            ..Changeset::default()
        };
        if let Some(ignore) = table.get("ignore").and_then(toml::Value::as_array) {
            changeset.ignore = ignore.iter().map(|v| string(v, "ignore")).collect::<Result<_>>()?;
        }
        for (uid, path) in table.get("entries").and_then(toml::Value::as_table).into_iter().flatten() {
            changeset.entries.insert(uid.clone(), string(path, uid)?);
        }
        for (uid, op) in table.get("ops").and_then(toml::Value::as_table).into_iter().flatten() {
            let op = op.as_table().with_context(|| format!("{uid}: not a table"))?;
            let to = op.get("to").map(|v| string(v, uid)).transpose()?;
            let op = if op.get("delete").and_then(toml::Value::as_bool) == Some(true) {
                Op::Delete
            } else if let Some(base) = op.get("override") {
                Op::Override { base: string(base, uid)?, to }
            } else if let Some(to) = to {
                Op::Move { to }
            } else {
                bail!("{uid}: an op that neither overrides, moves nor deletes");
            };
            changeset.ops.insert(uid.clone(), op);
        }
        Ok(changeset)
    }

    /// Its text: the entries sorted by path, so a diff reads as a tree.
    #[must_use]
    pub fn render(&self) -> String {
        let quote = |s: &str| toml::Value::String(s.to_string()).to_string();
        let mut lines =
            vec!["# A layer's changeset: what it creates, and what it does to the layers below.".to_string()];
        if self.read_only {
            lines.push("read_only = true".into());
        }
        if !self.ignore.is_empty() {
            lines.push(format!("ignore = [{}]", self.ignore.iter().map(|i| quote(i)).collect::<Vec<_>>().join(", ")));
        }
        lines.extend([String::new(), "[entries]".into()]);
        let mut entries: Vec<(&String, &String)> = self.entries.iter().collect();
        entries.sort_by(|a, b| a.1.cmp(b.1));
        lines.extend(entries.into_iter().map(|(uid, path)| format!("{} = {}", quote(uid), quote(path))));
        if !self.ops.is_empty() {
            lines.extend([String::new(), "[ops]".into()]);
            for (uid, op) in &self.ops {
                let fields = match op {
                    Op::Override { base, to: None } => format!("override = {}", quote(base)),
                    Op::Override { base, to: Some(to) } => format!("override = {}, to = {}", quote(base), quote(to)),
                    Op::Move { to } => format!("to = {}", quote(to)),
                    Op::Delete => "delete = true".into(),
                };
                lines.push(format!("{} = {{ {fields} }}", quote(uid)));
            }
        }
        lines.join("\n") + "\n"
    }

    /// Write it whole.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, self.render().as_bytes())
    }

    /// Whether `ignore` leaves out a path relative to the layer (a pattern matches a name in it; `*` stands for any
    /// run of characters within it).
    #[must_use]
    pub fn ignores(&self, rel: &str) -> bool {
        ignored(&self.ignore, rel)
    }
}

/// Whether patterns leave out a path: a pattern matches a name in it, `*` standing for any run of characters within
/// it.
#[must_use]
pub fn ignored(patterns: &[String], rel: &str) -> bool {
    rel.split('/').any(|name| patterns.iter().any(|pattern| glob(pattern, name)))
}

/// The name patterns of a `.gitignore`: those matching a name anywhere (`*.o`, `__pycache__/`). A pattern anchored to
/// a path or negated is not one, and is left out.
#[must_use]
pub fn gitignore_names(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .map(|l| l.strip_suffix('/').unwrap_or(l))
        .filter(|l| !l.contains('/'))
        .map(str::to_string)
        .collect()
}

/// A name matched against a pattern where `*` is any run of characters.
fn glob(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => {
            name.starts_with(head) && (0..=name.len() - head.len()).any(|skip| glob(tail, &name[head.len() + skip..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Changeset, Op};

    #[test]
    fn round_trip() {
        let mut changeset = Changeset { read_only: true, ignore: vec!["*.swp".into()], ..Changeset::default() };
        changeset.entries.insert("uid://b".into(), "z/b".into());
        changeset.entries.insert("uid://a".into(), "y/a".into());
        changeset.ops.insert("uid://o".into(), Op::Override { base: "sha1:1".into(), to: Some("w".into()) });
        changeset.ops.insert("uid://m".into(), Op::Move { to: "v".into() });
        changeset.ops.insert("uid://d".into(), Op::Delete);
        let text = changeset.render();
        assert!(text.find("y/a").unwrap() < text.find("z/b").unwrap());
        assert_eq!(Changeset::parse(&text).unwrap(), changeset);
        assert!(changeset.ignores("a/.x.swp") && !changeset.ignores("a/x.swap"));
    }
}
