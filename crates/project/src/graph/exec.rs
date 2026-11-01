//! Evaluating a node's outputs: only the operations they reach run, in the plan's order, each result moved to what
//! takes it where one thing does and shared, read-only, until its last taker where several do. What the operations
//! read of their node - its files, its declaration - is recorded as they read it: the outputs' trace, by which a kept
//! output is known still to hold without making it again.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use toml::Table;

use sha1::{Digest, Sha1};

use super::store::{Store, Trace};
use super::{Content, File, Ops, Plan, Retain, Typed, Value};

/// A node as its operations read it: an asset - its declaration and its bundle's stored files.
pub trait Node: Sync {
    fn path(&self) -> &str;
    fn uid(&self) -> Option<&str>;
    /// Its declaration, as written.
    fn declaration(&self) -> &str;
    /// Its declaration's fields.
    fn fields(&self) -> &Table;
    /// Its declaration's hash: what an operation reading its settings depends on.
    fn declaration_sha1(&self) -> String {
        crate::extract::sha1_hex(self.declaration().as_bytes())
    }
    /// A stored file's hash; none where its bundle has no such file.
    fn file_sha1(&self, name: &str) -> Option<String>;
    /// A stored file, whole.
    ///
    /// # Errors
    /// Its bundle has no such file, or it cannot be read.
    fn read(&self, name: &str) -> Result<Vec<u8>>;
    /// A stored file's size.
    fn file_size(&self, name: &str) -> Option<u64> {
        self.read(name).ok().map(|b| b.len() as u64)
    }
    /// A stored file's first bytes, up to `len`: what its formats are recognized by.
    fn read_head(&self, name: &str, len: usize) -> Vec<u8> {
        self.read(name).map_or_else(
            |_| Vec::new(),
            |mut b| {
                b.truncate(len);
                b
            },
        )
    }
    /// Where a content of the node is, by its hash: a stored file's, or what its outputs made.
    fn content_path(&self, sha1: &str) -> Option<std::path::PathBuf> {
        let _ = sha1;
        None
    }
    /// A content of the node, by its hash.
    ///
    /// # Errors
    /// It has no such content, or it cannot be read.
    fn read_content(&self, sha1: &str) -> Result<Vec<u8>> {
        let path = self.content_path(sha1).with_context(|| format!("{}: no content {sha1}", self.path()))?;
        std::fs::read(&path).with_context(|| path.display().to_string())
    }
}

/// An asset of the index is a node: its stored files those of its bundle, not what imports made; its declaration as last
/// applied (`applied`).
impl Node for crate::index::Asset {
    fn path(&self) -> &str {
        &self.path
    }

    fn uid(&self) -> Option<&str> {
        self.uid.as_deref()
    }

    fn declaration(&self) -> &str {
        self.applied.as_ref().map_or_else(|| self.text(), |a| a.text.as_str())
    }

    fn fields(&self) -> &Table {
        self.applied.as_ref().map_or_else(|| crate::index::Asset::fields(self), |a| &a.fields)
    }

    fn file_sha1(&self, name: &str) -> Option<String> {
        self.files.iter().find(|f| f.name == name && f.made_by.is_none()).map(|f| f.sha1.clone())
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        let file = self.files.iter().find(|f| f.name == name && f.made_by.is_none());
        let file = file.with_context(|| format!("{}: no file {name}", self.path))?;
        std::fs::read(&file.path).with_context(|| file.path.display().to_string())
    }

    fn file_size(&self, name: &str) -> Option<u64> {
        self.files.iter().find(|f| f.name == name && f.made_by.is_none()).map(|f| f.size)
    }

    fn content_path(&self, sha1: &str) -> Option<std::path::PathBuf> {
        self.files.iter().find(|f| f.sha1 == sha1).map(|f| f.path.clone())
    }

    fn read_head(&self, name: &str, len: usize) -> Vec<u8> {
        use std::io::Read;
        let Some(file) = self.files.iter().find(|f| f.name == name && f.made_by.is_none()) else { return Vec::new() };
        let mut head = Vec::new();
        if let Ok(f) = std::fs::File::open(&file.path) {
            let _ = f.take(len as u64).read_to_end(&mut head);
        }
        head
    }

}

/// What operations read of their node: its stored files by their hashes (empty where it had none of the name), its
/// declaration's hash where they read its settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reads {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<String>,
    /// Other nodes' outputs read, each by `<node>#<address>` (its node as traces name it), with its hash.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    /// The project's plain files read, each by its path, with its hash; a folder listed, by its path and a `/`, with its list's.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, String>,
    /// The config it was made for, where it read which.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
}

