//! The store: retained outputs, kept by content in the project cache (`.cache/store`). A value is kept under its hash
//! (`values/<hash>.toml`, which says its type and holds or names its contents), its bytes as blobs by their own hashes
//! (`blobs/<aa>/<sha1>`), shared by every value holding them - an import made twice of one source is kept once. An
//! output's trace (`traces/<key>.toml`) names its value and what it read, under the key of what makes it. Like the rest
//! of the cache, any of it may be deleted at the cost of time only; a value is written before the trace naming it, so
//! a trace never names what is not there.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use toml::Table;

use super::exec::Reads;
use super::{Content, File, Image, Pcm, Typed, Value};
use crate::cache::{Cache, write_atomic};
use crate::extract::sha1_hex;

const DIR: &str = "store";

/// An output's trace: its value's hash, what it read, and whose output it is (its node, as traces name it, and its
/// address).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    pub value: String,
    #[serde(default)]
    pub reads: Reads,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub output: String,
}

/// A blob as a value names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub sha1: String,
    pub size: u64,
}

/// A kept file: its name, its content's blob.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptFile {
    pub name: String,
    pub blob: Blob,
}

/// A kept value as its `values/<hash>.toml` describes it: contents as blobs, read back by reference.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Kept {
    Typed {
        ty: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        also: Vec<String>,
        files: Vec<KeptFile>,
        #[serde(default)]
        settings: Table,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    Image {
        width: usize,
        height: usize,
        blob: Blob,
    },
    Table {
        table: Table,
    },
    Pcm {
        rate: u32,
        channels: u16,
        blob: Blob,
    },
    Video {
        format: String,
        width: u32,
        height: u32,
        timestamp: u64,
        blob: Blob,
    },
    Audio {
        format: String,
        sample_rate: u32,
        channels: u32,
        timestamp: u64,
        blob: Blob,
    },
    Marks {
        boxes: Vec<KeptMark>,
        summary: String,
        toggle: String,
        legend: bool,
    },
    /// Its items, each kept by its own hash.
    List {
        items: Vec<String>,
    },
}

/// A mark as kept.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeptMark {
    name: String,
    rect: [f32; 4],
}

impl KeptMark {
    fn of(mark: &pe2_ext::Mark) -> KeptMark {
        let r = mark.rect;
        KeptMark { name: mark.name.clone(), rect: [r.x, r.y, r.width, r.height] }
    }

    fn into_mark(self) -> pe2_ext::Mark {
        let [x, y, width, height] = self.rect;
        pe2_ext::Mark { name: self.name, rect: pe2_ext::Rect { x, y, width, height } }
    }
}

impl Kept {
    /// The blobs it names.
    fn blobs(&self) -> Vec<&Blob> {
        match self {
            Kept::Typed { files, .. } => files.iter().map(|f| &f.blob).collect(),
            Kept::Image { blob, .. } | Kept::Pcm { blob, .. } | Kept::Video { blob, .. } | Kept::Audio { blob, .. } => {
                vec![blob]
            }
            Kept::Table { .. } | Kept::Marks { .. } | Kept::List { .. } => vec![],
        }
    }
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

    /// A file to write a content too large to hold into: `tmp/` in the store, adopted as a blob once written.
    ///
    /// # Errors
    /// It cannot be made.
    pub fn spill(&self) -> Result<(std::fs::File, PathBuf)> {
        static SPILLED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = self.dir.join("tmp");
        std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
        let n = SPILLED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(format!("{}-{n}", std::process::id()));
        let file = std::fs::File::create(&path).with_context(|| path.display().to_string())?;
        Ok((file, path))
    }

    /// Adopt a written file as the blob of its hash: moved where blobs are kept (dropped where it is kept already).
    ///
    /// # Errors
    /// It cannot be moved.
    pub fn adopt(&self, written: &std::path::Path, sha1: &str) -> Result<()> {
        let path = self.blob_path(sha1);
        if path.exists() {
            let _ = std::fs::remove_file(written);
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
        }
        // Moved; or, made on another file system, copied.
        if std::fs::rename(written, &path).is_err() {
            std::fs::copy(written, &path).with_context(|| path.display().to_string())?;
            let _ = std::fs::remove_file(written);
        }
        Ok(())
    }

    /// Keep bytes as a blob, where a program can read them (`blob_path`).
    ///
    /// # Errors
    /// They cannot be written.
    pub fn keep(&self, bytes: &[u8]) -> Result<Blob> {
        self.put_blob(bytes)
    }

    fn read_blob(&self, blob: &Blob) -> Result<Vec<u8>> {
        let path = self.blob_path(&blob.sha1);
        std::fs::read(&path).with_context(|| path.display().to_string())
    }

    fn value_path(&self, hash: &str) -> PathBuf {
        self.dir.join("values").join(format!("{hash}.toml"))
    }

