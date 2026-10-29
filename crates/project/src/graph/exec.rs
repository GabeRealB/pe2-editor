//! Evaluating a node's outputs: only the operations they reach run, in the plan's order, each result moved to what
//! takes it where one thing does and shared, read-only, until its last taker where several do. What the operations
//! read of their node - its files, its declaration - is recorded as they read it: the outputs' trace, by which a kept
//! output is known still to hold without making it again.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use toml::Table;

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

/// An asset of the index is a node: its stored files those of its bundle, not what imports made.
impl Node for crate::index::Asset {
    fn path(&self) -> &str {
        &self.path
    }

    fn uid(&self) -> Option<&str> {
        self.uid.as_deref()
    }

    fn declaration(&self) -> &str {
        self.text()
    }

    fn fields(&self) -> &Table {
        crate::index::Asset::fields(self)
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
}

impl Reads {
    /// Add what another read: an output made from a kept one read what that one did.
    pub fn merge(&mut self, other: &Reads) {
        self.files.extend(other.files.iter().map(|(k, v)| (k.clone(), v.clone())));
        if other.declaration.is_some() {
            self.declaration.clone_from(&other.declaration);
        }
    }

    /// Whether the node still reads the same: each file read has its hash, the declaration its.
    #[must_use]
    pub fn hold(&self, node: &dyn Node) -> bool {
        self.files.iter().all(|(name, sha1)| node.file_sha1(name).unwrap_or_default() == *sha1)
            && self.declaration.as_ref().is_none_or(|d| *d == node.declaration_sha1())

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
}

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
        Ok(Typed { ty, also: names, files: vec![file], ..Typed::default() })
    }

    /// Where a stored content is: in the store, or among its node's files.
    #[must_use]
    pub fn content_path(&self, sha1: &str) -> Option<std::path::PathBuf> {
        self.store.map(|s| s.blob_path(sha1)).filter(|p| p.exists()).or_else(|| self.recorder.node.content_path(sha1))
    }

