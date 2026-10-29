//! The work graph: what the editor makes of a project's assets, as one lazy computation.
//!
//! An asset is a node. What it makes is its stack (`[[stack]]` in its declaration), read top to bottom in the order it
//! runs: a source on top - a file of its bundle, a constant, another node's output - then layers each taking the
//! result above and passing their own down: an operation with its settings, or a graph layer, an operation graph whose
//! input is the result above and whose result goes on down. Output layers are taps: each exposes the value at its level
//! as one of the node's outputs, under a name, and passes it on unchanged. An output reaching an operation with a
//! parameter is a family of it, each of its outputs addressed by a value of the parameter (`picture/night`). A node's
//! outputs are its output layers and its kind's, known without evaluating anything.
//!
//! A stack is lowered to a plan - its operations in an order each comes after those it takes from - checked as it is:
//! an operation no extension provides, a source below the top, a value of one type given to a port of another, a cycle
//! in a graph layer, two outputs of one name are its node's problems before anything runs.
//!
//! Evaluating outputs runs only the operations they reach (`exec`). Inside a node every result is transient: one taken
//! once is moved to what takes it, one taken more is shared, read-only, until its last. Outputs are retained as their
//! layers say: `cache` keeps them in the project's store by content (`store`), `transient` hands them to whoever asked
//! and keeps only their trace - what they read, by hash - so a copy is known stale without the value being kept.

pub mod exec;
pub mod store;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::Context as _;
use toml::{Table, Value as Toml};

pub use pe2_ext::{Content, Data as Value, File, Image, Pcm, Port, Retain, Type, Typed};

/// A value's hash: of its type and its contents - a file's by its content's hash, inline or stored alike.
#[must_use]
pub fn hash(value: &Value) -> String {
    let sha1 = crate::extract::sha1_hex;
    let content = |c: &Content| match c {
        Content::Inline(bytes) => sha1(bytes),
        Content::Stored { sha1, .. } => sha1.clone(),
    };
    let text = match value {
        Value::Typed(typed) => {
            let mut text = format!("{} {:?}\n", typed.ty, typed.also);
            for file in &typed.files {
                let _ = writeln!(text, "{} {}", file.name, content(&file.content));
            }
            let _ = writeln!(text, "{}", toml::to_string(&typed.settings).unwrap_or_default());
            for note in &typed.notes {
                let _ = writeln!(text, "note {note}");
            }
            text
        }
        Value::Image(image) => format!("image {}x{}\n{}", image.width, image.height, sha1(&image.rgba)),
        Value::Table(table) => format!("table\n{}", toml::to_string(table).unwrap_or_default()),
        Value::Pcm(pcm) => {
            let bytes: Vec<u8> = pcm.samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            format!("pcm {} {}\n{}", pcm.rate, pcm.channels, sha1(&bytes))
        }
        Value::List(items) => items.iter().fold(String::from("list\n"), |mut text, item| {
            let _ = writeln!(text, "{}", hash(item));
            text
        }),
    };
    sha1(text.as_bytes())
}

/// A port taking one value of a type.
#[must_use]
pub fn port(name: &str, ty: Type) -> Port {
    Port { name: name.to_string(), ty, many: false }
}

/// A port taking any number of values of a type, in order.
#[must_use]
pub fn ports(name: &str, ty: Type) -> Port {
    Port { name: name.to_string(), ty, many: true }
}