impl Reads {
    /// Add what another read: an output made from a kept one read what that one did.
    pub fn merge(&mut self, other: &Reads) {
        self.files.extend(other.files.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.outputs.extend(other.outputs.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.entries.extend(other.entries.iter().map(|(k, v)| (k.clone(), v.clone())));
        if other.declaration.is_some() {
            self.declaration.clone_from(&other.declaration);
        }
        if other.config.is_some() {
            self.config.clone_from(&other.config);
        }
    }

    /// Whether the node still reads the same: each file read has its hash, the declaration its, and each other node's
    /// output read its - made, where it must be, in `reach` (without which none can be told).
    /// Each of the project's files and folders read has its too.
    #[must_use]
    pub fn hold(&self, node: &dyn Node, reach: Option<Reach<'_>>) -> bool {
        self.files.iter().all(|(name, sha1)| node.file_sha1(name).unwrap_or_default() == *sha1)
            && self.declaration.as_ref().is_none_or(|d| *d == node.declaration_sha1())
            && (self.outputs.is_empty()
                || reach.is_some_and(|reach| self.outputs.iter().all(|(read, hash)| reach.hash(read).is_some_and(|h| h == *hash))))
            && (self.entries.is_empty()
                || reach.is_some_and(|reach| self.entries.iter().all(|(read, hash)| reach.entry_hash(read).as_deref() == Some(hash))))
    }
}

/// The other nodes a node's operations may read the outputs of: each by a reference (TOML: `"uid://..."`, `{ uid,
/// path }`, or a path), with its plan.
pub trait World: Sync {
    fn node(&self, reference: &toml::Value) -> Option<(&dyn Node, Plan)>;
    /// The node a reference names, as what reads it is told.
    fn named(&self, reference: &toml::Value) -> Option<pe2_ext::NodeNamed> {
        let (node, _) = self.node(reference)?;
        Some(pe2_ext::NodeNamed { path: node.path().to_string(), uid: node.uid().map(str::to_string), types: Vec::new() })
    }
    /// The plain files under a folder of the project, by their paths, in order.
    fn entries(&self, folder: &str) -> Vec<String> {
        let _ = folder;
        vec![]
    }
    /// Where a plain file of the project is on disk.
    fn entry(&self, path: &str) -> Option<std::path::PathBuf> {
        let _ = path;
        None
    }

    /// Where a file or folder is on disk, followed through links.
    fn place(&self, path: &str) -> Option<std::path::PathBuf> {
        let _ = path;
        None
    }
    /// The assets of a type, each by its path with its declaration as written.
    fn assets_of(&self, node_type: &str) -> Vec<(String, String)> {
        let _ = node_type;
        vec![]
    }
}

/// How a trace names a type's assets, as no path of the project can begin.
const TYPE_READ: &str = "type:";

/// The hash a type's assets have together, what tells one changed, come or gone.
fn type_hash(assets: &[(String, String)]) -> String {
    let joined: Vec<String> = assets.iter().map(|(path, text)| format!("{path}\n{text}")).collect();
    list_hash(&joined)
}

/// The hash a folder's list has, what tells a file added or gone.
fn list_hash(files: &[String]) -> String {
    crate::extract::sha1_hex(files.join("\n").as_bytes())
}

/// What evaluation reaches beyond its node: the engine making other nodes' outputs, in a world of them.
#[derive(Clone, Copy)]
pub struct Reach<'a> {
    pub engine: &'a Engine,
    pub world: &'a dyn World,
}

thread_local! {
    /// The outputs being made of other nodes on this thread, outermost first: one asked again is a cycle.
    static MAKING: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl<'a> Reach<'a> {
    /// Another node's output, by its reference and its address: made, or found kept, in the world; with its hash.
    ///
    /// # Errors
    /// No node is so named, it has no such output, it reads, through others, itself, or it cannot be made.
    pub fn output(&self, reference: &toml::Value, address: &str) -> Result<(String, Given, &'a dyn Node)> {
        let (node, plan) = self.world.node(reference).with_context(|| format!("{reference}: no such node"))?;
        let read = format!("{}#{address}", id(node));
        let cycle = MAKING.with(|m| m.borrow().contains(&read));
        anyhow::ensure!(!cycle, "{read} reads, through others, itself");
        MAKING.with(|m| m.borrow_mut().push(read.clone()));
        let given = self.engine.output_in(Some(*self), &plan, node, address, None, true);
        MAKING.with(|m| m.borrow_mut().pop());
        Ok((read, given?, node))
    }

    /// The hash a project's file or folder has now, by how a trace names it (a folder's with a `/`).
    fn entry_hash(&self, read: &str) -> Option<String> {
        if let Some(node_type) = read.strip_prefix(TYPE_READ) {
            return Some(type_hash(&self.world.assets_of(node_type)));
        }
        match read.strip_suffix('/') {
            Some(folder) => Some(list_hash(&self.world.entries(folder))),
            None => std::fs::read(self.world.entry(read)?).ok().map(|bytes| crate::extract::sha1_hex(&bytes)),
        }
    }

    /// The hash another node's output has now, by how a trace names it (`<node>#<address>`).
    fn hash(&self, read: &str) -> Option<String> {
        let (node, address) = read.rsplit_once('#')?;
        self.output(&toml::Value::String(node.to_string()), address).ok().map(|(_, given, _)| given.hash)
    }
}

/// Records what is read of a node: given to what reads it on an operation's behalf.
#[derive(Clone, Copy)]
pub struct Recorder<'a> {
    node: &'a dyn Node,
    reads: &'a Mutex<Reads>,
}

impl<'a> Recorder<'a> {
    pub fn file(&self, name: &str) {
        let sha1 = self.node.file_sha1(name).unwrap_or_default();
        self.reads.lock().unwrap_or_else(std::sync::PoisonError::into_inner).files.insert(name.to_string(), sha1);
    }

