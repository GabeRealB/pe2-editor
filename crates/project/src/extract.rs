//! What running an extraction plan (`plan`) does beyond reading the discs: hashing what it makes, writing an asset's
//! source from its native files and checking it imports back into them (`Sourcing`), and joining what the layer's
//! bundles were given into what a container assembled from them is (`assembled_sha1`: a disc's movie file).

use anyhow::{Context, Result, bail};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::changeset::Changeset;
use crate::ext::Extensions;

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[must_use]
pub fn sha1_hex(bytes: &[u8]) -> String {
    hex(&Sha1::digest(bytes))
}

pub(crate) fn sha1_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let (mut h, mut buf) = (Sha1::new(), vec![0; 1 << 22]);
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// What makes an asset's source of its native files: the extensions, and the engine importing them back - which keeps
/// the imports it checked.
pub(crate) struct Sourcing<'a> {
    pub extensions: &'a Extensions,
    pub engine: &'a crate::graph::exec::Engine,
    /// How many assets were written as their sources, by every worker.
    pub sources: std::sync::atomic::AtomicUsize,
}

/// An asset as extraction lends it to its importer: its declaration and the files given.
struct Lent<'a> {
    path: &'a str,
    uid: Option<&'a str>,
    fields: toml::Table,
    declaration: &'a str,
    files: BTreeMap<String, Vec<u8>>,
}

impl pe2_ext::Source for Lent<'_> {
    fn path(&self) -> &str {
        self.path
    }

    fn uid(&self) -> Option<&str> {
        None
    }

    fn fields(&self) -> &toml::Table {
        &self.fields
    }

    fn declaration(&self) -> &str {
        self.declaration
    }

    fn has_file(&self, name: &str) -> bool {
        self.files.contains_key(name)
    }

    fn read(&self, name: &str) -> pe2_ext::Result<Vec<u8>> {
        self.files.get(name).cloned().ok_or_else(|| pe2_ext::anyhow!("{}: no file {name}", self.path))
    }
}

impl crate::graph::exec::Node for Lent<'_> {
    fn path(&self) -> &str {
        self.path
    }

    fn uid(&self) -> Option<&str> {
        self.uid
    }

    fn declaration(&self) -> &str {
        self.declaration
    }

    fn fields(&self) -> &toml::Table {
        &self.fields
    }

    fn file_sha1(&self, name: &str) -> Option<String> {
        self.files.get(name).map(|bytes| sha1_hex(bytes))
    }

    fn file_names(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        self.files.get(name).cloned().with_context(|| format!("{}: no file {name}", self.path))
    }

    fn read_content(&self, sha1: &str) -> Result<Vec<u8>> {
        let found = self.files.values().find(|bytes| sha1_hex(bytes) == sha1);
        found.cloned().with_context(|| format!("{}: no content {sha1}", self.path))
    }
}