/// What an operation does when it runs.
pub type Run = dyn Fn(&mut exec::Call<'_>) -> anyhow::Result<Value> + Send + Sync;

/// An operation type: its ports, what it makes, and how. One taking nothing is a source; one making nothing an output.
#[derive(Clone)]
pub struct OpDef {
    pub name: String,
    pub label: String,
    /// Its version: a new one makes again what it made.
    pub version: String,
    pub inputs: Vec<Port>,
    pub output: Option<Type>,
    /// Its parameter's name: what reaches it is a family of it.
    pub parameter: Option<String>,
    pub run: Option<Arc<Run>>,
}

impl OpDef {
    #[must_use]
    pub fn new(name: &str, label: &str, version: &str) -> OpDef {
        let (name, label, version) = (name.to_string(), label.to_string(), version.to_string());
        OpDef { name, label, version, inputs: Vec::new(), output: None, parameter: None, run: None }
    }

    /// With a parameter: what reaches it is a family of it.
    #[must_use]
    pub fn parameter(mut self, name: &str) -> OpDef {
        self.parameter = Some(name.to_string());
        self
    }

    #[must_use]
    pub fn input(mut self, port: Port) -> OpDef {
        self.inputs.push(port);
        self
    }

    #[must_use]
    pub fn makes(mut self, ty: Type, run: impl Fn(&mut exec::Call<'_>) -> anyhow::Result<Value> + Send + Sync + 'static) -> OpDef {
        self.output = Some(ty);
        self.run = Some(Arc::new(run));
        self
    }

    /// Whether it is a source: it takes nothing.
    #[must_use]
    pub fn is_source(&self) -> bool {
        self.inputs.is_empty()
    }
}

/// The built-in operations' names.
pub const FILE: &str = "file";
pub const CONSTANT: &str = "constant";
pub const OUTPUT: &str = "output";
pub const GRAPH: &str = "graph";
/// Other nodes' outputs, by the references a field of the node's settings holds: `field` (dotted), each item's `key`
/// where its items are tables, and the `output` taken of each.
pub const REFERENCES: &str = "references";
/// Files joined end to end into one, `name`d.
pub const CONCATENATE: &str = "concatenate";
/// The host's thumbnail: a picture made small (`side`, its longer side's pixels).
pub const THUMBNAIL: &str = "thumbnail";
/// The host's output keeping a node's result, where nothing in its stack keeps it and it is not a stored file.
pub const RESULT: &str = "result";
/// What a graph layer's operations name the result above it by.
pub const ABOVE: &str = "above";

/// The operation types a project's stacks are made of: the built-ins, and those its extensions provide.
#[derive(Clone)]
pub struct Ops {
    defs: HashMap<String, Arc<OpDef>>,
}

impl Default for Ops {
    fn default() -> Ops {
        let mut ops = Ops { defs: HashMap::new() };
        ops.add(OpDef::new(FILE, "Stored file", "1").makes(Type::File(Box::new(Type::Any)), |call| {
            let name = call.setting_str("file")?.to_string();
            call.stored(&name, Vec::new()).map(Value::Typed)
        }));
        ops.add(OpDef::new(CONSTANT, "Constant", "1").makes(Type::Table, |call| {
            let value = call.settings().get("value").and_then(Toml::as_table).cloned();
            value.map(Value::Table).ok_or_else(|| anyhow::anyhow!("its `value` is not a table"))
        }));
        ops.add(OpDef::new(OUTPUT, "Output", "1").input(port("value", Type::Any)));
        ops.add(OpDef::new(REFERENCES, "Other nodes' outputs", "1").makes(Type::List(Box::new(Type::Any)), |call| {
            let field = call.setting_str("field")?;
            let key = call.settings().get("key").and_then(Toml::as_str);
            let address = call.setting_str("output")?;
            let mut at = call.fields().get(field.split('.').next().unwrap_or_default());
            for part in field.split('.').skip(1) {
                at = at.and_then(|v| v.get(part));
            }
            let items: Vec<&Toml> = match at {
                Some(Toml::Table(table)) => table.values().collect(),
                Some(Toml::Array(array)) => array.iter().collect(),
                Some(_) => anyhow::bail!("its `{field}` is no list"),
                None => Vec::new(),
            };
            let outputs = items.into_iter().enumerate().map(|(i, item)| {
                let reference = match key {
                    Some(key) => item.get(key).with_context(|| format!("{field}'s item {i} has no `{key}`"))?,
                    None => item,
                };
                call.output_of(reference, address).map(|value| (*value).clone())
            });
            outputs.collect::<anyhow::Result<_>>().map(Value::List)
        }));
        ops.add(OpDef::new(CONCATENATE, "Concatenate", "1").input(port("files", Type::List(Box::new(Type::Any)))).makes(
            Type::Bytes,
            |call| {
                let name = call.setting_str("name")?;
                let Value::List(items) = call.take(0)?.take() else { anyhow::bail!("given no list") };
                let mut writer = call.writer(name);
                for (i, item) in items.iter().enumerate() {
                    let file = match item {
                        Value::Typed(typed) if typed.files.len() == 1 => &typed.files[0],
                        other => anyhow::bail!("its item {i} is {}, not one file", other.ty()),
                    };
                    call.copy(&file.content, &mut writer)?;
                }
                let file = writer.finish()?;
                Ok(Value::Typed(Typed { ty: pe2_ext::data::BYTES.to_string(), files: vec![file], ..Typed::default() }))
            },
        ));
        ops.add(OpDef::new(THUMBNAIL, "Thumbnail", "1").input(port("picture", Type::Image)).makes(Type::Image, |call| {
            let side = call.settings().get("side").and_then(Toml::as_integer).and_then(|s| usize::try_from(s).ok()).unwrap_or(32);
            match call.take(0)?.take() {
                Value::Image(image) => Ok(Value::Image(image.thumbnail(side))),
                other => anyhow::bail!("given {}, not an image", other.ty()),
            }
        }));
        ops
    }
}

impl Ops {
    pub fn add(&mut self, def: OpDef) {
        self.defs.insert(def.name.clone(), Arc::new(def));
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<OpDef>> {
        self.defs.get(name)
    }
}

/// An operation of a plan: its type, its settings, and the steps each of its ports takes from.
#[derive(Clone, Debug)]
pub struct Step {
    pub op: String,
    pub settings: Table,
    pub inputs: Vec<Vec<usize>>,
    /// What it makes, its operation's variables bound by what it takes.
    pub ty: Type,
    /// Where its node's declaration has it: `stack[1]`, `stack[2].ops.join`.
    pub at: String,
}

/// An output of a node.
#[derive(Clone, Debug)]
pub struct Output {
    pub name: String,
    /// Its parameter, for a family: each output addressed `<name>/<value>`.
    pub parameter: Option<String>,
    /// One output, though what it reaches has a parameter: its value supplied by whoever asks (the thumbnail, of
    /// the first look: `Engine::output_with`).
    pub fixed: bool,
    /// The step whose value it is.
    pub from: usize,
    pub retain: Retain,
    /// Made as soon as what it reads changes, rather than when asked for.
    pub eager: bool,
    pub at: String,
}

/// A stack lowered: its operations, each after those it takes from, its outputs, and its result - the step its stack
/// ends with, which its kind's outputs take, and which is the node as its readers see it.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub outputs: Vec<Output>,
    pub result: Option<usize>,
}

/// An output a node's kind adds after its stack: made by an operation from the stack's result - or from what another
/// of the kind's outputs is made of (`from`, its index among them) - with settings of its own.
#[derive(Clone, Debug)]
pub struct Tail {
    pub name: String,
    /// One output rather than a family, the value of the parameter it reaches supplied by whoever asks.
    pub fixed: bool,
    pub op: String,
    pub retain: Retain,
    pub from: Option<usize>,
    pub settings: Table,
}

/// How a retention is written.
#[must_use]
pub fn retain_name(retain: Retain) -> &'static str {
    match retain {
        Retain::Cache => "cache",
        Retain::Build => "build",
        Retain::Transient => "transient",
    }
}

/// What is wrong with a stack, where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub at: String,
    pub what: String,
}