    pub fn declaration(&self) {
        let sha1 = self.node.declaration_sha1();
        self.reads.lock().unwrap_or_else(std::sync::PoisonError::into_inner).declaration = Some(sha1);
    }


    #[must_use]
    pub fn node(&self) -> &'a dyn Node {
        self.node
    }
}

/// A value given to an operation: its own where nothing else takes it, else shared until the last taker.
#[derive(Clone, Debug)]
pub struct Input(Arc<Value>);

impl Input {
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.0
    }

    /// Whether something else still holds it: taking it would copy it.
    #[must_use]
    pub fn is_shared(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }

    /// The value as the operation's own: moved where nothing else holds it, copied where something still does.
    #[must_use]
    pub fn take(self) -> Value {
        Arc::try_unwrap(self.0).unwrap_or_else(|shared| (*shared).clone())
    }
}

/// An operation running: what it is given, its settings, its output's parameter, its node, and the store its
/// contents may be in.
pub struct Call<'a> {
    inputs: Vec<Vec<Input>>,
    settings: &'a Table,
    parameter: Option<&'a str>,
    recorder: Recorder<'a>,
    store: Option<&'a Store>,
    reach: Option<Reach<'a>>,
    /// Where the stored contents of other nodes' outputs read in its evaluation are.
    others: &'a Others,
}

/// Where stored contents of other nodes' outputs are, by hash.
type Others = Mutex<HashMap<String, std::path::PathBuf>>;

impl<'a> Call<'a> {
    /// The value given to a port taking one.
    ///
    /// # Errors
    /// The port was given none.
    pub fn take(&mut self, port: usize) -> Result<Input> {
        self.inputs.get_mut(port).and_then(Vec::pop).context("given nothing")
    }

    /// Every port's values as the operation's own: moved where nothing else holds them.
    pub fn take_inputs(&mut self) -> Vec<Vec<Value>> {
        std::mem::take(&mut self.inputs).into_iter().map(|port| port.into_iter().map(Input::take).collect()).collect()
    }

    /// The values given to a port taking any number, in order.
    pub fn take_all(&mut self, port: usize) -> Vec<Input> {
        self.inputs.get_mut(port).map_or_else(Vec::new, std::mem::take)
    }