    /// A content, whole.
    ///
    /// # Errors
    /// It is stored where it cannot be found or read.
    pub fn read(&self, content: &Content) -> Result<Vec<u8>> {
        match content {
            Content::Inline(bytes) => Ok(bytes.clone()),
            Content::Stored { sha1, .. } => match self.store.map(|s| s.blob_path(sha1)).filter(|p| p.exists()) {
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
    store: Option<&Store>,
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
    let context = Evaluation { plan, ops, node, known, store, reads: &reads };
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
    reads: &'a Mutex<Reads>,
}

impl Evaluation<'_> {
    fn run(&self, outputs: &[usize], parameter: Option<&str>) -> Vec<Made> {
        let plan = self.plan;
        let known: HashMap<usize, (&Arc<Value>, &Reads)> = self.known.iter().map(|(s, v, r)| (*s, (v, r))).collect();
        let needed = plan.reached_until(outputs, &known.keys().copied().collect());
        // How many times each step's value is still to be taken: by the steps needed, and by the outputs.
        let mut left = vec![0usize; plan.steps.len()];
        for (s, step) in plan.steps.iter().enumerate().filter(|(s, _)| needed[*s] && !known.contains_key(s)) {
            let _ = s;
            for &from in step.inputs.iter().flatten() {
                left[from] += 1;
            }
        }
        for &o in outputs {
            left[plan.outputs[o].from] += 1;
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
        outputs.iter().map(|&o| take(&mut slots, &mut left, plan.outputs[o].from)).collect()
    }

    /// Run a step: what it is given checked against its ports (their variables bound by it), what it makes against
    /// what it says it makes.
    fn step(&self, step: &super::Step, mut inputs: Vec<Vec<Input>>, parameter: Option<&str>, recorder: Recorder<'_>) -> Made {
        let fail = |what: String| -> Made { Err(Arc::from(format!("{}: {what}", step.at))) };
        let Some(def) = self.ops.get(&step.op) else { return fail(format!("`{}`: no extension provides it", step.op)) };
        let Some(run) = def.run.as_ref() else { return fail(format!("`{}` makes nothing", step.op)) };
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
        let mut call = Call { inputs, settings: &step.settings, parameter, recorder, store: self.store };
        let value = match run(&mut call) {
            Ok(value) => value,
            Err(e) => return fail(format!("{e:#}")),
        };
        if let Some(ty) = def.output.as_ref().map(|t| t.bind(&bound)).filter(|t| !t.unify(&value.ty(), &mut Vec::new())) {
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

/// Evaluates nodes' outputs, keeping those retained in the project's store.
pub struct Engine {
    pub ops: Ops,
    store: Option<Store>,
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
        Engine { ops, store }
    }

    #[must_use]
    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }

    /// The key an output's trace is kept under: its node (by uid, else by path), what makes it, and its parameter's
    /// value. Two nodes making alike keep a trace each, of what each read, and share the value they made.
    #[must_use]
    pub fn key(&self, plan: &Plan, node: &dyn Node, output: usize, parameter: Option<&str>) -> String {
        let text = format!("{}\n{}\n{parameter:?}", id(node), plan.describe(output, &self.ops));
        crate::extract::sha1_hex(text.as_bytes())
    }

    /// An output's hash where it is kept and what it read still holds; none where it must be made.
    #[must_use]
    pub fn kept(&self, plan: &Plan, node: &dyn Node, output: usize, parameter: Option<&str>) -> Option<Trace> {
        let store = self.store.as_ref()?;
        let trace = store.trace(&self.key(plan, node, output, parameter))?;
        (trace.node == id(node) && trace.reads.hold(node) && store.has(&trace.value)).then_some(trace)
    }

    /// An output by its address (`native`, `picture/night`): found kept where its trace holds - its value then read only
    /// where `read` asks - else made now, from what its plan's other kept outputs hold, and kept as it says.
    ///
    /// # Errors
    /// The node has no such output, or it could not be made or kept.
    pub fn output(&self, plan: &Plan, node: &dyn Node, address: &str, read: bool) -> Result<Given> {
        let (output, parameter) = plan.output(address).with_context(|| format!("{} has no output `{address}`", node.path()))?;
        let parameter = parameter.as_deref().or(plan.outputs[output].given.as_deref());
        let retained = plan.outputs[output].retain != Retain::Transient;
        if retained && let Some(trace) = self.kept(plan, node, output, parameter) {
            let value = if read {
                let store = self.store.as_ref().context("no store")?;
                Some(Arc::new(store.get(&trace.value).with_context(|| format!("{}: kept, and not readable", trace.value))?))
            } else {
                None
            };
            return Ok(Given { value, hash: trace.value, kept: true, reads: trace.reads });
        }
        let known = self.known(plan, node, output);
        let (mut made, reads) = evaluate(plan, &self.ops, node, &[(output, parameter)], &known, self.store.as_ref());
        let value = made.pop().unwrap_or_else(|| Err("not made".into())).map_err(|e| anyhow!("{e}"))?;
        let hash = super::hash(&value);
        if retained && let Some(store) = &self.store {
            store.put(&hash, &value, &|sha1| node.read_content(sha1).ok())?;
            let trace = Trace { value: hash.clone(), reads: reads.clone(), node: id(node).to_string(), output: address.to_string() };
            store.put_trace(&self.key(plan, node, output, parameter), &trace)?;
        }
        Ok(Given { value: Some(value), hash, kept: false, reads })
    }

    /// Run one operation now, outside any plan, given its values by port: what an inverse writes from what its import
    /// makes.
    ///
    /// # Errors
    /// No extension provides it, or it failed.
    pub fn run(&self, op: &str, inputs: Vec<Vec<Value>>, settings: &Table, node: &dyn Node) -> Result<Value> {
        let def = self.ops.get(op).with_context(|| format!("`{op}`: no extension provides it"))?;
        let run = def.run.as_ref().with_context(|| format!("`{op}` makes nothing"))?;
        let reads = Mutex::new(Reads::default());
        let inputs = inputs.into_iter().map(|port| port.into_iter().map(|v| Input(Arc::new(v))).collect()).collect();
        let mut call = Call { inputs, settings, parameter: None, recorder: Recorder { node, reads: &reads }, store: self.store.as_ref() };
        run(&mut call)
    }

    /// The plan's cached outputs found kept - but `except` - as what the steps making them are known to make.
    fn known(&self, plan: &Plan, node: &dyn Node, except: usize) -> Known {
        let Some(store) = &self.store else { return Vec::new() };
        let cached = plan.outputs.iter().enumerate().filter(|(i, o)| *i != except && o.retain == Retain::Cache && o.parameter.is_none());
        cached
            .filter_map(|(i, o)| {
                let trace = self.kept(plan, node, i, None)?;
                let value = store.get(&trace.value).ok()?;
                Some((o.from, Arc::new(value), trace.reads))
            })
            .collect()
    }
}

/// A node as traces name it: by uid, else by path.
#[must_use]
pub fn id(node: &dyn Node) -> &str {
    node.uid().unwrap_or_else(|| node.path())
}