impl Plan {
    /// A declaration's stack lowered and checked: the plan, and what is wrong with it. A plan with problems still has
    /// the steps and outputs that are sound, so a node keeps what it can make.
    #[must_use]
    pub fn of(declaration: &Table, ops: &Ops, tail: &[Tail]) -> (Plan, Vec<Problem>) {
        let mut lowering = Lowering { ops, plan: Plan::default(), problems: Vec::new() };
        let result = match declaration.get("stack") {
            None => None,
            Some(Toml::Array(layers)) => lowering.stack(layers),
            Some(_) => {
                lowering.problem("stack", "is not a list of layers");
                None
            }
        };
        lowering.plan.result = result;
        // The node as its readers see it is its result: kept - by the stack where an output keeps it, else by the
        // host - unless it is a stored file, which its readers see as it is, or a family (one asset is no family).
        if let Some(result) = result
            && lowering.plan.steps[result].op != FILE
            && lowering.plan.result_output().is_none()
            && !lowering.reaches_parameter(result)
        {
            let mut layer = Table::new();
            layer.insert("name".into(), RESULT.into());
            layer.insert("retain".into(), retain_name(Retain::Cache).into());
            layer.insert("when".into(), "eager".into());
            lowering.output(&layer, result, "stack");
        }
        // The kind's outputs, after the stack: a node without a result has none of them.
        let mut made: Vec<Option<usize>> = Vec::with_capacity(tail.len());
        for (i, output) in tail.iter().enumerate() {
            let at = format!("kind.outputs[{i}]");
            let from = output.from.map_or(result, |f| made.get(f).copied().flatten());
            let step = match (from, ops.get(&output.op).cloned()) {
                (Some(from), Some(def)) => Some(lowering.step(&def, &output.op, output.settings.clone(), vec![vec![from]], &at)),
                (Some(_), None) => {
                    lowering.problem(&at, &format!("`{}`: no extension provides it", output.op));
                    None
                }
                (None, _) => None,
            };
            if let Some(step) = step {
                let mut layer = Table::new();
                layer.insert("name".into(), output.name.clone().into());
                if output.fixed {
                    layer.insert("fixed".into(), true.into());
                }
                layer.insert("retain".into(), retain_name(output.retain).into());
                lowering.output(&layer, step, &at);
            }
            made.push(step);
        }
        lowering.families();
        (lowering.plan, lowering.problems)
    }