    /// Keep a content as a blob: inline, written; stored elsewhere (`locate` reads it, by its hash), copied.
    fn put_content(&self, content: &Content, locate: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<Blob> {
        match content {
            Content::Inline(bytes) => self.put_blob(bytes),
            Content::Stored { sha1, size } => {
                let path = self.blob_path(sha1);
                if !path.exists() {
                    let bytes = locate(sha1).with_context(|| format!("no content {sha1} to keep"))?;
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
                    }
                    write_atomic(&path, &bytes)?;
                }
                Ok(Blob { sha1: sha1.clone(), size: *size })
            }
        }
    }

    fn put_file(&self, file: &File, locate: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<KeptFile> {
        Ok(KeptFile { name: file.name.clone(), blob: self.put_content(&file.content, locate)? })
    }

    /// Keep a value under its hash: its contents as blobs (what is stored elsewhere read by `locate`), then its
    /// description.
    ///
    /// # Errors
    /// It cannot be written, or a content of it found.
    pub fn put(&self, hash: &str, value: &Value, locate: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<()> {
        // Kept already, as this store reads values (one written otherwise is written again).
        if self.describe(hash).is_some() {
            return Ok(());
        }
        let path = self.value_path(hash);
        let kept = match value {
            Value::Typed(typed) => Kept::Typed {
                ty: typed.ty.clone(),
                also: typed.also.clone(),
                files: typed.files.iter().map(|f| self.put_file(f, locate)).collect::<Result<_>>()?,
                settings: typed.settings.clone(),
                notes: typed.notes.clone(),
            },
            Value::Image(image) => {
                Kept::Image { width: image.width, height: image.height, blob: self.put_blob(&image.rgba)? }
            }
            Value::Table(table) => Kept::Table { table: table.clone() },
            Value::Pcm(pcm) => {
                let bytes: Vec<u8> = pcm.samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                Kept::Pcm { rate: pcm.rate, channels: pcm.channels, blob: self.put_blob(&bytes)? }
            }
            Value::Video(frame) => Kept::Video {
                format: frame.format.name().to_string(),
                width: frame.width,
                height: frame.height,
                timestamp: frame.timestamp,
                blob: self.put_blob(frame.buffer.bytes())?,
            },
            Value::Audio(sound) => Kept::Audio {
                format: sound.format.name().to_string(),
                sample_rate: sound.sample_rate,
                channels: sound.channels,
                timestamp: sound.timestamp,
                blob: self.put_blob(sound.buffer.bytes())?,
            },
            Value::Marks(marks) => Kept::Marks {
                boxes: marks.boxes.iter().map(KeptMark::of).collect(),
                summary: marks.summary.clone(),
                toggle: marks.toggle.clone(),
                legend: marks.legend,
            },
            Value::Stream(_) => anyhow::bail!("a stream is read, not kept"),
            Value::List(items) => {
                let mut hashes = Vec::with_capacity(items.len());
                for item in items {
                    let hash = super::hash(item);
                    self.put(&hash, item, locate)?;
                    hashes.push(hash);
                }
                Kept::List { items: hashes }
            }
        };
        write_atomic(&path, toml::to_string(&kept)?.as_bytes())
    }

    /// Whether a value is kept. (One this store cannot read - written otherwise - is found when it is read, and made
    /// again.)
    #[must_use]
    pub fn has(&self, hash: &str) -> bool {
        self.value_path(hash).exists()
    }

    /// A kept value's description, without reading its blobs.
    #[must_use]
    pub fn describe(&self, hash: &str) -> Option<Kept> {
        toml::from_str(&std::fs::read_to_string(self.value_path(hash)).ok()?).ok()
    }

    /// A kept value: its files' contents by reference (read through `blob_path`), what is not a file's read now. One
    /// whose contents are not all kept is not.
    ///
    /// # Errors
    /// It is not kept, or a blob of it cannot be read.
    pub fn get(&self, hash: &str) -> Result<Value> {
        let kept = self.describe(hash).with_context(|| format!("{hash}: not kept"))?;
        let file =
            |f: KeptFile| File { name: f.name, content: Content::Stored { sha1: f.blob.sha1, size: f.blob.size } };
        Ok(match kept {
            Kept::Typed { ty, also, files, settings, notes } => {
                // Its contents by reference: each still kept (none removed but by the sweep, with the value).
                if let Some(gone) = files.iter().find(|f| !self.blob_path(&f.blob.sha1).exists()) {
                    anyhow::bail!("{hash}: its {} is no longer kept", gone.name);
                }
                Value::Typed(Typed { ty, also, files: files.into_iter().map(file).collect(), settings, notes })
            }
            Kept::Image { width, height, blob } => Value::Image(Image { width, height, rgba: self.read_blob(&blob)? }),
            Kept::Video { format, width, height, timestamp, blob } => Value::Video(pe2_ext::media::VideoFrame {
                format: pe2_ext::media::PixelFormat::named(&format)
                    .with_context(|| format!("{hash}: no layout {format}"))?,
                buffer: pe2_ext::media::MediaBuffer::new(self.read_blob(&blob)?),
                width,
                height,
                timestamp,
            }),
            Kept::Audio { format, sample_rate, channels, timestamp, blob } => Value::Audio(pe2_ext::media::AudioData {
                format: pe2_ext::media::SampleFormat::named(&format)
                    .with_context(|| format!("{hash}: no layout {format}"))?,
                buffer: pe2_ext::media::MediaBuffer::new(self.read_blob(&blob)?),
                sample_rate,
                channels,
                timestamp,
            }),
            Kept::Table { table } => Value::Table(table),
            Kept::Marks { boxes, summary, toggle, legend } => Value::Marks(pe2_ext::Marks {
                boxes: boxes.into_iter().map(KeptMark::into_mark).collect(),
                summary,
                toggle,
                legend,
            }),
            Kept::Pcm { rate, channels, blob } => {
                let bytes = self.read_blob(&blob)?;
                let samples = bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
                Value::Pcm(Pcm { rate, channels, samples })
            }
            Kept::List { items } => Value::List(items.iter().map(|h| self.get(h)).collect::<Result<_>>()?),
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

    /// Forget a trace: what it was the trace of is made again when next asked for, though its value stays.
    pub fn forget_trace(&self, key: &str) {
        let _ = std::fs::remove_file(self.trace_path(key));
    }

    /// Remove the traces of nodes not `live`, and of each live node's output all but the newest (what made it
    /// before) - never those under the keys `used`, which what swept them reads - then the values no trace left
    /// names, then the blobs no value left names: what was swept, as (traces, values, blobs). It may run beside what
    /// keeps outputs: what was written since a little before it began is left (a value's blobs are written before it,
    /// it before its trace).
    #[must_use = "what was swept is the sweep's report"]
    pub fn sweep(&self, live: &HashSet<String>, used: &HashSet<&String>) -> (usize, usize, usize) {
        self.sweep_older(live, used, std::time::SystemTime::now() - std::time::Duration::from_secs(2))
    }

    /// Sweep as `sweep` does what was written before `began`.
    #[must_use = "what was swept is the sweep's report"]
    pub fn sweep_older(
        &self,
        live: &HashSet<String>,
        used: &HashSet<&String>,
        began: std::time::SystemTime,
    ) -> (usize, usize, usize) {
        let stem = |e: &std::fs::DirEntry| e.path().file_stem().and_then(|s| s.to_str()).map(str::to_string);
        let young = |e: &std::fs::DirEntry| e.metadata().and_then(|m| m.modified()).is_ok_and(|m| m >= began);
        let entries =
            |sub: &str| std::fs::read_dir(self.dir.join(sub)).into_iter().flatten().flatten().filter(|e| !young(e));
        let mut swept = (0, 0, 0);
        // Each live output's newest trace, by when it was written.
        let mut newest: HashMap<(String, String), (std::time::SystemTime, String, String)> = HashMap::new();
        let mut gone = Vec::new();
        for entry in entries("traces") {
            let (Some(key), Some(trace)) = (stem(&entry), stem(&entry).and_then(|k| self.trace(&k))) else {
                gone.push(entry.path());
                continue;
            };
            // A trace in use is the newest there is.
            let written = if used.contains(&key) {
                std::time::SystemTime::now() + std::time::Duration::from_secs(1)
            } else {
                entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH)
            };
            if !live.contains(&trace.node) && !used.contains(&key) {
                gone.push(entry.path());
                continue;
            }
            match newest.entry((trace.node, trace.output)) {
                std::collections::hash_map::Entry::Occupied(mut held) => {
                    if held.get().0 < written {
                        let (_, older, _) = held.insert((written, key, trace.value));
                        gone.push(self.trace_path(&older));
                    } else {
                        gone.push(entry.path());
                    }
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert((written, key, trace.value));
                }
            }
        }
        for path in gone {
            if std::fs::remove_file(path).is_ok() {
                swept.0 += 1;
            }
        }
        let mut values: HashSet<String> = newest.into_values().map(|(_, _, value)| value).collect();
        // A kept list keeps its items.
        let mut lists: Vec<String> = values.iter().cloned().collect();
        while let Some(hash) = lists.pop() {
            if let Some(Kept::List { items }) = self.describe(&hash) {
                lists.extend(items.iter().filter(|i| values.insert((*i).clone())).cloned());
            }
        }
        let mut blobs = HashSet::new();
        for entry in entries("values") {
            let Some(hash) = stem(&entry) else { continue };
            if values.contains(&hash)
                && let Some(kept) = self.describe(&hash)
            {
                blobs.extend(kept.blobs().into_iter().map(|b| b.sha1.clone()));
            } else if std::fs::remove_file(entry.path()).is_ok() {
                swept.1 += 1;
            }
        }
        for dir in entries("blobs") {
            for entry in std::fs::read_dir(dir.path()).into_iter().flatten().flatten().filter(|e| !young(e)) {
                let name = entry.file_name();
                if !name.to_str().is_some_and(|n| blobs.contains(n)) && std::fs::remove_file(entry.path()).is_ok() {
                    swept.2 += 1;
                }
            }
        }
        swept
    }
}