impl Sourcing<'_> {
    /// A content, whole: inline, or kept in the store.
    fn read(&self, content: &crate::graph::Content) -> Result<Vec<u8>> {
        match content {
            crate::graph::Content::Inline(bytes) => Ok(bytes.clone()),
            crate::graph::Content::Stored { sha1, .. } => {
                let path = self.engine.store().context("no store")?.blob_path(sha1);
                std::fs::read(&path).with_context(|| path.display().to_string())
            }
        }
    }

    /// An asset's `node.toml` and files as its bundle holds them: its native files where its import is undone by
    /// none, else its import's source alone - written by the operation undoing it from the native files, imported back
    /// by its stack with the declaration less what the import derives, and compared, files and settings both; its
    /// import kept.
    pub(crate) fn bundle(
        &self,
        path: &str,
        uid: &str,
        toml: String,
        native: pe2_ext::Made,
    ) -> Result<(String, pe2_ext::Made)> {
        let fields: toml::Table = toml.parse()?;
        let Some(import) = crate::graph::import_of(&fields) else { return Ok((toml, native)) };
        let (op, source) = (import.op.to_string(), import.source.to_string());
        let def = self.extensions.op(&op).with_context(|| format!("{path}: no extension provides {op}"))?;
        let Some(inverse) = def.inverse.clone() else { return Ok((toml, native)) };
        let pe2_ext::Type::Named(ty) = &def.output else {
            bail!("{path}: {op} makes {}, no extension's type", def.output)
        };
        let lent = Lent { path, uid: Some(uid), fields: fields.clone(), declaration: &toml, files: BTreeMap::new() };
        let files = native.iter().map(|(name, bytes)| pe2_ext::File::new(name, bytes.clone())).collect();
        let settings = crate::graph::with_settings(fields.clone());
        let native_form =
            crate::graph::Value::Typed(pe2_ext::Typed { ty: ty.clone(), files, settings, ..pe2_ext::Typed::default() });
        let written = self.engine.run(&inverse, vec![vec![native_form]], &toml::Table::new(), &lent);
        // One file, the source itself (`source.ora`); several, a folder of that name (`medium.ufo/glyphs/A_.glif`).
        let written_files: Vec<(String, Vec<u8>)> = match written
            .with_context(|| format!("{path}: {inverse} could not write its source"))?
        {
            crate::graph::Value::Typed(written) if written.files.len() == 1 => {
                vec![(source.clone(), self.read(&written.file()?.content)?)]
            }
            crate::graph::Value::Typed(written) if !written.files.is_empty() => written
                .files
                .iter()
                .map(|file| Ok((format!("{}/{}", source.trim_end_matches('/'), file.name), self.read(&file.content)?)))
                .collect::<Result<_>>()?,
            other => bail!("{path}: {inverse} wrote {}, not a file", other.ty()),
        };
        // What the bundle says: the declaration without what the source holds. Imported back by its stack, as the
        // project will - which keeps the import.
        let written = crate::imports::without_derived(&toml, &def.derives)?;
        let placed = crate::graph::layers::settings_on_layers(&written)?;
        let lent = Lent {
            path,
            uid: Some(uid),
            fields: crate::graph::with_settings(placed.parse()?),
            declaration: &written,
            files: written_files.iter().cloned().collect(),
        };
        let (plan, wrong) = crate::graph::Plan::of(&lent.fields, &self.engine.ops, &[]);
        if let Some(wrong) = wrong.first() {
            bail!("{path}: {}: {}", wrong.at, wrong.what);
        }
        let result = plan
            .result_output()
            .map(|o| plan.outputs[o].name.clone())
            .with_context(|| format!("{path}: its stack keeps no result"))?;
        let given = self
            .engine
            .output(&plan, &lent, &result, true)
            .with_context(|| format!("{path}: {op} could not import its source"))?;
        let Some(crate::graph::Value::Typed(made)) = given.value.as_deref() else {
            bail!("{path}: its import made no native form");
        };
        let made_map: BTreeMap<String, Vec<u8>> =
            made.files.iter().map(|f| Ok((f.name.clone(), self.read(&f.content)?))).collect::<Result<_>>()?;
        let native_map: BTreeMap<String, Vec<u8>> = native.into_iter().collect();
        if made_map != native_map {
            let differ: Vec<&String> =
                native_map.keys().chain(made_map.keys()).filter(|n| native_map.get(*n) != made_map.get(*n)).collect();
            bail!("{path}: its {source} does not import back into the disc's files ({differ:?} differ)");
        }
        let resolved = crate::imports::resolve(&lent.fields, &made.settings, &def.derives);
        let settings_of = |fields: &toml::Table| {
            let mut settings = fields.clone();
            settings.remove("stack");
            settings
        };
        if settings_of(&resolved.settings) != settings_of(&fields) || !resolved.orphans.is_empty() {
            bail!(
                "{path}: the settings its {source} gives are not the declaration's (orphaned: {:?})",
                resolved.orphans
            );
        }
        self.sources.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok((written, written_files))
    }
}

/// A bundle's `node.toml` with a stack added, before the originals.
pub(crate) fn with_stack(toml: &str, stack: &str) -> String {
    let body = toml.find("\n[original.").map_or(toml.len(), |i| i + 1);
    let (body, originals) = toml.split_at(body);
    let separator = if originals.is_empty() { "" } else { "\n" };
    format!("{}\n\n{}\n{separator}{originals}", body.trim_end(), stack.trim())
}

/// A bundle of the layer as written, as a node of the work graph: its declaration and stored files as on disk, each
/// file hashed once, when first asked.
struct Written {
    path: String,
    uid: String,
    /// The bundle's folder in the layer of its node, and in the layer of its files.
    dirs: [PathBuf; 2],
    declaration: String,
    fields: toml::Table,
    sha1s: std::sync::Mutex<HashMap<String, String>>,
}

impl Written {
    fn open(layers: [&Path; 2], path: &str, uid: &str) -> Result<Written> {
        let dirs = layers.map(|layer| layer.join(format!("{path}{}", crate::index::BUNDLE_SUFFIX)));
        let declaration =
            fs::read_to_string(dirs[0].join("node.toml")).with_context(|| format!("{path}: its node.toml"))?;
        let fields =
            crate::graph::with_settings(declaration.parse().with_context(|| format!("{path}: its node.toml"))?);
        Ok(Written {
            path: path.to_string(),
            uid: uid.to_string(),
            dirs,
            declaration,
            fields,
            sha1s: std::sync::Mutex::default(),
        })
    }

    /// A file's place: in the layer of its files, else beside the node.
    fn at(&self, name: &str) -> PathBuf {
        let filled = self.dirs[1].join(name);
        if crate::content::exists(&filled) { filled } else { self.dirs[0].join(name) }
    }
}

impl crate::graph::exec::Node for Written {
    fn path(&self) -> &str {
        &self.path
    }

    fn uid(&self) -> Option<&str> {
        Some(&self.uid)
    }

    fn declaration(&self) -> &str {
        &self.declaration
    }

    fn fields(&self) -> &toml::Table {
        &self.fields
    }

    fn file_sha1(&self, name: &str) -> Option<String> {
        let mut sha1s = self.sha1s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(sha1) = sha1s.get(name) {
            return Some(sha1.clone());
        }
        let sha1 = crate::content::sha1(&self.at(name)).ok()?;
        sha1s.insert(name.to_string(), sha1.clone());
        Some(sha1)
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        let path = self.at(name);
        crate::content::read(&path).with_context(|| path.display().to_string())
    }