    /// The output keeping its result in the store: one of its stack's, or the host's (`RESULT`).
    #[must_use]
    pub fn result_output(&self) -> Option<usize> {
        let result = self.result?;
        self.outputs.iter().position(|o| o.from == result && o.parameter.is_none() && o.retain == Retain::Cache)
    }

    /// Whether its result is a stored file of its node, which readers see as it is.
    #[must_use]
    pub fn result_stored(&self) -> bool {
        self.result.is_some_and(|r| self.steps[r].op == FILE)
    }

    /// An output by its address: `native`, `picture/night` - with its parameter's value.
    #[must_use]
    pub fn output(&self, address: &str) -> Option<(usize, Option<String>)> {
        if let Some(i) = self.outputs.iter().position(|o| o.parameter.is_none() && o.name == address) {
            return Some((i, None));
        }
        let (name, value) = address.split_once('/')?;
        let i = self.outputs.iter().position(|o| o.parameter.is_some() && o.name == name)?;
        Some((i, Some(value.to_string())))
    }

    /// The steps an output reaches, in order.
    #[must_use]
    pub fn reached(&self, outputs: &[usize]) -> Vec<bool> {
        self.reached_until(outputs, &std::collections::HashSet::new())
    }

    /// The steps an output reaches, not going past those `known` (made already).
    #[must_use]
    pub fn reached_until(&self, outputs: &[usize], known: &std::collections::HashSet<usize>) -> Vec<bool> {
        let mut needed = vec![false; self.steps.len()];
        let mut todo: Vec<usize> = outputs.iter().map(|&o| self.outputs[o].from).collect();
        while let Some(s) = todo.pop() {
            if !std::mem::replace(&mut needed[s], true) && !known.contains(&s) {
                todo.extend(self.steps[s].inputs.iter().flatten());
            }
        }
        needed
    }

