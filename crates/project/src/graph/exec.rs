//! Evaluating a node's outputs: only the operations they reach run, in the plan's order, each result moved to what
//! takes it where one thing does and shared, read-only, until its last taker where several do. What the operations
//! read of their node - its files, its declaration - is recorded as they read it: the outputs' trace, by which a kept
//! output is known still to hold without making it again.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use toml::Table;

use super::store::Store;
use super::{Ops, Plan, Retain, Value};

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
}

/// What operations read of their node: its files by their hashes (empty where it had none of the name), its
/// declaration's hash where they read its settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reads {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<String>,
}

impl Reads {
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

/// An operation running: what it is given, its settings, its output's parameter, and its node.
pub struct Call<'a> {
    inputs: Vec<Vec<Input>>,
    settings: &'a Table,
    parameter: Option<&'a str>,
    recorder: Recorder<'a>,
}

impl<'a> Call<'a> {
    /// The value given to a port taking one.
    ///
    /// # Errors
    /// The port was given none.
    pub fn take(&mut self, port: usize) -> Result<Input> {
        self.inputs.get_mut(port).and_then(Vec::pop).context("given nothing")
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

    /// A file of its node's bundle, recorded as read.
    ///
    /// # Errors
    /// The node has no such file, or it cannot be read.
    pub fn file(&self, name: &str) -> Result<Vec<u8>> {
        self.recorder.file(name);
        self.recorder.node.read(name)
    }

    /// What records its reads, for reading its node as a whole.
    #[must_use]
    pub fn recorder(&self) -> Recorder<'a> {
        self.recorder
    }
}

/// An output's value, or why it could not be made.
pub type Made = std::result::Result<Arc<Value>, Arc<str>>;

/// Make some of a plan's outputs - each by its index and its parameter's value - running only what they reach: their
/// values, and what was read of the node making them.
#[must_use]
pub fn evaluate(plan: &Plan, ops: &Ops, node: &dyn Node, wanted: &[(usize, Option<&str>)]) -> (Vec<Made>, Reads) {
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
    for (parameter, members) in groups {
        let outputs: Vec<usize> = members.iter().map(|&i| wanted[i].0).collect();
        let values = run(plan, ops, node, &outputs, parameter, &reads);
        for (i, value) in members.into_iter().zip(values) {
            made[i] = Some(value);
        }
    }
    let made = made.into_iter().map(|m| m.unwrap_or_else(|| Err("not made".into()))).collect();
    (made, reads.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner))
}

fn run(plan: &Plan, ops: &Ops, node: &dyn Node, outputs: &[usize], parameter: Option<&str>, reads: &Mutex<Reads>) -> Vec<Made> {
    let needed = plan.reached(outputs);
    // How many times each step's value is still to be taken: by the steps needed, and by the outputs.
    let mut left = vec![0usize; plan.steps.len()];
    for (step, _) in plan.steps.iter().zip(&needed).filter(|(_, n)| **n) {
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
    let recorder = Recorder { node, reads };
    for s in (0..plan.steps.len()).filter(|&s| needed[s]) {
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
            None => run_step(ops, step, inputs, parameter, recorder),
        };
        slots[s] = Some(made);
    }
    outputs.iter().map(|&o| take(&mut slots, &mut left, plan.outputs[o].from)).collect()
}

fn run_step(ops: &Ops, step: &super::Step, inputs: Vec<Vec<Input>>, parameter: Option<&str>, recorder: Recorder<'_>) -> Made {
    let def = ops.get(&step.op).ok_or_else(|| Arc::from(format!("{}: `{}`: no extension provides it", step.at, step.op)))?;
    let run = def.run.as_ref().ok_or_else(|| Arc::from(format!("{}: `{}` makes nothing", step.at, step.op)))?;
    let mut call = Call { inputs, settings: &step.settings, parameter, recorder };
    let value = run(&mut call).map_err(|e| Arc::from(format!("{}: {e:#}", step.at)))?;
    if let Some(ty) = def.output.filter(|ty| !value.ty().fits(*ty)) {
        return Err(Arc::from(format!("{}: `{}` made {}, not {}", step.at, step.op, value.ty().name(), ty.name())));
    }
    Ok(Arc::new(value))
}

/// Evaluates nodes' outputs, keeping those retained in the project's store.
pub struct Engine {
    pub ops: Ops,
    store: Option<Store>,
}

/// An output as the engine gives it: its value, where it was made or read now, and its hash.
pub struct Given {
    pub value: Option<Arc<Value>>,
    pub hash: String,
    /// Found kept, not made now.
    pub kept: bool,
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
        let id = node.uid().unwrap_or_else(|| node.path());
        let text = format!("{id}\n{}\n{parameter:?}", plan.describe(output, &self.ops));
        crate::extract::sha1_hex(text.as_bytes())
    }

    /// An output's hash where it is kept and what it read still holds; none where it must be made.
    #[must_use]
    pub fn kept(&self, plan: &Plan, node: &dyn Node, output: usize, parameter: Option<&str>) -> Option<super::store::Trace> {
        let store = self.store.as_ref()?;
        let trace = store.trace(&self.key(plan, node, output, parameter))?;
        (trace.reads.hold(node) && store.has(&trace.value)).then_some(trace)
    }

    /// An output by its address (`native`, `picture/0`): found kept where its trace holds - its value then read only
    /// where `read` asks - else made now, and kept as its layer says.
    ///
    /// # Errors
    /// The node has no such output, or it could not be made or kept.
    pub fn output(&self, plan: &Plan, node: &dyn Node, address: &str, read: bool) -> Result<Given> {
        let (output, parameter) = plan.output(address).with_context(|| format!("{} has no output `{address}`", node.path()))?;
        let parameter = parameter.as_deref();
        let retained = plan.outputs[output].retain != Retain::Transient;
        if retained && let Some(super::store::Trace { value: hash, .. }) = self.kept(plan, node, output, parameter) {
            let value = if read {
                let store = self.store.as_ref().context("no store")?;
                Some(Arc::new(store.get(&hash).with_context(|| format!("{hash}: kept, and not readable"))?))
            } else {
                None
            };
            return Ok(Given { value, hash, kept: true });
        }
        let (mut made, reads) = evaluate(plan, &self.ops, node, &[(output, parameter)]);
        let value = made.pop().unwrap_or_else(|| Err("not made".into())).map_err(|e| anyhow!("{e}"))?;
        let hash = value.hash();
        if retained && let Some(store) = &self.store {
            store.put(&hash, &value)?;
            store.put_trace(&self.key(plan, node, output, parameter), &super::store::Trace { value: hash.clone(), reads })?;
        }
        Ok(Given { value: Some(value), hash, kept: false })
    }
}
