//! The work graph: what the editor makes of a project's assets, as one lazy computation.
//!
//! An asset is a node. What it makes is its stack (`[[stack]]` in its declaration), read top to bottom in the order it
//! runs: a source on top - a file of its bundle, a constant, another node's output - then layers each taking the
//! result above and passing their own down: an operation with its settings, or a graph layer, an operation graph whose
//! input is the result above and whose result goes on down. Output layers are taps: each exposes the value at its level
//! as one of the node's outputs, under a name, and passes it on unchanged. A family of outputs is one output with a
//! parameter (`picture(look)`), each of its outputs addressed by the parameter's value (`picture/0`). A node's outputs
//! are its output layers, known without evaluating anything.
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

use toml::{Table, Value as Toml};

pub use pe2_ext::{Image, Imported};

/// What a port takes or an operation makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// A file: its name and its bytes.
    File,
    /// An asset's native form: its files, and the settings derived with them.
    Native,
    Image,
    Table,
    /// Whatever it is given.
    Any,
}

impl Type {
    /// Whether a value of this type may be given to a port taking `port`.
    #[must_use]
    pub fn fits(self, port: Type) -> bool {
        self == port || self == Type::Any || port == Type::Any
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Type::File => "file",
            Type::Native => "native",
            Type::Image => "image",
            Type::Table => "table",
            Type::Any => "any",
        }
    }
}

/// A value passed between operations, or given out as an output.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    File(File),
    Native(Imported),
    Image(Image),
    Table(Table),
}

impl Value {
    #[must_use]
    pub fn ty(&self) -> Type {
        match self {
            Value::File(_) => Type::File,
            Value::Native(_) => Type::Native,
            Value::Image(_) => Type::Image,
            Value::Table(_) => Type::Table,
        }
    }

    /// Its hash: of its type and its contents, a native form's files by their names and hashes.
    #[must_use]
    pub fn hash(&self) -> String {
        let sha1 = crate::extract::sha1_hex;
        let text = match self {
            Value::File(file) => format!("file {}\n{}", file.name, sha1(&file.bytes)),
            Value::Native(native) => {
                let mut text = String::from("native\n");
                for (name, bytes) in &native.files {
                    let _ = writeln!(text, "{name} {}", sha1(bytes));
                }
                let _ = writeln!(text, "{}", toml::to_string(&native.settings).unwrap_or_default());
                for note in &native.notes {
                    let _ = writeln!(text, "note {note}");
                }
                text
            }
            Value::Image(image) => format!("image {}x{}\n{}", image.width, image.height, sha1(&image.rgba)),
            Value::Table(table) => format!("table\n{}", toml::to_string(table).unwrap_or_default()),
        };
        sha1(text.as_bytes())
    }
}

/// A file as a value: what a bundle names it, and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// A port of an operation: what it takes, and whether it takes any number of values (a list joined in order).
#[derive(Clone, Debug)]
pub struct Port {
    pub name: String,
    pub ty: Type,
    pub many: bool,
}

impl Port {
    #[must_use]
    pub fn one(name: &str, ty: Type) -> Port {
        Port { name: name.to_string(), ty, many: false }
    }

    #[must_use]
    pub fn many(name: &str, ty: Type) -> Port {
        Port { name: name.to_string(), ty, many: true }
    }
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
    pub run: Option<Arc<Run>>,
}