    fn file_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.dirs.iter().flat_map(|d| crate::index::bundle_files(d).into_keys()).collect();
        names.sort();
        names.dedup();
        names
    }

    fn file_size(&self, name: &str) -> Option<u64> {
        crate::content::len(&self.at(name)).ok()
    }

    fn read_head(&self, name: &str, len: usize) -> Vec<u8> {
        crate::content::head(&self.at(name), len)
    }

    fn content_path(&self, sha1: &str) -> Option<PathBuf> {
        let sha1s = self.sha1s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        sha1s.iter().find(|(_, s)| *s == sha1).map(|(name, _)| self.at(name))
    }
}

/// The layer's bundles as the work graph reaches them, by uid or path: each read from disk when first asked for.
struct Layer<'a> {
    /// The layer of the nodes, and the layer of their files.
    roots: [&'a Path; 2],
    extensions: &'a Extensions,
    uids: HashMap<String, String>,
    /// By path: its uid, and the node once read.
    nodes: HashMap<String, (String, std::sync::OnceLock<Option<Written>>)>,
    ops: &'a crate::graph::Ops,
}

impl<'a> Layer<'a> {
    /// The layer's nodes, each by its path and uid.
    fn of(
        roots: [&'a Path; 2],
        all: Vec<(String, String)>,
        ops: &'a crate::graph::Ops,
        extensions: &'a Extensions,
    ) -> Layer<'a> {
        let uids = all.iter().map(|(path, uid)| (uid.clone(), path.clone())).collect();
        let nodes = all.into_iter().map(|(path, uid)| (path, (uid, std::sync::OnceLock::new()))).collect();
        Layer { roots, extensions, uids, nodes, ops }
    }
}

impl crate::graph::exec::World for Layer<'_> {
    fn node(&self, reference: &toml::Value) -> Option<(&dyn crate::graph::exec::Node, crate::graph::Plan)> {
        let path = match reference.as_str().filter(|r| !r.starts_with("uid://")) {
            Some(path) => path,
            None => self.uids.get(crate::index::reference(reference)?.0.as_str())?.as_str(),
        };
        let (path, (uid, slot)) = self.nodes.get_key_value(path)?;
        let node = slot.get_or_init(|| Written::open(self.roots, path, uid).ok()).as_ref()?;
        // Planned as the project plans it: its stack, then what its type offers beyond it (its settings among them).
        let tail = self.extensions.tail_of_shape(&std::sync::Arc::from(crate::graph::shape(&node.fields)));
        let (plan, _) = crate::graph::Plan::of(&node.fields, self.ops, &tail);
        Some((node as &dyn crate::graph::exec::Node, plan))
    }
}

/// Make each container assembled from its movies (a disc's movie file: its stages' movies with sound, in order) by its
/// stack, as the project will - those movies, as written, joined - holding it to the disc's file, and keeping
/// it in the store. Returns how many were made.
/// What a container assembled from its movies is, joined by its stack from the layers as written: its file's sha1. The
/// nodes are the first layer's, by its changeset (`<layer>.toml`), their files the second's (the generated layer
/// filling it) or, where it has none of a name, the first's.
///
/// # Errors
/// The changeset cannot be read, the node is not there, or its stack cannot join what it joins.
pub(crate) fn assembled_sha1(
    layers: [&Path; 2],
    node: &str,
    engine: &crate::graph::exec::Engine,
    extensions: &Extensions,
) -> Result<String> {
    let changeset = Changeset::load(&layers[0].with_extension("toml"))?;
    let all = changeset
        .entries
        .iter()
        .filter_map(|(uid, at)| Some((at.strip_suffix(crate::index::BUNDLE_SUFFIX)?.to_string(), uid.clone())))
        .collect();
    let world = Layer::of(layers, all, &engine.ops, extensions);
    joined_sha1(&world, node, engine)
}

/// What a node's stack makes of the layer, as one file: its sha1.
fn joined_sha1(world: &Layer<'_>, path: &str, engine: &crate::graph::exec::Engine) -> Result<String> {
    let reach = crate::graph::exec::Reach { engine, world };
    let (node, plan) = crate::graph::exec::World::node(world, &toml::Value::String(path.to_string()))
        .with_context(|| format!("{path}: not written"))?;
    let result = plan
        .result_output()
        .map(|o| plan.outputs[o].name.clone())
        .with_context(|| format!("{path}: its stack keeps no result"))?;
    let _span = tracing::info_span!("assembled", path = %path).entered();
    let given = engine
        .output_in(Some(reach), &plan, node, &result, None, true)
        .with_context(|| format!("{path}: its movies could not be joined"))?;
    Ok(match given.value.as_deref() {
        Some(crate::graph::Value::Typed(typed)) => match &typed.file()?.content {
            crate::graph::Content::Stored { sha1, .. } => sha1.clone(),
            crate::graph::Content::Inline(bytes) => sha1_hex(bytes),
        },
        _ => bail!("{path}: its stack made no file"),
    })
}