    /// What an output is made by, as text to hash: the steps it reaches - their operations and those' versions, their
    /// settings, what each takes from - and the output itself.
    #[must_use]
    pub fn describe(&self, output: usize, ops: &Ops) -> String {
        let needed = self.reached(&[output]);
        let mut text = String::new();
        for (i, step) in self.steps.iter().enumerate().filter(|(i, _)| needed[*i]) {
            let version = ops.get(&step.op).map_or("?", |d| d.version.as_str());
            let settings = toml::to_string(&step.settings).unwrap_or_default();
            let _ = writeln!(text, "{i} {}@{version} {:?}\n{settings}", step.op, step.inputs);
        }
        let output = &self.outputs[output];
        let _ = writeln!(text, "output {} {:?} from {}", output.name, output.parameter, output.from);
        text
    }
}

struct Lowering<'a> {
    ops: &'a Ops,
    plan: Plan,
    problems: Vec<Problem>,
}

/// A layer's or graph operation's settings: its table without the keys that place it.
fn settings_of(table: &Table, placing: &[&str]) -> Table {
    table.iter().filter(|(k, _)| !placing.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect()
}

impl Lowering<'_> {
    fn problem(&mut self, at: &str, what: &str) {
        self.problems.push(Problem { at: at.to_string(), what: what.to_string() });
    }

    /// A step of an operation taking from `inputs` (by port): its types checked, its variables bound by what it is
    /// given; its index.
    fn step(&mut self, def: &OpDef, op: &str, settings: Table, inputs: Vec<Vec<usize>>, at: &str) -> usize {
        let mut bound = Vec::new();
        for (port, from) in def.inputs.iter().zip(&inputs) {
            for &from in from {
                let made = self.plan.steps[from].ty.clone();
                if !port.ty.unify(&made, &mut bound) {
                    self.problem(at, &format!("its `{}` takes {}, and is given {made}", port.name, port.ty));
                }
            }
        }
        let ty = def.output.as_ref().map_or(Type::Any, |t| t.bind(&bound));
        self.plan.steps.push(Step { op: op.to_string(), settings, inputs, ty, at: at.to_string() });
        self.plan.steps.len() - 1
    }

    /// A stack's layers: the step of its result, where it has one.
    fn stack(&mut self, layers: &[Toml]) -> Option<usize> {
        let mut above: Option<usize> = None;
        for (i, layer) in layers.iter().enumerate() {
            let at = format!("stack[{i}]");
            let Some(layer) = layer.as_table() else {
                self.problem(&at, "is not a table");
                continue;
            };
            let Some(op) = layer.get("op").and_then(Toml::as_str) else {
                self.problem(&at, "names no operation (`op`)");
                continue;
            };
            match op {
                OUTPUT => {
                    if let Some(from) = above {
                        self.output(layer, from, &at);
                    } else {
                        self.problem(&at, "is an output with nothing above it");
                    }
                }
                GRAPH => above = self.graph(layer, above, &at),
                _ => {
                    let Some(def) = self.ops.get(op).cloned() else {
                        self.problem(&at, &format!("`{op}`: no extension provides it"));
                        above = None;
                        continue;
                    };
                    let inputs = if def.is_source() {
                        if above.is_some() {
                            self.problem(&at, &format!("`{op}` is a source below the top: it would drop what is above it"));
                        }
                        Vec::new()
                    } else if def.inputs.len() > 1 {
                        self.problem(&at, &format!("`{op}` takes {} inputs: it belongs in a graph layer", def.inputs.len()));
                        above = None;
                        continue;
                    } else if let Some(from) = above {
                        vec![vec![from]]
                    } else {
                        self.problem(&at, &format!("`{op}` takes a value, and nothing is above it"));
                        continue;
                    };
                    above = Some(self.step(&def, op, settings_of(layer, &["op"]), inputs, &at));
                }
            }
        }
        above
    }

    /// Whether a step reaches an operation with a parameter: what it makes is a family.
    fn reaches_parameter(&self, step: usize) -> bool {
        let mut todo = vec![step];
        let mut seen = vec![false; self.plan.steps.len()];
        while let Some(s) = todo.pop() {
            if std::mem::replace(&mut seen[s], true) {
                continue;
            }
            if self.ops.get(&self.plan.steps[s].op).is_some_and(|d| d.parameter.is_some()) {
                return true;
            }
            todo.extend(self.plan.steps[s].inputs.iter().flatten());
        }
        false
    }

    /// Each output a family of the parameter the operations it reaches have: one at most - two are a problem.
    fn families(&mut self) {
        let families: Vec<usize> = (0..self.plan.outputs.len()).filter(|o| !self.plan.outputs[*o].fixed).collect();
        for o in families {
            let reached = self.plan.reached(&[o]);
            let mut parameters: Vec<&String> = (0..self.plan.steps.len())
                .filter(|s| reached[*s])
                .filter_map(|s| self.ops.get(&self.plan.steps[s].op)?.parameter.as_ref())
                .collect();
            parameters.sort_unstable();
            parameters.dedup();
            let output = &self.plan.outputs[o];
            match parameters.as_slice() {
                [] => {}
                [parameter] => self.plan.outputs[o].parameter = Some((*parameter).clone()),
                many => {
                    let (at, name) = (output.at.clone(), output.name.clone());
                    let named: Vec<String> = many.iter().map(|p| format!("`{p}`")).collect();
                    self.problem(&at, &format!("`{name}` reaches operations of {}: an output is one family", named.join(" and ")));
                }
            }
        }
    }

    fn output(&mut self, layer: &Table, from: usize, at: &str) {
        let Some(name) = layer.get("name").and_then(Toml::as_str) else {
            self.problem(at, "is an output without a name");
            return;
        };
        if name.is_empty() || name.contains(['/', '(', ')']) {
            self.problem(at, &format!("`{name}` is not an output's name"));
            return;
        }
        if self.plan.outputs.iter().any(|o| o.name == name) {
            self.problem(at, &format!("a second output named `{name}`"));
            return;
        }
        let retain = match layer.get("retain").and_then(Toml::as_str) {
            None | Some("transient") => Retain::Transient,
            Some("cache") => Retain::Cache,
            Some("build") => Retain::Build,
            Some(other) => {
                self.problem(at, &format!("`retain = \"{other}\"`: it is `cache`, `build` or `transient`"));
                Retain::Transient
            }
        };
        let eager = match layer.get("when").and_then(Toml::as_str) {
            None | Some("lazy") => false,
            Some("eager") => true,
            Some(other) => {
                self.problem(at, &format!("`when = \"{other}\"`: it is `eager` or `lazy`"));
                false
            }
        };
        // A family where what it reaches has a parameter: found once it is lowered (`families`).
        let fixed = layer.get("fixed").and_then(Toml::as_bool).unwrap_or(false);
        self.plan.outputs.push(Output { name: name.to_string(), parameter: None, fixed, from, retain, eager, at: at.to_string() });
    }

    /// A graph layer's operations, after those they take from (the layer above's result as `above`): its result's
    /// step, where it has a sound one.
    fn graph(&mut self, layer: &Table, above: Option<usize>, at: &str) -> Option<usize> {
        let empty = Table::new();
        let ops = match layer.get("ops") {
            None => &empty,
            Some(Toml::Table(ops)) => ops,
            Some(_) => {
                self.problem(at, "its `ops` is not a table of operations");
                return None;
            }
        };
        // Each operation's inputs by port, as ids.
        let mut wiring: BTreeMap<&str, (&Table, Vec<Vec<&str>>)> = BTreeMap::new();
        for (id, op) in ops {
            let here = format!("{at}.ops.{id}");
            let Some(op) = op.as_table() else {
                self.problem(&here, "is not a table");
                continue;
            };
            let from = op.get("from").and_then(Toml::as_table);
            let name = op.get("op").and_then(Toml::as_str).unwrap_or_default();
            let ports = self.ops.get(name).map_or_default(|d| d.inputs.clone());
            let ins = ports
                .iter()
                .map(|port| match from.and_then(|f| f.get(&port.name)) {
                    Some(Toml::String(id)) => vec![id.as_str()],
                    Some(Toml::Array(ids)) => ids.iter().filter_map(Toml::as_str).collect(),
                    _ => Vec::new(),
                })
                .collect();
            wiring.insert(id, (op, ins));
        }
        // Kahn's order: each operation once all it takes from are placed; what is left is in a cycle.
        let mut placed: HashMap<&str, usize> = HashMap::new();
        let mut left: Vec<&str> = wiring.keys().copied().collect();
        loop {
            let ready = left.iter().position(|id| {
                wiring[id].1.iter().flatten().all(|from| *from == ABOVE || placed.contains_key(from) || !wiring.contains_key(from))
            });
            let Some(i) = ready else { break };
            let id = left.remove(i);
            let (op, ins) = &wiring[id];
            let here = format!("{at}.ops.{id}");
            if let Some(step) = self.graph_op(op, ins, &placed, above, &here) {
                placed.insert(id, step);
            }
        }
        for id in &left {
            self.problem(&format!("{at}.ops.{id}"), "is in a cycle: it takes, through others, from itself");
        }
        match layer.get("result").and_then(Toml::as_str) {
            Some(ABOVE) => above,
            Some(id) => {
                let step = placed.get(id).copied();
                if step.is_none() && !left.contains(&id) && !wiring.contains_key(id) {
                    self.problem(at, &format!("its result `{id}` is none of its operations"));
                }
                step
            }
            None => {
                self.problem(at, "names no result");
                None
            }
        }
    }

    fn graph_op(&mut self, op: &Table, ins: &[Vec<&str>], placed: &HashMap<&str, usize>, above: Option<usize>, at: &str) -> Option<usize> {
        let Some(name) = op.get("op").and_then(Toml::as_str) else {
            self.problem(at, "names no operation (`op`)");
            return None;
        };
        if name == GRAPH {
            self.problem(at, "is a graph inside a graph");
            return None;
        }
        let Some(def) = self.ops.get(name).cloned() else {
            self.problem(at, &format!("`{name}`: no extension provides it"));
            return None;
        };
        let mut inputs = Vec::new();
        for (port, ids) in def.inputs.iter().zip(ins) {
            if !port.many && ids.len() != 1 {
                self.problem(at, &format!("its `{}` takes one value, and is given {}", port.name, ids.len()));
                return None;
            }
            let mut steps = Vec::new();
            for id in ids {
                let step = if *id == ABOVE { above } else { placed.get(id).copied() };
                let Some(step) = step else {
                    self.problem(at, &format!("its `{}` takes from `{id}`, which makes nothing", port.name));
                    return None;
                };
                steps.push(step);
            }
            inputs.push(steps);
        }
        if name == OUTPUT {
            self.output(op, inputs[0][0], at);
            return None;
        }
        Some(self.step(&def, name, settings_of(op, &["op", "from"]), inputs, at))
    }
}