impl OpDef {
    #[must_use]
    pub fn new(name: &str, label: &str, version: &str) -> OpDef {
        OpDef { name: name.to_string(), label: label.to_string(), version: version.to_string(), inputs: Vec::new(), output: None, run: None }
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
        ops.add(OpDef::new(FILE, "Stored file", "1").makes(Type::File, |call| {
            let name = call.setting_str("file")?.to_string();
            let bytes = call.file(&name)?;
            Ok(Value::File(File { name, bytes }))
        }));
        ops.add(OpDef::new(CONSTANT, "Constant", "1").makes(Type::Table, |call| {
            let value = call.settings().get("value").and_then(Toml::as_table).cloned();
            value.map(Value::Table).ok_or_else(|| anyhow::anyhow!("its `value` is not a table"))
        }));
        ops.add(OpDef::new(OUTPUT, "Output", "1").input(Port::one("value", Type::Any)));
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

/// How an output is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retain {
    /// In the project's store, by content.
    Cache,
    /// In the project's build directory.
    Build,
    /// Not at all: handed to whoever asked, its trace kept.
    Transient,
}

/// An operation of a plan: its type, its settings, and the steps each of its ports takes from.
#[derive(Clone, Debug)]
pub struct Step {
    pub op: String,
    pub settings: Table,
    pub inputs: Vec<Vec<usize>>,
    /// Where its node's declaration has it: `stack[1]`, `stack[2].ops.join`.
    pub at: String,
}

/// An output of a node.
#[derive(Clone, Debug)]
pub struct Output {
    pub name: String,
    /// Its parameter, for a family (`picture(look)`): each output addressed `<name>/<value>`.
    pub parameter: Option<String>,
    /// The step whose value it is.
    pub from: usize,
    pub retain: Retain,
    /// Made as soon as what it reads changes, rather than when asked for.
    pub eager: bool,
    pub at: String,
}

/// A stack lowered: its operations, each after those it takes from, and its outputs.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub outputs: Vec<Output>,
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
    pub fn of(declaration: &Table, ops: &Ops) -> (Plan, Vec<Problem>) {
        let mut lowering = Lowering { ops, plan: Plan::default(), problems: Vec::new() };
        match declaration.get("stack") {
            None => {}
            Some(Toml::Array(layers)) => lowering.stack(layers),
            Some(_) => lowering.problem("stack", "is not a list of layers"),
        }
        (lowering.plan, lowering.problems)
    }

    /// An output by its address: `native`, `picture/0` - with its parameter's value.
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
        let mut needed = vec![false; self.steps.len()];
        let mut todo: Vec<usize> = outputs.iter().map(|&o| self.outputs[o].from).collect();
        while let Some(s) = todo.pop() {
            if !std::mem::replace(&mut needed[s], true) {
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

    fn stack(&mut self, layers: &[Toml]) {
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
                        self.check_type(from, &def.inputs[0], &at);
                        vec![vec![from]]
                    } else {
                        self.problem(&at, &format!("`{op}` takes a value, and nothing is above it"));
                        continue;
                    };
                    self.plan.steps.push(Step { op: op.to_string(), settings: settings_of(layer, &["op"]), inputs, at });
                    above = Some(self.plan.steps.len() - 1);
                }
            }
        }
    }

    /// Whether the value of step `from` fits a port; a problem where it does not.
    fn check_type(&mut self, from: usize, port: &Port, at: &str) {
        let made = self.ops.get(&self.plan.steps[from].op).and_then(|d| d.output).unwrap_or(Type::Any);
        if !made.fits(port.ty) {
            self.problem(at, &format!("its `{}` takes {}, and is given {}", port.name, port.ty.name(), made.name()));
        }
    }

    fn output(&mut self, layer: &Table, from: usize, at: &str) {
        let Some(name) = layer.get("name").and_then(Toml::as_str) else {
            self.problem(at, "is an output without a name");
            return;
        };
        let (name, parameter) = match name.split_once('(') {
            Some((name, rest)) => {
                let Some(parameter) = rest.strip_suffix(')').filter(|p| !p.is_empty() && !p.contains(['(', ')', '/'])) else {
                    self.problem(at, &format!("`{name}`: a family's name is `name(parameter)`"));
                    return;
                };
                (name, Some(parameter.to_string()))
            }
            None => (name, None),
        };
        if name.is_empty() || name.contains('/') {
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
        self.plan.outputs.push(Output { name: name.to_string(), parameter, from, retain, eager, at: at.to_string() });
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
                self.check_type(step, port, at);
                steps.push(step);
            }
            inputs.push(steps);
        }
        if name == OUTPUT {
            self.output(op, inputs[0][0], at);
            return None;
        }
        self.plan.steps.push(Step { op: name.to_string(), settings: settings_of(op, &["op", "from"]), inputs, at: at.to_string() });
        Some(self.plan.steps.len() - 1)
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