    #[must_use]
    pub fn settings(&self) -> &'a Table {
        self.settings
    }

    /// A setting that is text.
    ///
    /// # Errors
    /// It has no such setting, or it is not text.
    pub fn setting_str(&self, key: &str) -> Result<&'a str> {
        self.settings.get(key).and_then(toml::Value::as_str).with_context(|| format!("its `{key}` is not set"))
    }

    /// The value of its output's parameter, where it makes one of a family.
    #[must_use]
    pub fn parameter(&self) -> Option<&'a str> {
        self.parameter
    }

    /// A stored file of its node as a value of the types it is recognized as (`names`: the first its type, the
    /// others what else it answers to; none: `bytes`) - by reference, its content read when asked - recorded as read.
    ///
    /// # Errors
    /// The node has no such file.
    pub fn stored(&self, name: &str, mut names: Vec<String>) -> Result<Typed> {
        self.recorder.file(name);
        let node = self.recorder.node;
        let sha1 = node.file_sha1(name).with_context(|| format!("{}: no file {name}", node.path()))?;
        let size = node.file_size(name).unwrap_or_default();
        let ty = if names.is_empty() { pe2_ext::data::BYTES.to_string() } else { names.remove(0) };
        let file = File { name: name.to_string(), content: Content::Stored { sha1, size } };
        // A stored file comes with what is said of it, on its layer.
        Ok(Typed { ty, also: names, files: vec![file], settings: super::set_on(super::FILE, self.settings), ..Typed::default() })
    }

    /// Another node's output, by a reference to it and its address, recorded as read.
    ///
    /// # Errors
    /// Nothing beyond its node is reachable here, or the output cannot be made.
    pub fn output_of(&self, reference: &toml::Value, address: &str) -> Result<Arc<Value>> {
        let reach = self.reach.context("no other node is reachable here")?;
        let (read, given, node) = reach.output(reference, address)?;
        let value = given.value.with_context(|| format!("{read}: made nothing"))?;
        // Its stored contents are found where its node has them, or - its node having them nowhere a path names -
        // kept in the store.
        let mut stored = Vec::new();
        stored_contents(&value, &mut stored);
        let mut others = self.others.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for sha1 in stored {
            let path = match (reach.engine.store(), node.content_path(&sha1)) {
                (_, Some(path)) => path,
                // What a node's own output made is in no file of its: the store has it already.
                (Some(store), None) if store.blob_path(&sha1).exists() => store.blob_path(&sha1),
                (Some(store), None) => store.blob_path(&store.keep(&node.read_content(&sha1)?)?.sha1),
                (None, None) => continue,
            };
            others.insert(sha1, path);
        }
        self.recorder.reads.lock().unwrap_or_else(std::sync::PoisonError::into_inner).outputs.insert(read, given.hash);
        Ok(value)
    }

    /// The plain files under a folder of the project, recorded as read.
    ///
    /// # Errors
    /// Nothing beyond its node is reachable here.
    pub fn entries(&self, folder: &str) -> Result<Vec<String>> {
        let files = self.reach.context("the project is not reachable here")?.world.entries(folder);
        let read = format!("{}/", folder.trim_end_matches('/'));
        self.recorder.reads.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entries.insert(read, list_hash(&files));
        Ok(files)
    }

    /// A plain file of the project, recorded as read.
    ///
    /// # Errors
    /// Nothing beyond its node is reachable here, the project holds no such file, or it cannot be read.
    pub fn read_entry(&self, path: &str) -> Result<Vec<u8>> {
        let disk = self.reach.context("the project is not reachable here")?.world.entry(path).with_context(|| format!("{path}: no such file"))?;
        let bytes = std::fs::read(&disk).with_context(|| disk.display().to_string())?;
        let hash = crate::extract::sha1_hex(&bytes);
        self.recorder.reads.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entries.insert(path.to_string(), hash);
        Ok(bytes)
    }

    /// The assets of a type, each by its path with its declaration as written, recorded as read.
    ///
    /// # Errors
    /// Nothing beyond its node is reachable here.
    pub fn assets_of(&self, node_type: &str) -> Result<Vec<(String, String)>> {
        let assets = self.reach.context("the project is not reachable here")?.world.assets_of(node_type);
        let read = format!("{TYPE_READ}{node_type}");
        self.recorder.reads.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entries.insert(read, type_hash(&assets));
        Ok(assets)
    }

    /// The node a reference names.
    #[must_use]
    pub fn node_named(&self, reference: &toml::Value) -> Option<pe2_ext::NodeNamed> {
        self.reach?.world.named(reference)
    }

    /// Where a file or folder of the project is on disk.
    #[must_use]
    pub fn place(&self, path: &str) -> Option<String> {
        Some(self.reach?.world.place(path)?.display().to_string())
    }

    /// The config being built, recorded as read.
    #[must_use]
    pub fn config(&self) -> Option<pe2_ext::data::BuildConfig> {
        let config = self.reach?.engine.config.clone()?;
        self.recorder.reads.lock().unwrap_or_else(std::sync::PoisonError::into_inner).config = Some(config.name.clone());
        Some(config)
    }

    /// Its node's settings, recorded as read (its declaration as a whole).
    #[must_use]
    pub fn fields(&self) -> &'a Table {
        self.recorder.declaration();
        self.recorder.node.fields()
    }

    /// A file being made, named `name`: held in memory while small, spilled to the store past `SPILL` bytes (where
    /// there is a store), and finished into a file whose content is inline or stored - which readers cannot tell.
    #[must_use]
    pub fn writer(&self, name: &str) -> Writer {
        Writer { name: name.to_string(), store: self.store.cloned(), memory: Vec::new(), spilled: None, hasher: Sha1::new(), size: 0 }
    }

    /// A path in the store for a program to write to.
    ///
    /// # Errors
    /// There is no store, or it cannot be made.
    pub fn temp(&self) -> Result<std::path::PathBuf> {
        Ok(self.store.context("no store to keep it in")?.spill()?.1)
    }

    /// A file made kept as a content: moved to its blob, named `name`.
    ///
    /// # Errors
    /// There is no store, or it cannot be read or moved.
    pub fn keep(&self, path: &std::path::Path, name: &str) -> Result<File> {
        let store = self.store.context("no store to keep it in")?;
        let sha1 = crate::extract::sha1_file(path).with_context(|| path.display().to_string())?;
        let size = std::fs::metadata(path).with_context(|| path.display().to_string())?.len();
        store.adopt(path, &sha1)?;
        Ok(File { name: name.to_string(), content: Content::Stored { sha1, size } })
    }

    /// Copy a content into a writer, a range at a time: never read whole.
    ///
    /// # Errors
    /// It cannot be read, or written.
    pub fn copy(&self, content: &Content, into: &mut Writer) -> Result<()> {
        use std::io::Write as _;
        const RANGE: usize = 4 << 20;
        let size = match content {
            Content::Inline(bytes) => return into.write_all(bytes).map_err(Into::into),
            Content::Stored { size, .. } => *size,
        };
        let mut at = 0;
        while at < size {
            let bytes = self.read_range(content, at, RANGE)?;
            anyhow::ensure!(!bytes.is_empty(), "a content ends at {at} of its {size} bytes");
            into.write_all(&bytes)?;
            at += bytes.len() as u64;
        }
        Ok(())
    }

    /// Where a stored content is: in the store, or among its node's files.
    #[must_use]
    pub fn content_path(&self, sha1: &str) -> Option<std::path::PathBuf> {
        let other = || self.others.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(sha1).cloned();
        self.store
            .map(|s| s.blob_path(sha1))
            .filter(|p| p.exists())
            .or_else(other)
            .or_else(|| self.recorder.node.content_path(sha1))
    }

    /// A content, whole.
    ///
    /// # Errors
    /// It is stored where it cannot be found or read.
    pub fn read(&self, content: &Content) -> Result<Vec<u8>> {
        match content {
            Content::Inline(bytes) => Ok(bytes.clone()),
            Content::Stored { sha1, .. } => match self.content_path(sha1) {
                Some(path) => std::fs::read(&path).with_context(|| path.display().to_string()),
                None => self.recorder.node.read_content(sha1),
            },
        }
    }

    /// Up to `len` bytes of a content from `offset`.
    ///
    /// # Errors
    /// As `read`.
    pub fn read_range(&self, content: &Content, offset: u64, len: usize) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        match content {
            Content::Inline(bytes) => {
                let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
                Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec())
            }
            Content::Stored { sha1, .. } => {
                let Some(path) = self.content_path(sha1) else {
                    let bytes = self.recorder.node.read_content(sha1)?;
                    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
                    return Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec());
                };
                let mut file = std::fs::File::open(&path).with_context(|| path.display().to_string())?;
                file.seek(SeekFrom::Start(offset))?;
                let mut out = Vec::new();
                file.take(len as u64).read_to_end(&mut out)?;
                Ok(out)
            }
        }
    }

    /// What records its reads, for reading its node as a whole.
    #[must_use]
    pub fn recorder(&self) -> Recorder<'a> {
        self.recorder
    }
}