/// A stack's import, as what read `[import]` before stacks reads it: the stored file on top - its source - and the
/// operation below it, with that operation's settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Import<'a> {
    pub source: &'a str,
    pub op: &'a str,
    pub settings: Table,
}

/// A declaration's import, where its stack begins with a stored file and an operation taking it.
#[must_use]
pub fn import_of(fields: &Table) -> Option<Import<'_>> {
    let stack = fields.get("stack")?.as_array()?;
    let top = stack.first()?.as_table()?;
    let source = top.get("file")?.as_str().filter(|_| top.get("op").and_then(Toml::as_str) == Some(FILE))?;
    let below = stack.get(1)?.as_table()?;
    let op = below.get("op")?.as_str().filter(|op| ![FILE, CONSTANT, OUTPUT, GRAPH].contains(op))?;
    Some(Import { source, op, settings: settings_of(below, &["op"]) })
}

/// A declaration's fields as an importer reads them: with `[import]` - its importer, its source and its settings -
/// as the stack gives them.
#[must_use]
pub fn with_import(fields: &Table, import: &Import<'_>) -> Table {
    let mut fields = fields.clone();
    let mut section = import.settings.clone();
    section.insert("importer".into(), import.op.into());
    section.insert("source".into(), import.source.into());
    fields.insert("import".into(), Toml::Table(section));
    fields
}

#[cfg(test)]
mod tests;