/// A writer as an extension's operation writes through it (`CallHost::writer`).
impl pe2_ext::FileWriter for Writer {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        std::io::Write::write_all(self, bytes).map_err(Into::into)
    }

    fn finish(self: Box<Self>) -> Result<File> {
        Writer::finish(*self)
    }
}

/// The hashes of a value's stored contents.
pub(crate) fn stored_contents(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Typed(typed) => out.extend(typed.files.iter().filter_map(|f| match &f.content {
            Content::Stored { sha1, .. } => Some(sha1.clone()),
            Content::Inline(_) => None,
        })),
        Value::List(items) => items.iter().for_each(|item| stored_contents(item, out)),
        Value::Image(_) | Value::Table(_) | Value::Pcm(_) | Value::Video(_) | Value::Audio(_) | Value::Marks(_) | Value::Stream(_) => {}
    }
}

/// How large a content a writer holds in memory before spilling it to the store.
pub const SPILL: usize = 1 << 20;

/// A file being made (`Call::writer`): its bytes in memory up to `SPILL`, past it in a file of the store's, hashed as
/// they come.
pub struct Writer {
    name: String,
    store: Option<Store>,
    memory: Vec<u8>,
    spilled: Option<(std::fs::File, std::path::PathBuf)>,
    hasher: Sha1,
    size: u64,
}

impl std::io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(bytes);
        self.size += bytes.len() as u64;
        if let Some((file, _)) = &mut self.spilled {
            file.write_all(bytes)?;
            return Ok(bytes.len());
        }
        self.memory.extend_from_slice(bytes);
        if self.memory.len() > SPILL
            && let Some(store) = &self.store
        {
            let (mut file, path) = store.spill().map_err(std::io::Error::other)?;
            file.write_all(&self.memory)?;
            self.memory = Vec::new();
            self.spilled = Some((file, path));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.spilled.as_mut().map_or(Ok(()), |(file, _)| file.flush())
    }
}

impl Writer {
    /// The file made: its bytes inline, or - spilled - the blob of their hash in the store.
    ///
    /// # Errors
    /// What was spilled cannot be written or kept.
    pub fn finish(self) -> Result<File> {
        let Writer { name, store, memory, spilled, hasher, size } = self;
        let Some((mut file, path)) = spilled else { return Ok(File::new(&name, memory)) };
        std::io::Write::flush(&mut file)?;
        drop(file);
        let sha1 = crate::extract::hex(&hasher.finalize());
        store.context("spilled with no store")?.adopt(&path, &sha1)?;
        Ok(File { name, content: Content::Stored { sha1, size } })
    }
}

/// An output's value, or why it could not be made.
pub type Made = std::result::Result<Arc<Value>, Arc<str>>;

/// Values a plan's steps are known to make - kept outputs found in the store - with what they read: an evaluation
/// takes them as made, and runs nothing they are made of.
pub type Known = Vec<(usize, Arc<Value>, Reads)>;

/// Make some of a plan's outputs - each by its index and its parameter's value - running only what they reach, from
/// what is known made: their values, and what was read of the node making them.
#[must_use]
pub fn evaluate(
    plan: &Plan,
    ops: &Ops,
    node: &dyn Node,
    wanted: &[(usize, Option<&str>)],
    known: &Known,
    (store, reach): (Option<&Store>, Option<Reach<'_>>),
) -> (Vec<Made>, Reads) {
    let reads = Mutex::new(Reads::default());
    let mut made: Vec<Option<Made>> = vec![None; wanted.len()];
    // Outputs of one parameter's value run together; those of none with the first.
    let mut groups: Vec<(Option<&str>, Vec<usize>)> = Vec::new();
    for (i, (_, parameter)) in wanted.iter().enumerate() {
        match groups.iter_mut().find(|(p, _)| p == parameter || parameter.is_none()) {
            Some((_, members)) => members.push(i),
            None => groups.push((*parameter, vec![i])),
        }
    }
    let others = Mutex::default();
    let context = Evaluation { plan, ops, node, known, store, reach, reads: &reads, others: &others };
    for (parameter, members) in groups {
        let outputs: Vec<usize> = members.iter().map(|&i| wanted[i].0).collect();
        let values = context.run(&outputs, parameter);
        for (i, value) in members.into_iter().zip(values) {
            made[i] = Some(value);
        }
    }
    let made = made.into_iter().map(|m| m.unwrap_or_else(|| Err("not made".into()))).collect();
    (made, reads.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// What an evaluation runs with.
struct Evaluation<'a> {
    plan: &'a Plan,
    ops: &'a Ops,
    node: &'a dyn Node,
    known: &'a Known,
    store: Option<&'a Store>,
    reach: Option<Reach<'a>>,
    reads: &'a Mutex<Reads>,
    others: &'a Others,
}

impl Evaluation<'_> {
    fn run(&self, outputs: &[usize], parameter: Option<&str>) -> Vec<Made> {
        let steps: Vec<usize> = outputs.iter().map(|&o| self.plan.outputs[o].from).collect();
        self.run_steps(&steps, parameter)
    }

    /// The values of these steps, made as `run` makes outputs'.
    fn run_steps(&self, targets: &[usize], parameter: Option<&str>) -> Vec<Made> {
        let plan = self.plan;
        let known: HashMap<usize, (&Arc<Value>, &Reads)> = self.known.iter().map(|(s, v, r)| (*s, (v, r))).collect();
        let needed = plan.steps_reached(targets, &known.keys().copied().collect());
        // How many times each step's value is still to be taken: by the steps needed, and by the outputs.
        let mut left = vec![0usize; plan.steps.len()];
        for (s, step) in plan.steps.iter().enumerate().filter(|(s, _)| needed[*s] && !known.contains_key(s)) {
            let _ = s;
            for &from in step.inputs.iter().flatten() {
                left[from] += 1;
            }
        }
        for &t in targets {
            left[t] += 1;
        }
        let mut slots: Vec<Option<Made>> = vec![None; plan.steps.len()];
        let take = |slots: &mut Vec<Option<Made>>, left: &mut Vec<usize>, s: usize| -> Made {
            left[s] -= 1;
            let made = if left[s] == 0 { slots[s].take() } else { slots[s].clone() };
            made.unwrap_or_else(|| Err("not made".into()))
        };
        let recorder = Recorder { node: self.node, reads: self.reads };
        for s in (0..plan.steps.len()).filter(|&s| needed[s]) {
            if let Some((value, reads)) = known.get(&s) {
                self.reads.lock().unwrap_or_else(std::sync::PoisonError::into_inner).merge(reads);
                slots[s] = Some(Ok(Arc::clone(value)));
                continue;
            }
            let step = &plan.steps[s];
            let mut inputs = Vec::with_capacity(step.inputs.len());
            let mut failed = None;
            for port in &step.inputs {
                let mut values = Vec::with_capacity(port.len());
                for &from in port {
                    match take(&mut slots, &mut left, from) {
                        Ok(value) => values.push(Input(value)),
                        Err(e) => failed = Some(e),
                    }
                }
                inputs.push(values);
            }
            let made = match failed {
                Some(e) => Err(e),
                None => self.step(step, inputs, parameter, recorder),
            };
            slots[s] = Some(made);
        }
        targets.iter().map(|&t| take(&mut slots, &mut left, t)).collect()
    }

    /// Run a step: what it is given checked against its ports (their variables bound by it), what it makes against
    /// what it says it makes.
    fn step(&self, step: &super::Step, mut inputs: Vec<Vec<Input>>, parameter: Option<&str>, recorder: Recorder<'_>) -> Made {
        let fail = |what: String| -> Made { Err(Arc::from(format!("{}: {what}", step.at))) };
        let Some(def) = self.ops.get(&step.op) else { return fail(format!("`{}`: no extension provides it", step.op)) };
        let Some(run) = def.run.as_ref() else {
            let Some(start) = def.start.as_ref() else { return fail(format!("`{}` makes nothing", step.op)) };
            // A stream, for what takes it from here: opened, and read in turn.
            let others = self.others;
            let mut call = Call { inputs, settings: &step.settings, parameter, recorder, store: self.store, reach: self.reach, others };
            return match start(&mut call) {
                Ok(producer) => {
                    let stream = super::stream::Stream::open(producer);
                    Ok(Arc::new(Value::Stream(pe2_ext::StreamInput::of(Arc::new(super::stream::InTurn::new(&stream))))))
                }
                Err(e) => fail(format!("{e:#}")),
            };
        };
        let mut bound = Vec::new();
        for (port, given) in def.inputs.iter().zip(&mut inputs) {
            for value in given {
                let ty = value.value().ty();
                if !port.ty.unify(&ty, &mut bound) {
                    return fail(format!("its `{}` takes {}, and is given {ty}", port.name, port.ty));
                }
                // A port asking for a file is given one: a value that is one file, its content where a program can
                // read it - in the store, where it was in memory.
                if port.ty.wants_file() {
                    match self.as_file(value) {
                        Ok(file) => *value = file,
                        Err(why) => return fail(format!("its `{}` takes a file: {why}", port.name)),
                    }
                }
            }
        }
        let others = self.others;
        let mut call = Call { inputs, settings: &step.settings, parameter, recorder, store: self.store, reach: self.reach, others };
        let value = match run(&mut call) {
            Ok(value) => value,
            Err(e) => return fail(format!("{e:#}")),
        };
        // With several, what it makes is a list of them, each checked where it is picked.
        let one = def.output.as_ref().filter(|_| def.also.is_empty() && step.op != super::PICK);
        if let Some(ty) = one.map(|t| t.bind(&bound)).filter(|t| !t.unify(&value.ty(), &mut Vec::new())) {
            return fail(format!("`{}` made {}, not {ty}", step.op, value.ty()));
        }
        Ok(Arc::new(value))
    }

    /// A value as one file, its content kept in the store where it was in memory (and there is one).
    fn as_file(&self, input: &Input) -> std::result::Result<Input, String> {
        let Value::Typed(typed) = input.value() else { return Err(format!("given {}", input.value().ty())) };
        let file = typed.file().map_err(|e| e.to_string())?;
        match (&file.content, self.store) {
            (Content::Inline(bytes), Some(store)) => {
                let blob = store.keep(bytes).map_err(|e| format!("{e:#}"))?;
                let content = Content::Stored { sha1: blob.sha1, size: blob.size };
                let mut typed = typed.clone();
                typed.files = vec![File { name: file.name.clone(), content }];
                Ok(Input(Arc::new(Value::Typed(typed))))
            }
            _ => Ok(input.clone()),
        }
    }
}

/// What kept values hold: raised when that changes, so that none kept before is taken for one made now.
const VALUES: u32 = 3;

/// Evaluates nodes' outputs, keeping those retained in the project's store.
pub struct Engine {
    pub ops: Ops,
    store: Option<Store>,
    /// The config being built.
    pub config: Option<pe2_ext::data::BuildConfig>,
}

/// An output as the engine gives it: its value, where it was made or read now, its hash, and what it read - by which
/// a copy of it is known stale.
pub struct Given {
    pub value: Option<Arc<Value>>,
    pub hash: String,
    /// Found kept, not made now.
    pub kept: bool,
    pub reads: Reads,
}

impl Engine {
    /// An engine keeping retained outputs in a store; none keeps nothing.
    #[must_use]
    pub fn new(ops: Ops, store: Option<Store>) -> Engine {
        Engine { ops, store, config: None }
    }

    #[must_use]
    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }

    /// The key an output's trace is kept under: its node (by uid, else by path), what makes it, and its parameter's
    /// value. Two nodes making alike keep a trace each, of what each read, and share the value they made.
    #[must_use]
    pub fn key(&self, plan: &Plan, node: &dyn Node, output: usize, parameter: Option<&str>) -> String {
        let text = format!("{VALUES}\n{}\n{}\n{parameter:?}", id(node), plan.describe(output, &self.ops));
        crate::extract::sha1_hex(text.as_bytes())
    }

    /// An output's hash where it is kept and what it read still holds; none where it must be made.
    #[must_use]
    pub fn kept(&self, plan: &Plan, node: &dyn Node, output: usize, parameter: Option<&str>) -> Option<Trace> {
        self.kept_in(None, plan, node, output, parameter)
    }

    /// An output's trace as `kept` finds it, what it read of other nodes told in `reach`.
    #[must_use]
    pub fn kept_in(&self, reach: Option<Reach<'_>>, plan: &Plan, node: &dyn Node, output: usize, parameter: Option<&str>) -> Option<Trace> {
        let store = self.store.as_ref()?;
        let trace = store.trace(&self.key(plan, node, output, parameter))?;
        let same_config = trace.reads.config.as_ref().is_none_or(|read| self.config.as_ref().is_some_and(|config| config.name == *read));
        (trace.node == id(node) && same_config && trace.reads.hold(node, reach) && store.has(&trace.value)).then_some(trace)
    }

    /// An output by its address (`native`, `picture/night`): found kept where its trace holds - its value then read only
    /// where `read` asks - else made now, from what its plan's other kept outputs hold, and kept as it says.
    ///
    /// # Errors
    /// The node has no such output, or it could not be made or kept.
    pub fn output(&self, plan: &Plan, node: &dyn Node, address: &str, read: bool) -> Result<Given> {
        self.output_with(plan, node, address, None, read)
    }

    /// An output as `output` makes it, the parameter of what it reaches given this value where the address gives none
    /// (a fixed output: the thumbnail's look).
    ///
    /// # Errors
    /// As `output`.
    pub fn output_with(&self, plan: &Plan, node: &dyn Node, address: &str, supplied: Option<&str>, read: bool) -> Result<Given> {
        self.output_in(None, plan, node, address, supplied, read)
    }

    /// An output as `output_with` makes it, its operations reaching other nodes' outputs in `reach`.
    ///
    /// # Errors
    /// As `output`.
    pub fn output_in(
        &self,
        reach: Option<Reach<'_>>,
        plan: &Plan,
        node: &dyn Node,
        address: &str,
        supplied: Option<&str>,
        read: bool,
    ) -> Result<Given> {
        let (output, parameter) = plan.output(address).with_context(|| format!("{} has no output `{address}`", node.path()))?;
        let parameter = parameter.as_deref().or(supplied);
        let retained = plan.outputs[output].retain != Retain::Transient;
        if retained && let Some(trace) = self.kept_in(reach, plan, node, output, parameter) {
            // Read where asked; one kept as this store does not read values is made again.
            let value = match (read, &self.store) {
                (true, Some(store)) => store.get(&trace.value).ok().map(Arc::new),
                _ => None,
            };
            if value.is_some() || !read {
                return Ok(Given { value, hash: trace.value, kept: true, reads: trace.reads });
            }
        }
        let known = self.known(reach, plan, node, output);
        let (mut made, reads) = evaluate(plan, &self.ops, node, &[(output, parameter)], &known, (self.store.as_ref(), reach));
        let value = made.pop().unwrap_or_else(|| Err("not made".into())).map_err(|e| anyhow!("{e}"))?;
        let hash = super::hash(&value);
        if retained && let Some(store) = &self.store {
            store.put(&hash, &value, &|sha1| node.read_content(sha1).ok())?;
            let trace = Trace { value: hash.clone(), reads: reads.clone(), node: id(node).to_string(), output: address.to_string() };
            store.put_trace(&self.key(plan, node, output, parameter), &trace)?;
        }
        Ok(Given { value: Some(value), hash, kept: false, reads })
    }

    /// Open a stream output (`stream`): what its operation takes evaluated as any output's inputs - found kept where
    /// they are - and its producer started with them. Returns the producer, what it read, and the key it is known by.
    ///
    /// # Errors
    /// The node has no such output, it is no stream, or what it takes could not be made.
    pub fn open_stream(
        &self,
        reach: Option<Reach<'_>>,
        plan: &Plan,
        node: &dyn Node,
        address: &str,
    ) -> Result<(Box<dyn super::stream::Producer>, Reads, String)> {
        let (output, parameter) = plan.output(address).with_context(|| format!("{} has no output `{address}`", node.path()))?;
        let parameter = parameter.as_deref();
        let step = &plan.steps[plan.outputs[output].from];
        let def = self.ops.get(&step.op).with_context(|| format!("`{}`: no extension provides it", step.op))?;
        let start = def.start.as_ref().with_context(|| format!("{address} is no stream"))?;
        let known = self.known(reach, plan, node, output);
        let reads = Mutex::new(Reads::default());
        let others = Mutex::default();
        let context = Evaluation { plan, ops: &self.ops, node, known: &known, store: self.store.as_ref(), reach, reads: &reads, others: &others };
        let mut inputs = Vec::with_capacity(step.inputs.len());
        for port in &step.inputs {
            let values = context.run_steps(port, parameter);
            inputs.push(values.into_iter().map(|v| v.map(Input).map_err(|e| anyhow!("{e}"))).collect::<Result<Vec<_>>>()?);
        }
        let recorder = Recorder { node, reads: &reads };
        let mut call = Call { inputs, settings: &step.settings, parameter, recorder, store: self.store.as_ref(), reach, others: &others };
        let producer = start(&mut call).with_context(|| format!("{}: {address} could not be opened", node.path()))?;
        let reads = reads.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok((producer, reads, self.key(plan, node, output, parameter)))
    }

    /// Run one operation now, outside any plan, given its values by port: what an inverse writes from what its import
    /// makes.
    ///
    /// # Errors
    /// No extension provides it, or it failed.
    pub fn run(&self, op: &str, inputs: Vec<Vec<Value>>, settings: &Table, node: &dyn Node) -> Result<Value> {
        self.run_in(None, op, inputs, settings, node)
    }

    /// `run`, reaching the project in `reach`.
    ///
    /// # Errors
    /// No extension provides it, or it failed.
    pub fn run_in(&self, reach: Option<Reach<'_>>, op: &str, inputs: Vec<Vec<Value>>, settings: &Table, node: &dyn Node) -> Result<Value> {
        let def = self.ops.get(op).with_context(|| format!("`{op}`: no extension provides it"))?;
        let run = def.run.as_ref().with_context(|| format!("`{op}` makes nothing"))?;
        let reads = Mutex::new(Reads::default());
        let inputs = inputs.into_iter().map(|port| port.into_iter().map(|v| Input(Arc::new(v))).collect()).collect();
        let recorder = Recorder { node, reads: &reads };
        let others = &Mutex::default();
        let mut call = Call { inputs, settings, parameter: None, recorder, store: self.store.as_ref(), reach, others };
        run(&mut call)
    }

    /// The plan's cached outputs found kept - but `except` - as what the steps making them are known to make.
    fn known(&self, reach: Option<Reach<'_>>, plan: &Plan, node: &dyn Node, except: usize) -> Known {
        let Some(store) = &self.store else { return Vec::new() };
        let cached = plan.outputs.iter().enumerate().filter(|(i, o)| *i != except && o.retain != Retain::Transient && o.parameter.is_none());
        cached
            .filter_map(|(i, o)| {
                let trace = self.kept_in(reach, plan, node, i, None)?;
                let value = store.get(&trace.value).ok()?;
                Some((o.from, Arc::new(value), trace.reads))
            })
            .collect()
    }
}

/// The node of what is about no asset: a hook's.
#[derive(Default)]
pub struct Nowhere {
    fields: Table,
}

impl Node for Nowhere {
    fn path(&self) -> &'static str {
        ""
    }

    fn uid(&self) -> Option<&str> {
        None
    }

    fn declaration(&self) -> &'static str {
        ""
    }

    fn fields(&self) -> &Table {
        &self.fields
    }

    fn file_sha1(&self, _: &str) -> Option<String> {
        None
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        anyhow::bail!("{name}: it has no node")
    }
}

/// A node as traces name it: by uid, else by path.
#[must_use]
pub fn id(node: &dyn Node) -> &str {
    node.uid().unwrap_or_else(|| node.path())
}
