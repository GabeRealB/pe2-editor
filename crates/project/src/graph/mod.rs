//! The work graph: what the editor makes of a project's assets, as one lazy computation.
//!
//! An asset is a node. What it makes is its stack (`[[stack]]` in its declaration), read top to bottom in the order it
//! runs: a source on top - a file of its bundle, a constant, another node's output - then layers each taking the
//! result above and passing their own down: an operation with its settings, or a graph layer, an operation graph whose
//! input is the result above and whose result goes on down. Output layers are taps: each exposes the value at its level
//! as one of the node's outputs, under a name, and passes it on unchanged. An output reaching an operation with a
//! parameter is a family of it, each of its outputs addressed by a value of the parameter (`picture/night`). A node's
//! outputs are its output layers and its type's, known without evaluating anything.
//!
//! A stack is lowered to a plan - its operations in an order each comes after those it takes from - checked as it is:
//! an operation no extension provides, a source below the top, a value of one type given to a port of another, a cycle
//! in a graph layer, two outputs of one name are its node's problems before anything runs.
//!
//! An extension's operation reads of other nodes only what it said it reads, before it ran (`OpDef::prepare`): that is made
//! first, with none of its instances held, so that what reads what reads what holds one instance however far it goes on. It
//! can be asked again for what the last read found, and it can split into micro ops: others of its extension's operations,
//! run in its place, each known by what it takes and read, each saying what it reads as an operation does. Those are what a
//! layer's work is kept and shared in, as a stack only knows its layers. The host's own operations run nothing to hold.
//!
//! Evaluating outputs runs only the operations they reach (`exec`). Inside a node every result is transient: one taken
//! once is moved to what takes it, one taken more is shared, read-only, until its last. Outputs are retained as their
//! layers say: `cache` keeps them in the project's store by content (`store`), `transient` lasts a run - an epoch, an attach -
//! for those reading it to share, while what it read holds, and otherwise goes to whoever asked; only its trace is kept,
//! what it read by hash, so a copy is known stale without the value being kept.

pub mod exec;
pub mod layers;
pub mod store;
pub mod stream;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::Context as _;
use toml::{Table, Value as Toml};

use crate::events::Expr;

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
        Value::Video(frame) => format!(
            "video {} {}x{} {}\n{}",
            frame.format.name(),
            frame.width,
            frame.height,
            frame.timestamp,
            sha1(frame.buffer.bytes())
        ),
        Value::Audio(sound) => format!(
            "audio {} {} {} {}\n{}",
            sound.format.name(),
            sound.sample_rate,
            sound.channels,
            sound.timestamp,
            sha1(sound.buffer.bytes())
        ),
        Value::Marks(marks) => format!("marks\n{marks:?}"),
        Value::Stream(stream) => format!("stream\n{}", toml::to_string(&stream.header()).unwrap_or_default()),
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

/// What an operation makes, as its settings say.
pub type OutputOf = dyn Fn(&Table) -> Option<Type> + Send + Sync;

/// What says what an operation will read before it runs.
pub type Prepare = dyn Fn(&mut exec::Call<'_>) -> anyhow::Result<pe2_ext::Prepared> + Send + Sync;

/// What a stream operation does when it is opened: starts what makes its items.
pub type Start = dyn Fn(&mut exec::Call<'_>) -> anyhow::Result<Box<dyn stream::Producer>> + Send + Sync;

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
    /// What it will read, said before it runs.
    pub prepare: Option<Arc<Prepare>>,
    /// Whether it runs in steps (`pe2_ext::stepped`): what it was given is kept for its steps after the first.
    pub stepped: bool,
    /// An extension's, which reads of other nodes only what it said: what reads otherwise holds an instance while what it
    /// reads is made. The host's own run nothing to hold.
    pub strict: bool,
    /// For a stream operation: how it is opened.
    pub start: Option<Arc<Start>>,
    /// What it makes as its settings say, where they say: what a stored file's name or a declared type tells of it
    /// before anything runs, in place of `output`.
    pub output_of: Option<Arc<OutputOf>>,
    /// Which comes first of those doing the same.
    pub priority: i32,
    /// It makes what is set on it, as it is: what a node holding only data is.
    pub data: bool,
    /// What a layer of it has to set: nothing stands in for these.
    pub required: Vec<String>,
    /// Its outputs after the first, by name: with any, what it makes is a list of them all, in order.
    pub also: Vec<(String, Type)>,
}

impl OpDef {
    #[must_use]
    pub fn new(name: &str, label: &str, version: &str) -> OpDef {
        let (name, label, version) = (name.to_string(), label.to_string(), version.to_string());
        OpDef {
            name,
            label,
            version,
            inputs: Vec::new(),
            output: None,
            parameter: None,
            run: None,
            prepare: None,
            stepped: false,
            strict: false,
            start: None,
            output_of: None,
            priority: 0,
            data: false,
            required: Vec::new(),
            also: Vec::new(),
        }
    }

    /// What it makes by its settings, where they say more than `output`.
    #[must_use]
    pub fn output_of(mut self, output_of: impl Fn(&Table) -> Option<Type> + Send + Sync + 'static) -> OpDef {
        self.output_of = Some(Arc::new(output_of));
        self
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

    /// A stream operation: it makes a stream of `item`s, opened by starting what `start` makes.
    #[must_use]
    pub fn streams(
        mut self,
        item: Type,
        start: impl Fn(&mut exec::Call<'_>) -> anyhow::Result<Box<dyn stream::Producer>> + Send + Sync + 'static,
    ) -> OpDef {
        self.output = Some(Type::Stream(Box::new(item)));
        self.start = Some(Arc::new(start));
        self
    }

    #[must_use]
    pub fn makes(
        mut self,
        ty: Type,
        run: impl Fn(&mut exec::Call<'_>) -> anyhow::Result<Value> + Send + Sync + 'static,
    ) -> OpDef {
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
/// The version of a node the layer below has: an output of it (`output`, its `native` where unsaid), read as another
/// node's, of the `type` it declares (else any, told when it runs). What an override keeps of what it replaces need not
/// be copied: an import of a movie reads the STR it replaces.
pub const BELOW: &str = "below";
pub const CONSTANT: &str = "constant";
/// What a node says of itself, whatever its stack: its output of this name, for others to read.
pub const SETTINGS: &str = "settings";
pub const OUTPUT: &str = "output";
/// What comes down to it shown, by a viewer of what it is or through the operation that makes it what one shows: a tap
/// as `output` is, taking part in no build.
pub const PREVIEW: &str = "preview";
/// One of the outputs of an operation with several: what a wire to it comes to.
pub const PICK: &str = "pick";
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
/// What any step may take besides its operation's ports: whether it runs (a boolean, true where it takes nothing). One
/// that does not makes nothing - nor does what takes from it, but `merge`.
pub const ACTIVE: &str = "active";
/// The first of what it takes that is made: what takes branches together again.
pub const MERGE: &str = "merge";
/// The config being built, as a table: its `release`, `target` and `properties`.
pub const CONFIG: &str = "config";
/// Whether what it takes (at `field`, dotted, of a table) is `to`.
pub const EQUALS: &str = "equals";
/// Booleans, all true; any true; and one turned around. Named apart from the event gates, which take signals.
pub const ALL: &str = "all";
pub const ANY: &str = "any";
pub const INVERT: &str = "invert";
/// Another node's output, chosen by a key: what it takes (at `field`) names one of its `of` (key to reference), else
/// its `default`'s, else nothing. Only the one chosen is read.
pub const SELECT: &str = "select";
/// A table over another, key by key, nested tables by theirs (at `field`, dotted, where only that part is).
pub const OVERLAY: &str = "overlay";
/// What `active` takes, and the logic operations make.
pub const BOOL_TYPE: &str = "bool";
/// An operation the project defines' boxes, inside it: what it takes (`name`, `type`, `many`, `index`), what it is set
/// to (`name`, `type` or `values`, `default`, `label`, `index`: a property's shape), and - `output` - what it makes.
pub const INPUT: &str = "input";
pub const SETTING: &str = "setting";
/// What stands for a value given from outside a plan: an operation's input, laid out alone (`Plan::of_definition`).
pub const GIVEN: &str = "given";
/// An operation the project defines (`body`, by uid) run for each item of a list - or of a table, each as its `key` and
/// `value` - given too what `with` takes, each item made once for what it takes; what it makes of them, a list, those
/// it makes nothing of left out.
pub const FOR_EACH: &str = "for_each";

/// A boolean, as the graph passes one.
#[must_use]
pub fn boolean(value: bool) -> Value {
    let settings = Table::from_iter([("value".to_string(), Toml::Boolean(value))]);
    Value::Typed(Typed { ty: BOOL_TYPE.to_string(), settings, ..Typed::default() })
}

/// A boolean passed, where it is one.
#[must_use]
pub fn truth(value: &Value) -> Option<bool> {
    match value {
        Value::Typed(typed) if typed.ty == BOOL_TYPE => typed.settings.get("value")?.as_bool(),
        _ => None,
    }
}

/// What an operation fails with to make nothing, as an inactive step does: `select` naming no node.
#[derive(Debug)]
pub struct Absent;

impl std::fmt::Display for Absent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("nothing is made")
    }
}

impl std::error::Error for Absent {}

/// An event as a box: it holds while its `event` is signaled. What it is the value of is no value: only a gate, a pull or a listen takes it.
pub const EVENT: &str = "event";
/// Signal an `event` where what it takes holds: a box that makes nothing, as an output does.
pub const SIGNAL: &str = "signal";
/// What event boxes pass along.
pub const SIGNAL_TYPE: &str = "pe2.events@signal";

/// An event gate: a truth table.
#[derive(Debug)]
pub struct Gate {
    pub name: &'static str,
    pub label: &'static str,
    pub table: fn(&[bool]) -> bool,
}

impl PartialEq for Gate {
    fn eq(&self, other: &Gate) -> bool {
        self.name == other.name
    }
}

impl Eq for Gate {}

impl Gate {
    /// Whether a further input turning on can only ever leave it true, so that it may be told before its inputs have settled. Found from its table.
    #[must_use]
    pub fn is_monotone(&self) -> bool {
        let of = |bits: u8| [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0];
        (0..8u8).all(|low| {
            (0..8u8).filter(|high| low & !high == 0).all(|high| !(self.table)(&of(low)) || (self.table)(&of(high)))
        })
    }
}

/// The gates. What is left of `not` and the like with no input on would be true of every epoch, so a trigger must not be (`Plan::check_trigger`).
pub const GATES: &[Gate] = &[
    Gate { name: "and", label: "And", table: |inputs| inputs.iter().all(|i| *i) },
    Gate { name: "or", label: "Or", table: |inputs| inputs.iter().any(|i| *i) },
    Gate { name: "not", label: "Not", table: |inputs| !inputs.first().copied().unwrap_or(false) },
    Gate { name: "xor", label: "Xor", table: |inputs| inputs.iter().filter(|i| **i).count() % 2 == 1 },
    Gate { name: "nand", label: "Nand", table: |inputs| !inputs.iter().all(|i| *i) },
    Gate { name: "nor", label: "Nor", table: |inputs| !inputs.iter().any(|i| *i) },
    Gate { name: "xnor", label: "Xnor", table: |inputs| inputs.iter().filter(|i| **i).count() % 2 == 0 },
];

/// The operation types a project's stacks are made of: the built-ins, and those its extensions provide.
#[derive(Clone)]
pub struct Ops {
    defs: HashMap<String, Arc<OpDef>>,
    /// What a viewer takes: the host's own, and those extensions register.
    pub viewers: Arc<Vec<Type>>,
    /// The operations the project defines, by uid (`uid://...`): laid into each stack using one.
    pub definitions: Arc<Definitions>,
}

/// What a project defines an operation as: a node of this type (`pe2.graph@operation`).
pub const DEFINED: &str = "pe2.graph@operation";
/// What a project defines a template as.
pub const DEFINED_TEMPLATE: &str = "pe2.graph@template";

/// The operations a project defines, by uid.
pub type Definitions = HashMap<String, Arc<Definition>>;

/// An operation a project defines: a graph of others - laid into each stack naming it by its node's uid, as a graph
/// layer is - whose boxes say what it is from outside: each `input` it takes, each `setting` it is set to (read inside it
/// by the box's value, or where a value is `{ setting = "<name>" }`, its default where the layer does not set it) and
/// each `output` it makes, the first its result.
#[derive(Clone, Debug)]
pub struct Definition {
    pub uid: String,
    pub label: String,
    /// Its declaration's hash: what tells it changed.
    pub version: String,
    /// What it takes, in order, each with the box that stands for it inside.
    pub inputs: Vec<Port>,
    pub input_boxes: Vec<String>,
    /// What it is set to: each a property's shape, default and label (`crate::Property`), with its box.
    pub settings: Vec<(crate::Property, String)>,
    /// What it makes, by name, in order - the first its result - each with its box.
    pub outputs: Vec<(String, String)>,
    pub ops: Table,
}

/// Its boxes of a kind, in the order of their `index`, else of their ids: each id and table.
fn boxes<'a>(ops: &'a Table, kind: &str) -> Vec<(&'a String, &'a Table)> {
    let mut found: Vec<(i64, &String, &Table)> = ops
        .iter()
        .filter_map(|(id, op)| Some((id, op.as_table()?)))
        .filter(|(_, op)| op.get("op").and_then(Toml::as_str) == Some(kind))
        .map(|(id, op)| (op.get("index").and_then(Toml::as_integer).unwrap_or(i64::MAX), id, op))
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    found.into_iter().map(|(_, id, op)| (id, op)).collect()
}

impl Definition {
    /// A node's definition, from its declaration's layer that is one: its boxes.
    ///
    /// # Errors
    /// It holds no such layer, it makes nothing, or a box is not one: two of a name, a type or setting not as one is
    /// written.
    pub fn of(uid: &str, fields: &Table, version: &str) -> Result<Definition, String> {
        let layers = fields.get("stack").and_then(Toml::as_array).into_iter().flatten();
        let layer =
            layers.filter_map(Toml::as_table).find(|layer| layer.get("op").and_then(Toml::as_str) == Some(DEFINED));
        let layer = layer.ok_or("it defines no operation")?;
        let ops = layer.get("ops").and_then(Toml::as_table).cloned().unwrap_or_default();
        let name_of = |id: &String, op: &Table| op.get("name").and_then(Toml::as_str).unwrap_or(id).to_string();
        let mut seen = HashSet::new();
        let mut once = |kind: &str, name: &str| {
            if seen.insert((kind.to_string(), name.to_string())) {
                Ok(())
            } else {
                Err(format!("it has two {kind}s `{name}`"))
            }
        };
        let (mut inputs, mut input_boxes) = (Vec::new(), Vec::new());
        for (id, op) in boxes(&ops, INPUT) {
            let name = name_of(id, op);
            once(INPUT, &name)?;
            let ty = op.get("type").and_then(Toml::as_str).unwrap_or("any");
            let ty = Type::parse(ty, "").map_err(|e| format!("its input `{name}`: {e}"))?;
            let many = op.get("many").and_then(Toml::as_bool).unwrap_or(false);
            inputs.push(Port { name, ty, many });
            input_boxes.push(id.clone());
        }
        let mut settings = Vec::new();
        for (id, op) in boxes(&ops, SETTING) {
            let name = name_of(id, op);
            once(SETTING, &name)?;
            let property = crate::property_of(&name, &Toml::Table(op.clone()))
                .map_err(|why| format!("its setting `{name}`: {why}"))?;
            if let Some(why) = property.default.as_ref().and_then(|d| property.kind.refuses(d)) {
                return Err(format!("its setting `{name}`'s default: {why}"));
            }
            settings.push((property, id.clone()));
        }
        let mut outputs = Vec::new();
        for (id, op) in boxes(&ops, OUTPUT) {
            let name = name_of(id, op);
            once(OUTPUT, &name)?;
            outputs.push((name, id.clone()));
        }
        if outputs.is_empty() {
            return Err("it makes nothing: it has no `output` box".into());
        }
        let label = layer.get("label").and_then(Toml::as_str).map_or_else(|| uid.to_string(), str::to_owned);
        Ok(Definition { uid: uid.to_string(), label, version: version.to_string(), inputs, input_boxes, settings, outputs, ops })
    }

    /// Its settings' values: the layer's, else their defaults.
    #[must_use]
    pub fn values(&self, layer: &Table) -> Table {
        self.settings
            .iter()
            .filter_map(|(p, _)| Some((p.name.clone(), layer.get(&p.name).or(p.default.as_ref())?.clone())))
            .collect()
    }
}

/// A value with each `{ setting = "<name>" }` in it the value of that name.
fn substituted(value: &Toml, settings: &Table) -> Result<Toml, String> {
    Ok(match value {
        Toml::Table(table) if table.len() == 1 && table.get("setting").is_some_and(Toml::is_str) => {
            let name = table.get("setting").and_then(Toml::as_str).unwrap_or_default();
            settings.get(name).cloned().ok_or_else(|| format!("its setting `{name}` is not set, and has no default"))?
        }
        Toml::Table(table) => Toml::Table(
            table.iter().map(|(k, v)| Ok((k.clone(), substituted(v, settings)?))).collect::<Result<_, String>>()?,
        ),
        Toml::Array(items) => {
            Toml::Array(items.iter().map(|v| substituted(v, settings)).collect::<Result<_, String>>()?)
        }
        other => other.clone(),
    })
}

impl Ops {
    /// Whether a viewer takes a value of a type.
    #[must_use]
    pub fn viewable(&self, ty: &Type) -> bool {
        // What is told only when it runs says nothing of what shows it.
        let told = |ty: &Type| matches!(ty, Type::Any | Type::Var(_));
        let open = told(ty) || matches!(ty, Type::File(inner) if told(inner));
        !open && self.viewers.iter().any(|viewer| viewer.unify(ty, &mut Vec::new()))
    }

    /// What turns a value of a type into what a viewer takes, step by step: the fewest steps there are, each the first
    /// by priority, then by name.
    #[must_use]
    pub fn conversion(&self, ty: &Type) -> Vec<(String, Arc<OpDef>)> {
        let mut ordered: Vec<(&String, &Arc<OpDef>)> =
            self.defs.iter().filter(|(_, def)| def.inputs.len() == 1 && def.output.is_some()).collect();
        ordered.sort_by_key(|(name, def)| (-def.priority, name.as_str()));
        let mut seen = vec![ty.to_string()];
        let mut reached = vec![(ty.clone(), Vec::new())];
        while !reached.is_empty() {
            let mut next = Vec::new();
            for (from, path) in reached {
                for (name, def) in ordered.iter().filter(|(_, def)| def.inputs[0].ty.unify(&from, &mut Vec::new())) {
                    let made = def.output.clone().unwrap_or(Type::Any);
                    let mut longer: Vec<(String, Arc<OpDef>)> = path.clone();
                    longer.push(((*name).clone(), (*def).clone()));
                    if self.viewable(&made) {
                        return longer;
                    }
                    if !seen.contains(&made.to_string()) {
                        seen.push(made.to_string());
                        next.push((made, longer));
                    }
                }
            }
            reached = next;
        }
        Vec::new()
    }
}

/// The operations of signals: an output, a preview, an event, a signal and the gates.
fn add_signals(ops: &mut Ops) {
    let signal = Type::Named(SIGNAL_TYPE.to_string());
    let wires = |name: &str| Port { name: name.to_string(), ty: signal.clone(), many: true };
    ops.add(
        OpDef::new(OUTPUT, "Output", "1").input(port("value", Type::Any)).input(wires("pull")).input(wires("listen")),
    );
    ops.add(OpDef::new(PREVIEW, "Preview", "1").input(port("value", Type::Any)));
    ops.add(OpDef::new(EVENT, "Event", "1").makes(signal.clone(), |_| anyhow::bail!("an event is no value")));
    ops.add(OpDef::new(SIGNAL, "Signal", "1").input(wires("when")));
    for gate in GATES {
        let made = OpDef::new(gate.name, gate.label, "1").input(wires("inputs"));
        ops.add(made.makes(signal.clone(), |_| anyhow::bail!("a gate is no value")));
    }
}

/// The table a value holds: itself, or a typed value's settings.
fn table_in(value: &Value) -> Option<&Table> {
    match value {
        Value::Table(table) => Some(table),
        Value::Typed(typed) => Some(&typed.settings),
        _ => None,
    }
}

/// What a value holds at a dotted `field` of its table (all of it where none): a boolean as itself, a table as one.
fn value_at(value: &Value, field: Option<&str>) -> Option<Toml> {
    if field.is_none()
        && let Some(truth) = truth(value)
    {
        return Some(Toml::Boolean(truth));
    }
    let table = table_in(value)?;
    let Some(field) = field else { return Some(Toml::Table(table.clone())) };
    let mut parts = field.split('.');
    let mut at = table.get(parts.next()?)?;
    for part in parts {
        at = at.get(part)?;
    }
    Some(at.clone())
}

/// `over` laid on `under`, key by key, tables by theirs.
fn laid_over(under: &mut Table, over: &Table) {
    for (key, value) in over {
        match (under.get_mut(key), value) {
            (Some(Toml::Table(below)), Toml::Table(above)) => laid_over(below, above),
            _ => {
                under.insert(key.clone(), value.clone());
            }
        }
    }
}

/// The operations of choice: the config built, comparison, logic, choosing another node's output, merging branches and
/// laying tables over others.
fn add_logic(ops: &mut Ops) {
    let bool_type = Type::Named(BOOL_TYPE.to_string());
    let bools = |name: &str| Port { name: name.to_string(), ty: Type::Named(BOOL_TYPE.to_string()), many: true };
    ops.add(OpDef::new(CONFIG, "The config built", "1").makes(Type::Table, |call| {
        let mut table = Table::new();
        if let Some((release, target, properties)) = call.config_said() {
            table.extend(release.map(|r| ("release".to_string(), Toml::String(r))));
            table.extend(target.map(|t| ("target".to_string(), Toml::String(t))));
            table.insert("properties".into(), Toml::Table(properties));
        }
        Ok(Value::Table(table))
    }));
    ops.add(OpDef::new(EQUALS, "Equals", "1").input(port("value", Type::Any)).makes(bool_type.clone(), |call| {
        let field = call.settings().get("field").and_then(Toml::as_str).map(str::to_owned);
        let to = call.settings().get("to").cloned().context("it is compared `to` nothing")?;
        let given = call.take(0)?;
        Ok(boolean(value_at(given.value(), field.as_deref()).is_some_and(|v| v == to)))
    }));
    let logic = |name: &'static str, label: &'static str, gate: &'static str| {
        let table = GATES.iter().find(|g| g.name == gate).map(|g| g.table);
        OpDef::new(name, label, "1").input(bools("inputs")).makes(Type::Named(BOOL_TYPE.to_string()), move |call| {
            let given = call.take_all(0);
            let truths: Option<Vec<bool>> = given.iter().map(|v| truth(v.value())).collect();
            let truths = truths.context("given what is no boolean")?;
            Ok(boolean(table.is_some_and(|table| table(&truths))))
        })
    };
    ops.add(logic(ALL, "All", "and"));
    ops.add(logic(ANY, "Any", "or"));
    ops.add(logic(INVERT, "Invert", "not"));
    // Taken by the evaluation itself, asking for its inputs in turn: run, it is given those made.
    ops.add(OpDef::new(MERGE, "Merge", "1").input(ports("inputs", Type::Any)).makes(Type::Any, |call| {
        call.take_all(0).into_iter().next().map(exec::Input::take).ok_or_else(|| anyhow::Error::new(Absent))
    }));
    ops.add(OpDef::new(SELECT, "Select", "1").input(port("key", Type::Any)).makes(Type::Any, |call| {
        let field = call.settings().get("field").and_then(Toml::as_str).map(str::to_owned);
        let of = call.settings().get("of").and_then(Toml::as_table).context("it chooses `of` nothing")?.clone();
        let address = call.settings().get("output").and_then(Toml::as_str).unwrap_or(RESULT).to_owned();
        let given = call.take(0)?;
        let key = value_at(given.value(), field.as_deref());
        let key = key.as_ref().and_then(Toml::as_str).filter(|k| of.contains_key(*k));
        let key = key.or_else(|| call.settings().get("default").and_then(Toml::as_str));
        let Some(reference) = key.and_then(|k| of.get(k)) else { return Err(anyhow::Error::new(Absent)) };
        Ok((*call.output_of(reference, &address)?).clone())
    }));
    ops.add(OpDef::new(OVERLAY, "Overlay", "1").input(port("over", Type::Any)).input(port("under", Type::Any)).makes(
        Type::Table,
        |call| {
            let field = call.settings().get("field").and_then(Toml::as_str).map(str::to_owned);
            let over = call.take(0)?;
            let under = call.take(1)?;
            let mut table = table_in(under.value()).cloned().context("laid over what is no table")?;
            let over = value_at(over.value(), field.as_deref());
            let Some(Toml::Table(over)) = over else { anyhow::bail!("lays what is no table") };
            match field.as_deref() {
                None => laid_over(&mut table, &over),
                Some(field) => {
                    let mut at = &mut table;
                    let mut parts = field.split('.').peekable();
                    while let Some(part) = parts.next() {
                        let entry = at.entry(part.to_string()).or_insert_with(|| Toml::Table(Table::new()));
                        let Toml::Table(inner) = entry else { anyhow::bail!("its `{field}` is no table below") };
                        if parts.peek().is_none() {
                            laid_over(inner, &over);
                            break;
                        }
                        at = inner;
                    }
                }
            }
            Ok(Value::Table(table))
        },
    ));
}

/// The boxes of an operation the project defines - its inputs, settings - and what stands for a value given from
/// outside a plan. An input and what is given are what the plan is given, never run; a setting makes its value: a
/// boolean as one, anything else as a table's `value`.
fn add_boxes(ops: &mut Ops) {
    let declared = |settings: &Table| Type::parse(settings.get("type")?.as_str()?, "").ok();
    ops.add(OpDef::new(INPUT, "Input", "1").output_of(declared).makes(Type::Any, |_| {
        anyhow::bail!("an input is given only inside an operation the project defines")
    }));
    ops.add(OpDef::new(GIVEN, "Given", "1").output_of(declared).makes(Type::Any, |_| {
        anyhow::bail!("what is given from outside is never made")
    }));
    // Run by the evaluation itself, which lays its body out (`exec::Evaluation::for_each`).
    ops.add(
        OpDef::new(FOR_EACH, "For each", "1")
            .input(port("items", Type::Any))
            .input(ports("with", Type::Any))
            .makes(Type::List(Box::new(Type::Any)), |_| anyhow::bail!("a for-each is run by the evaluation")),
    );
    let made = |settings: &Table| {
        Some(match settings.get("value")? {
            Toml::Boolean(_) => Type::Named(BOOL_TYPE.to_string()),
            _ => Type::Table,
        })
    };
    ops.add(OpDef::new(SETTING, "Setting", "1").output_of(made).makes(Type::Any, |call| {
        let value = call.settings().get("value").cloned().context("it is set to nothing")?;
        Ok(match value {
            Toml::Boolean(set) => boolean(set),
            other => Value::Table(Table::from_iter([("value".to_string(), other)])),
        })
    }));
}

/// The operations of lists: other nodes' outputs, and files concatenated.
fn add_lists(ops: &mut Ops) {
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
}

impl Default for Ops {
    fn default() -> Ops {
        let mut ops = Ops { defs: HashMap::new(), viewers: Arc::new(Vec::new()), definitions: Arc::default() };
        ops.add(OpDef::new(FILE, "Stored file", "1").makes(Type::File(Box::new(Type::Any)), |call| {
            // A file the releases have apart is read as the release being built has it (`<release>/<file>`).
            let name = call.of_release(call.setting_str("file")?);
            // A file the node may lack (`optional`): none, without it.
            if call.settings().get("optional").and_then(Toml::as_bool) == Some(true) {
                return call.stored_if_any(&name).map(Value::Typed);
            }
            call.stored(&name, Vec::new()).map(Value::Typed)
        }));
        ops.add(OpDef::new(PICK, "One of its outputs", "1").input(port("made", Type::Any)).makes(Type::Any, |call| {
            let at = call
                .settings()
                .get("output")
                .and_then(Toml::as_integer)
                .and_then(|at| usize::try_from(at).ok())
                .unwrap_or(0);
            let Value::List(mut made) = call.take(0)?.take() else { anyhow::bail!("given no outputs") };
            anyhow::ensure!(at < made.len(), "it made {} outputs, and its output {at} is asked for", made.len());
            Ok(made.swap_remove(at))
        }));
        ops.add(OpDef::new(CONSTANT, "Constant", "1").makes(Type::Table, |call| {
            let value = call.settings().get("value").and_then(Toml::as_table).cloned();
            value.map(Value::Table).ok_or_else(|| anyhow::anyhow!("its `value` is not a table"))
        }));
        ops.add(OpDef::new(SETTINGS, "Its settings", "1").input(port("node", Type::Any)).makes(Type::Table, |call| {
            let given = call.take(0)?;
            Ok(Value::Table(given.value().settings().cloned().unwrap_or_default()))
        }));
        let declared = |settings: &Table| Type::parse(settings.get("type")?.as_str()?, "").ok();
        ops.add(OpDef::new(BELOW, "The version below", "1").output_of(declared).makes(Type::Any, move |call| {
            let address = call.settings().get("output").and_then(Toml::as_str).unwrap_or("native");
            let reference = format!("{}{}", crate::index::BELOW, exec::id(call.recorder().node()));
            let value = (*call.output_of(&Toml::String(reference), address)?).clone();
            if let Some(ty) = declared(call.settings()) {
                anyhow::ensure!(
                    ty.unify(&value.ty(), &mut Vec::new()),
                    "the version below makes {}, not {ty}",
                    value.ty()
                );
            }
            Ok(value)
        }));
        add_signals(&mut ops);
        add_lists(&mut ops);
        add_logic(&mut ops);
        add_boxes(&mut ops);
        ops.add(OpDef::new(THUMBNAIL, "Thumbnail", "1").input(port("picture", Type::Image)).makes(
            Type::Image,
            |call| {
                let side = call
                    .settings()
                    .get("side")
                    .and_then(Toml::as_integer)
                    .and_then(|s| usize::try_from(s).ok())
                    .unwrap_or(32);
                match call.take(0)?.take() {
                    Value::Image(image) => Ok(Value::Image(image.thumbnail(side))),
                    other => anyhow::bail!("given {}, not an image", other.ty()),
                }
            },
        ));
        ops
    }
}

impl Ops {
    /// Every operation, by the name stacks use.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Arc<OpDef>)> {
        self.defs.iter()
    }

    pub fn add(&mut self, def: OpDef) {
        self.defs.insert(def.name.clone(), Arc::new(def));
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<OpDef>> {
        self.defs.get(name)
    }

    /// What an operation makes, by name - its first under none (`""`) - with its type: an extension's or the host's,
    /// or one the project defines.
    #[must_use]
    pub fn outputs_of(&self, name: &str) -> Vec<(String, Type)> {
        match (self.defs.get(name), self.definitions.get(name)) {
            (Some(def), _) => std::iter::once((String::new(), def.output.clone().unwrap_or(Type::Any)))
                .chain(def.also.iter().cloned())
                .collect(),
            (None, Some(definition)) => definition
                .outputs
                .iter()
                .enumerate()
                .map(|(i, (name, _))| (if i == 0 { String::new() } else { name.clone() }, Type::Any))
                .collect(),
            (None, None) => Vec::new(),
        }
    }

    /// What an operation takes, by port: an extension's, the host's, or one the project defines.
    #[must_use]
    pub fn ports_of(&self, name: &str) -> Vec<Port> {
        match (self.defs.get(name), self.definitions.get(name)) {
            (Some(def), _) => def.inputs.clone(),
            (None, Some(definition)) => definition.inputs.clone(),
            (None, None) => Vec::new(),
        }
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
    /// The step saying whether it runs (`ACTIVE`): it always does where none.
    pub active: Option<usize>,
}

impl Step {
    /// The steps it takes from: by its ports, and what says whether it runs.
    pub fn sources(&self) -> impl Iterator<Item = usize> + '_ {
        self.inputs.iter().flatten().copied().chain(self.active)
    }
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
    /// Shown by a viewer of what it is; it takes part in no build.
    pub preview: bool,
    /// Made as soon as what it reads changes, rather than when asked for.
    pub eager: bool,
    pub at: String,
    pub events: OutputEvents,
}

/// What an output has to do with events.
#[derive(Clone, Debug, Default)]
pub struct OutputEvents {
    /// The steps saying when it is made current, any of them: event expressions.
    pub pull: Vec<usize>,
    /// The steps saying when it goes stale, any of them.
    pub listen: Vec<usize>,
    /// Where it goes once made, under `build/`: `{config}` the config's name, a trailing `/` keeping its file's name.
    pub deploy: Vec<String>,
    /// Signaled where a pull made it now, where that changed what it was, and where it could not be made.
    pub ran: Option<String>,
    pub changed: Option<String>,
    pub failed: Option<String>,
}

/// A signal box: its event, where what it takes holds.
#[derive(Clone, Debug)]
pub struct Rule {
    pub when: Vec<usize>,
    pub event: String,
    pub at: String,
}

/// A stack lowered: its operations, each after those it takes from, its outputs, and its result - the step its stack
/// ends with, which its type's outputs take, and which is the node as its readers see it.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub outputs: Vec<Output>,
    pub result: Option<usize>,
    /// What comes down to each layer of its stack, by its place, and last what comes out of it: what a layer put there
    /// is given. None where nothing does.
    pub above: Vec<Option<Type>>,
    pub rules: Vec<Rule>,
    /// Outputs there is one of for each key of a table of its node's, as `(table, its step, how it is kept)`.
    pub each: Vec<(String, usize, Retain)>,
}

/// An output a node's type adds after its stack: made by an operation from the stack's result - or from what another
/// of the type's outputs is made of (`from`, its index among them) - with settings of its own.
#[derive(Clone, Debug)]
pub struct Tail {
    pub name: String,
    /// One output rather than a family, the value of the parameter it reaches supplied by whoever asks.
    pub fixed: bool,
    pub op: String,
    pub retain: Retain,
    pub from: Option<usize>,
    /// The step of the stack it is made from, where it is not the result.
    pub step: Option<usize>,
    pub settings: Table,
    pub preview: bool,
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

/// What is set on a layer, without how the stack is put together.
#[must_use]
pub fn set_on(op: &str, settings: &Table) -> Table {
    let held =
        settings.iter().filter(|(key, value)| holds_settings(op) && !structural(op, key) && wire_of(value).is_none());
    held.map(|(key, value)| (key.clone(), value.clone())).collect()
}

/// Whether a layer's key is no setting of its node's, but how the stack is put together.
fn structural(op: &str, key: &str) -> bool {
    matches!(key, "op" | "id" | "pos" | "ops" | "result" | ACTIVE)
        || (op == FILE && matches!(key, "file" | "optional"))
        || (op == REFERENCES && matches!(key, "field" | "key" | "output"))
}

/// Whether a layer holds settings of its node's: an output says how it is kept, not what its node is.
fn holds_settings(op: &str) -> bool {
    ![
        OUTPUT,
        PREVIEW,
        GRAPH,
        BELOW,
        CONCATENATE,
        CONFIG,
        EQUALS,
        ALL,
        ANY,
        INVERT,
        MERGE,
        SELECT,
        OVERLAY,
        INPUT,
        SETTING,
        GIVEN,
        FOR_EACH,
    ]
    .contains(&op)
}

/// A node's declaration with the settings a generated layer holds for it (its `settings.toml`, made by extraction) beneath
/// its own: each it does not set put where a setting of its belongs - the layer holding it, else its home layer (the
/// first of its own operations before anything is taken from it, else its stored file) - so that what is set over it
/// there wins, and what the node's operations are set to includes it. As `toml` holds them (`with_generated_values`).
#[must_use]
pub fn with_generated(fields: &Table, generated: &Table) -> Table {
    with_generated_values(pe2_value::Table::from(fields), &pe2_value::Table::from(generated)).to_toml()
}

/// `with_generated`, of the values the editor holds: what it puts in place is shared, not copied.
#[must_use]
pub fn with_generated_values(mut fields: pe2_value::Table, generated: &pe2_value::Table) -> pe2_value::Table {
    use pe2_value::Value as V;
    let Some(mut layers) = fields.get("stack").and_then(V::as_values).map(<[V]>::to_vec) else {
        for (key, value) in generated.iter() {
            if !fields.contains_key(key) {
                fields.insert(key, value.clone());
            }
        }
        return fields;
    };
    let op = |layer: &V| layer.as_table().and_then(|t| t.get("op")).and_then(V::as_str).map(str::to_string);
    let taken = layers.iter().position(|layer| matches!(op(layer).as_deref(), Some(OUTPUT | PREVIEW)));
    let own = layers.iter().take(taken.unwrap_or(usize::MAX)).position(|layer| op(layer).as_deref() != Some(FILE));
    let Some(home) = own.or_else(|| (!layers.is_empty()).then_some(0)) else { return fields };
    let mut changed = false;
    for (key, value) in generated.iter() {
        if layers.iter().any(|layer| layer.as_table().is_some_and(|t| t.contains_key(key))) {
            continue;
        }
        if let Some(layer) = layers[home].as_table_mut() {
            layer.insert(key, value.clone());
            changed = true;
        }
    }
    if changed {
        fields.insert("stack", V::Array(layers.into()));
    }
    fields
}

/// A node's declaration with the settings on its layers among its fields: what each operation is set to, as the node's
/// settings, for whatever reads them by name. Its `stack` stays as it is. As `toml` holds them (`with_settings_values`).
#[must_use]
pub fn with_settings(fields: &Table) -> Table {
    with_settings_values(pe2_value::Table::from(fields)).to_toml()
}

/// `with_settings`, of the values the editor holds: a setting is the layer's, shared, not a copy of it.
#[must_use]
pub fn with_settings_values(mut fields: pe2_value::Table) -> pe2_value::Table {
    use pe2_value::Value as V;
    let wired = |value: &V| {
        value
            .as_table()
            .filter(|t| t.len() == 1)
            .and_then(|t| t.get("from"))
            .is_some_and(|from| matches!(from, V::String(_) | V::Array(_)))
    };
    let mut found: Vec<(String, V)> = Vec::new();
    for layer in fields.get("stack").and_then(V::as_values).into_iter().flatten() {
        let Some(table) = layer.as_table() else { continue };
        let op = table.get("op").and_then(V::as_str).unwrap_or_default();
        if !holds_settings(op) {
            continue;
        }
        found.extend(
            table
                .iter()
                .filter(|(key, value)| !structural(op, key) && !wired(value))
                .map(|(k, v)| (k.to_string(), v.clone())),
        );
    }
    for (key, value) in found {
        if !fields.contains_key(&key) {
            fields.insert(key, value);
        }
    }
    fields
}

/// What a layer's input is connected to, where it says: `{ from = "read.cells" }`, a layer's `id` and, of one with several,
/// which of its outputs.
#[must_use]
pub fn wire_of(value: &Toml) -> Option<&Toml> {
    let table = value.as_table().filter(|table| table.len() == 1)?;
    table.get("from").filter(|from| from.is_str() || from.is_array())
}

/// What of a stack says what it makes: its layers without what is set on them, so that nodes alike share one plan.
#[must_use]
pub fn shape(fields: &Table) -> String {
    let kept = |layer: &Toml| {
        let held = layer.as_table().into_iter().flatten();
        let kept = held.filter(|(key, value)| {
            wire_of(value).is_some()
                || matches!(key.as_str(), "op" | "id" | "name" | "file" | "type" | "result" | "ops" | "each" | "branch")
        });
        Toml::Table(kept.map(|(key, value)| (key.clone(), value.clone())).collect())
    };
    let layers: Vec<Toml> = fields.get("stack").and_then(Toml::as_array).into_iter().flatten().map(kept).collect();
    toml::to_string(&Table::from_iter([("stack".to_string(), Toml::Array(layers))])).unwrap_or_default()
}

/// What is wrong with a stack, where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub at: String,
    pub what: String,
}

impl Plan {
    /// What an event step says.
    #[must_use]
    pub fn expression(&self, step: usize) -> Expr {
        let step = &self.steps[step];
        let inputs = || step.inputs.iter().flatten().map(|from| self.expression(*from)).collect();
        match GATES.iter().find(|gate| gate.name == step.op) {
            Some(gate) => Expr::Gate(gate, inputs()),
            None => Expr::Event(step.settings.get("event").and_then(Toml::as_str).unwrap_or_default().to_string()),
        }
    }

    /// A node's plan, of its fields as the editor holds them: its stack, and the tables its layers take a key each of
    /// (`each`), are what the plan reads of it (`of`).
    #[must_use]
    pub fn of_fields(fields: &pe2_value::Table, ops: &Ops, tail: &[Tail]) -> (Plan, Vec<Problem>) {
        let mut declaration = Table::new();
        if let Some(stack) = fields.get("stack") {
            declaration.insert("stack".into(), stack.to_toml());
            for layer in stack.as_values().into_iter().flatten() {
                let each = layer.as_table().and_then(|t| t.get("each")).and_then(pe2_value::Value::as_str);
                if let Some((each, table)) = each.and_then(|each| Some((each, fields.get(each)?))) {
                    declaration.insert(each.into(), table.to_toml());
                }
            }
        }
        Plan::of(&declaration, ops, tail)
    }

    /// An operation the project defines laid out alone, set to `settings` over its defaults: each of its inputs a step
    /// standing for what it is given (`GIVEN`, the first steps, in order), its outputs the plan's, by name, the first
    /// its result. What a `for_each` runs for each item; what the graph view shows of one.
    #[must_use]
    pub fn of_definition(definition: &Arc<Definition>, settings: &Table, ops: &Ops, at: &str) -> (Plan, Vec<Problem>) {
        let declaration = Table::new();
        let mut lowering = Lowering {
            ops,
            declaration: &declaration,
            plan: Plan::default(),
            problems: Vec::new(),
            named: HashMap::new(),
            picks: HashMap::new(),
            inlining: Vec::new(),
            collecting: Vec::new(),
            groups: HashMap::new(),
        };
        let Some(given) = ops.get(GIVEN).cloned() else { return (Plan::default(), Vec::new()) };
        let mut inputs = Vec::new();
        for port in &definition.inputs {
            let step = lowering.step(&given, GIVEN, Table::new(), Vec::new(), at);
            lowering.plan.steps[step].ty = port.ty.clone();
            inputs.push(vec![step]);
        }
        if let Some(first) = lowering.inline(definition, settings, inputs, None, at) {
            for (name, from) in lowering.groups.get(&first).cloned().unwrap_or_default() {
                lowering.plan.outputs.push(Output {
                    name,
                    parameter: None,
                    fixed: false,
                    from,
                    retain: Retain::Transient,
                    preview: false,
                    eager: false,
                    at: at.to_string(),
                    events: OutputEvents::default(),
                });
            }
            lowering.plan.result = Some(first);
        }
        (lowering.plan, lowering.problems)
    }

    /// A declaration's stack lowered and checked: the plan, and what is wrong with it. A plan with problems still has
    /// the steps and outputs that are sound, so a node keeps what it can make.
    #[must_use]
    pub fn of(declaration: &Table, ops: &Ops, tail: &[Tail]) -> (Plan, Vec<Problem>) {
        let mut lowering = Lowering {
            ops,
            declaration,
            plan: Plan::default(),
            problems: Vec::new(),
            named: HashMap::new(),
            picks: HashMap::new(),
            inlining: Vec::new(),
            collecting: Vec::new(),
            groups: HashMap::new(),
        };
        let result = match declaration.get("stack") {
            None => None,
            Some(Toml::Array(layers)) => lowering.stack(layers),
            Some(_) => {
                lowering.problem("stack", "is not a list of layers");
                None
            }
        };
        // What it is, whatever hangs off it further down: what it says is its native form, else what is set on it.
        let native = lowering.plan.outputs.iter().find(|output| output.name == "native").map(|output| output.from);
        let data = lowering.plan.steps.iter().position(|step| ops.get(&step.op).is_some_and(|def| def.data));
        let result = native.or(data).or(result);
        lowering.plan.above.push(result.map(|step| lowering.plan.steps[step].ty.clone()));
        lowering.plan.result = result;
        // The node as its readers see it is its result: kept - by the stack where an output keeps it, else by the
        // host - unless it is a stored file or what is set on it, which its readers see as they are, or a family (one
        // asset is no family).
        if let Some(result) = result
            && lowering.plan.steps[result].op != FILE
            && !lowering.plan.result_is_data(ops)
            && lowering.plan.result_output().is_none()
            && !lowering.reaches_parameter(result)
        {
            let mut layer = Table::new();
            layer.insert("name".into(), RESULT.into());
            layer.insert("retain".into(), retain_name(Retain::Cache).into());
            layer.insert("when".into(), "eager".into());
            lowering.output(&layer, result, "stack", &[]);
        }
        // The type's outputs, after the stack: a node without a result has none of them.
        let mut made: Vec<Option<usize>> = Vec::with_capacity(tail.len());
        for (i, output) in tail.iter().enumerate() {
            let at = format!("type.outputs[{i}]");
            let from = output.from.map_or_else(|| output.step.or(result), |f| made.get(f).copied().flatten());
            let step = match (from, ops.get(&output.op).cloned()) {
                // One that takes nothing is about the node, not what its stack makes: a stack that fails, or has nothing to make, does not take it away.
                (_, Some(def)) if def.inputs.is_empty() => {
                    Some(lowering.step(&def, &output.op, output.settings.clone(), Vec::new(), &at))
                }
                (Some(from), Some(def)) => {
                    Some(lowering.step(&def, &output.op, output.settings.clone(), vec![vec![from]], &at))
                }
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
                lowering.output(&layer, step, &at, &[]);
                if let Some(made) = lowering.plan.outputs.last_mut().filter(|made| made.name == output.name) {
                    made.preview = output.preview;
                }
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

    /// Whether its result is only what is set on its node.
    #[must_use]
    pub fn result_is_data(&self, ops: &Ops) -> bool {
        self.result.is_some_and(|result| ops.get(&self.steps[result].op).is_some_and(|def| def.data))
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
        let steps: Vec<usize> = outputs.iter().map(|&o| self.outputs[o].from).collect();
        self.steps_reached(&steps, known)
    }

    /// The steps these reach, themselves among them, not going past those `known`.
    #[must_use]
    pub fn steps_reached(&self, steps: &[usize], known: &std::collections::HashSet<usize>) -> Vec<bool> {
        let mut needed = vec![false; self.steps.len()];
        let mut todo: Vec<usize> = steps.to_vec();
        while let Some(s) = todo.pop() {
            if !std::mem::replace(&mut needed[s], true) && !known.contains(&s) {
                todo.extend(self.steps[s].sources());
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
            match step.active {
                Some(active) => {
                    let _ = writeln!(text, "{i} {}@{version} {:?} active {active}\n{settings}", step.op, step.inputs);
                }
                None => {
                    let _ = writeln!(text, "{i} {}@{version} {:?}\n{settings}", step.op, step.inputs);
                }
            }
        }
        let output = &self.outputs[output];
        let _ = writeln!(text, "output {} {:?} from {}", output.name, output.parameter, output.from);
        text
    }

    /// What an output is made with, as a trace keeps it: the operations it reaches and their versions - extensions'
    /// builds among them - sorted, and not their settings or what they take: what another build made, not an older
    /// edit of the same node.
    #[must_use]
    pub fn made(&self, output: usize, ops: &Ops) -> String {
        let needed = self.reached(&[output]);
        let made: std::collections::BTreeSet<String> = self
            .steps
            .iter()
            .enumerate()
            .filter(|(i, _)| needed[*i])
            .map(|(_, step)| format!("{}@{}", step.op, ops.get(&step.op).map_or("?", |d| d.version.as_str())))
            .collect();
        made.into_iter().collect::<Vec<_>>().join(" ")
    }
}

struct Lowering<'a> {
    ops: &'a Ops,
    /// What is set on the node, for what there is one of for each key of a table.
    declaration: &'a Table,
    plan: Plan,
    problems: Vec<Problem>,
    /// The layers of its stack with an `id`, by it: the step each is.
    named: HashMap<String, usize>,
    /// The steps picking outputs, by the step and the output.
    picks: HashMap<(usize, usize), usize>,
    /// The operations the project defines being laid in, outermost first.
    inlining: Vec<String>,
    /// What the `output` boxes of each being laid in are given, by name.
    collecting: Vec<Vec<(String, usize)>>,
    /// The outputs of each operation the project defines laid in, by its first: what `id.<name>` takes.
    groups: HashMap<usize, Vec<(String, usize)>>,
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
        // What comes to it through a value is not known here: a layer of a stack says what it is set to.
        if at.starts_with("stack") {
            for missing in def.required.iter().filter(|key| !settings.contains_key(*key)) {
                self.problem(at, &format!("`{op}` has to be given `{missing}`"));
            }
        }
        let mut bound = Vec::new();
        for (port, from) in def.inputs.iter().zip(&inputs) {
            for &from in from {
                let made = self.plan.steps[from].ty.clone();
                if !port.ty.unify(&made, &mut bound) {
                    self.problem(at, &format!("its `{}` takes {}, and is given {made}", port.name, port.ty));
                }
            }
        }
        let told = def.output_of.as_ref().and_then(|output_of| output_of(&settings));
        let ty = told.unwrap_or_else(|| def.output.as_ref().map_or(Type::Any, |t| t.bind(&bound)));
        self.plan.steps.push(Step { op: op.to_string(), settings, inputs, ty, at: at.to_string(), active: None });
        self.plan.steps.len() - 1
    }

    /// Say what a step takes to tell whether it runs: a boolean.
    fn activate(&mut self, step: usize, active: Option<usize>, at: &str) {
        let Some(active) = active else { return };
        let made = self.plan.steps[active].ty.clone();
        if !Type::Named(BOOL_TYPE.to_string()).unify(&made, &mut Vec::new()) {
            self.problem(at, &format!("its `{ACTIVE}` takes {BOOL_TYPE}, and is given {made}"));
        }
        self.plan.steps[step].active = Some(active);
    }

    /// A stack's layers: the step of its result, where it has one.
    fn stack(&mut self, layers: &[Toml]) -> Option<usize> {
        let mut above: Option<usize> = None;
        for (i, layer) in layers.iter().enumerate() {
            let passed = above.map(|step: usize| self.plan.steps[step].ty.clone());
            self.plan.above.push(passed);
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
                OUTPUT | PREVIEW => {
                    let wired = layer.get("value").and_then(wire_of).and_then(Toml::as_str);
                    let from = match wired {
                        Some(wire) => self.wire(wire, &at),
                        None => above,
                    };
                    match (from, op) {
                        (Some(from), OUTPUT) if layer.contains_key("each") => self.each(layer, from, &at),
                        (Some(from), OUTPUT) => self.output(layer, from, &at, &[]),
                        (Some(from), _) => self.preview(layer, from, &at),
                        (None, _) if wired.is_some() => {}
                        (None, _) => self.problem(
                            &at,
                            &format!(
                                "is {} with nothing above it",
                                if op == OUTPUT { "an output" } else { "a preview" }
                            ),
                        ),
                    }
                }
                GRAPH => above = self.graph(layer, above, &at),
                _ => {
                    let defined = self.ops.definitions.get(op).cloned();
                    let def = self.ops.get(op).cloned();
                    if def.is_none() && defined.is_none() {
                        self.problem(&at, &format!("`{op}`: no extension provides it"));
                        above = None;
                        continue;
                    }
                    let ports = self.ops.ports_of(op);
                    let Some(inputs) = self.inputs_of(&ports, layer, above, &at) else {
                        above = None;
                        continue;
                    };
                    let settings = layer.iter().filter(|(key, value)| {
                        !matches!(key.as_str(), "op" | "id" | ACTIVE) && wire_of(value).is_none()
                    });
                    let settings: Table = settings.map(|(key, value)| (key.clone(), value.clone())).collect();
                    let active = match layer.get(ACTIVE).and_then(wire_of).map(|w| (w, w.as_str())) {
                        Some((_, Some(wire))) => {
                            let Some(step) = self.wire(wire, &at) else {
                                above = None;
                                continue;
                            };
                            Some(step)
                        }
                        Some((_, None)) => {
                            self.problem(&at, &format!("its `{ACTIVE}` takes one value"));
                            None
                        }
                        None => None,
                    };
                    let (Some(def), None) = (def, &defined) else {
                        let defined = defined.unwrap_or_else(|| unreachable!("checked above"));
                        above = self.inline(&defined, &settings, inputs, active, &at);
                        if let (Some(id), Some(step)) = (layer.get("id").and_then(Toml::as_str), above) {
                            self.named.insert(id.to_string(), step);
                        }
                        continue;
                    };
                    let step = self.step(&def, op, settings, inputs, &at);
                    self.activate(step, active, &at);
                    if let Some(id) = layer.get("id").and_then(Toml::as_str) {
                        self.named.insert(id.to_string(), step);
                    }
                    above = Some(if def.also.is_empty() {
                        step
                    } else {
                        self.picked(step, 0, def.output.clone().unwrap_or(Type::Any), &at)
                    });
                }
            }
        }
        above
    }

    /// An output for each key of a table of the node's, under that key's name, its operation told the key as `each`.
    /// Where the layer names a layer above (`branch`, its `id`), every step from that one down is made again for each
    /// key and told it, so a chain of steps runs once a key: a package's variants, each gathered, compiled and linked.
    fn each(&mut self, layer: &Table, from: usize, at: &str) {
        let table = layer.get("each").and_then(Toml::as_str).unwrap_or_default().to_string();
        let keys: Vec<String> = self
            .declaration
            .get(&table)
            .and_then(Toml::as_table)
            .into_iter()
            .flatten()
            .map(|(key, _)| key.clone())
            .collect();
        let retain = match layer.get("retain").and_then(Toml::as_str) {
            Some("cache") => Retain::Cache,
            Some("build") => Retain::Build,
            _ => Retain::Transient,
        };
        let branch = layer.get("branch").and_then(Toml::as_str);
        let start = branch.map_or(Some(from), |id| self.named.get(id).copied().filter(|start| *start <= from));
        let Some(start) = start else {
            self.problem(at, &format!("`branch = \"{}\"`: no layer above it has that id", branch.unwrap_or_default()));
            return;
        };
        self.plan.each.push((table, from, retain));
        for key in keys {
            // Each step of the chain again, what it takes from the chain taken from its own again.
            let mut made: HashMap<usize, usize> = HashMap::new();
            for at_step in start..=from {
                let mut step = self.plan.steps[at_step].clone();
                for port in &mut step.inputs {
                    for input in port.iter_mut() {
                        *input = made.get(input).copied().unwrap_or(*input);
                    }
                }
                step.active = step.active.map(|active| made.get(&active).copied().unwrap_or(active));
                step.settings.insert("each".into(), key.clone().into());
                self.plan.steps.push(step);
                made.insert(at_step, self.plan.steps.len() - 1);
            }
            let mut output = layer.clone();
            output.remove("each");
            output.remove("branch");
            output.insert("name".into(), key.into());
            self.output(&output, made[&from], at, &[]);
        }
    }

    /// What a layer's ports take: what they say they are connected to, or the first what is above it.
    fn inputs_of(&mut self, ports: &[Port], layer: &Table, above: Option<usize>, at: &str) -> Option<Vec<Vec<usize>>> {
        let mut inputs = Vec::with_capacity(ports.len());
        for (place, port) in ports.iter().enumerate() {
            let given = match layer.get(&port.name).and_then(wire_of) {
                Some(Toml::String(wire)) => vec![self.wire(wire, at)?],
                Some(Toml::Array(wires)) => {
                    let wires = wires.iter().filter_map(Toml::as_str).map(|wire| self.wire(wire, at));
                    wires.collect::<Vec<_>>().into_iter().collect::<Option<Vec<usize>>>()?
                }
                _ if place == 0 && above.is_some() => above.into_iter().collect(),
                _ if port.many && place > 0 => Vec::new(),
                _ => {
                    self.problem(at, &format!("its `{}` is connected to nothing", port.name));
                    return None;
                }
            };
            inputs.push(given);
        }
        Some(inputs)
    }

    /// The step a wire comes from: a layer by its `id`, one of several outputs after a dot, the first where none is named.
    fn wire(&mut self, wire: &str, at: &str) -> Option<usize> {
        let (id, output) = wire.split_once('.').map_or((wire, None), |(id, output)| (id, Some(output)));
        let Some(&step) = self.named.get(id) else {
            self.problem(at, &format!("it takes from `{id}`, no layer above it"));
            return None;
        };
        self.output_of(step, id, output, at)
    }

    /// One of the outputs of what a layer or box (`id`) is: its first where none is named. An operation the project
    /// defines has its own, by name; one with several outputs (`also`), a step picking it.
    fn output_of(&mut self, step: usize, id: &str, output: Option<&str>, at: &str) -> Option<usize> {
        if let Some(outputs) = self.groups.get(&step) {
            let Some(name) = output else { return Some(step) };
            let found = outputs.iter().find(|(known, _)| known == name).map(|(_, step)| *step);
            if found.is_none() {
                self.problem(at, &format!("`{id}` has no output `{name}`"));
            }
            return found;
        }
        let def = self.ops.get(&self.plan.steps[step].op).cloned()?;
        let first = def.output.clone().unwrap_or(Type::Any);
        match output {
            None if def.also.is_empty() => Some(step),
            None => Some(self.picked(step, 0, first, at)),
            Some(name) => {
                let Some(place) = def.also.iter().position(|(known, _)| known == name) else {
                    self.problem(at, &format!("`{id}` has no output `{name}`"));
                    return None;
                };
                Some(self.picked(step, place + 1, def.also[place].1.clone(), at))
            }
        }
    }

    /// The step reading one of a step's outputs.
    fn picked(&mut self, step: usize, output: usize, ty: Type, at: &str) -> usize {
        if let Some(picked) = self.picks.get(&(step, output)) {
            return *picked;
        }
        let settings = Table::from_iter([("output".to_string(), Toml::Integer(i64::try_from(output).unwrap_or(0)))]);
        let pick =
            Step { op: PICK.to_string(), settings, inputs: vec![vec![step]], ty, at: at.to_string(), active: None };
        self.plan.steps.push(pick);
        let picked = self.plan.steps.len() - 1;
        self.picks.insert((step, output), picked);
        picked
    }

    /// What is above shown: as it is where a viewer takes it, else through the operation that makes it what one takes.
    fn preview(&mut self, layer: &Table, from: usize, at: &str) {
        let ty = self.plan.steps[from].ty.clone();
        let mut shown = from;
        if !self.ops.viewable(&ty) {
            let steps = self.ops.conversion(&ty);
            if steps.is_empty() {
                self.problem(at, &format!("nothing shows {ty}"));
                return;
            }
            for (op, def) in steps {
                shown = self.step(&def, &op, Table::new(), vec![vec![shown]], at);
            }
        }
        let mut output = layer.clone();
        let taken = self.plan.outputs.iter().filter(|o| o.preview).count();
        output.entry("name").or_insert_with(|| format!("preview_{taken}").into());
        output.entry("retain").or_insert_with(|| retain_name(Retain::Transient).into());
        let before = self.plan.outputs.len();
        self.output(&output, shown, at, &[]);
        if let Some(made) = self.plan.outputs.get_mut(before) {
            made.preview = true;
        }
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
            todo.extend(self.plan.steps[s].sources());
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
                    self.problem(
                        &at,
                        &format!("`{name}` reaches operations of {}: an output is one family", named.join(" and ")),
                    );
                }
            }
        }
    }

    /// What a list of event names or a name, or the wires of a port, come to: one step each, those that would fire on every epoch left out.
    fn triggers(&mut self, layer: &Table, key: &str, wired: &[usize], at: &str) -> Vec<usize> {
        let names: Vec<&str> = match layer.get(key) {
            Some(Toml::String(name)) => vec![name.as_str()],
            Some(Toml::Array(names)) => names.iter().filter_map(Toml::as_str).collect(),
            _ => Vec::new(),
        };
        let mut steps = wired.to_vec();
        for name in names {
            let mut settings = Table::new();
            settings.insert("event".into(), name.into());
            self.plan.steps.push(Step {
                op: EVENT.to_string(),
                settings,
                inputs: Vec::new(),
                ty: Type::Named(SIGNAL_TYPE.to_string()),
                at: at.to_string(),
                active: None,
            });
            steps.push(self.plan.steps.len() - 1);
        }
        let (kept, refused): (Vec<usize>, Vec<usize>) =
            steps.into_iter().partition(|step| !self.plan.expression(*step).holds(&|_| false));
        if !refused.is_empty() {
            self.problem(at, &format!("its `{key}` holds with nothing signaled: it would fire on every epoch"));
        }
        kept
    }

    fn output(&mut self, layer: &Table, from: usize, at: &str, wired: &[Vec<usize>]) {
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
        let pull = self.triggers(layer, "pull", wired.first().map_or(&[], Vec::as_slice), at);
        let listen = self.triggers(layer, "listen", wired.get(1).map_or(&[], Vec::as_slice), at);
        let text = |key: &str| layer.get(key).and_then(Toml::as_str).map(str::to_string);
        let deploy = match layer.get("deploy") {
            Some(Toml::String(place)) => vec![place.clone()],
            Some(Toml::Array(places)) => places.iter().filter_map(Toml::as_str).map(str::to_string).collect(),
            _ => Vec::new(),
        };
        for place in
            deploy.iter().filter(|place| !place.starts_with("build/") || place.split('/').any(|part| part == ".."))
        {
            self.problem(at, &format!("`deploy = \"{place}\"`: what is deployed goes under `build/`"));
        }
        let events =
            OutputEvents { pull, listen, deploy, ran: text("ran"), changed: text("changed"), failed: text("failed") };
        self.plan.outputs.push(Output {
            name: name.to_string(),
            parameter: None,
            fixed,
            from,
            retain,
            preview: false,
            eager,
            at: at.to_string(),
            events,
        });
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
        let given: HashMap<String, Vec<usize>> =
            above.map(|above| (ABOVE.to_string(), vec![above])).into_iter().collect();
        let placed = self.graph_of(ops, &given, at);
        match layer.get("result").and_then(Toml::as_str) {
            Some(ABOVE) => above,
            Some(result) => {
                let (id, output) = result.split_once('.').map_or((result, None), |(id, o)| (id, Some(o)));
                if let Some(&step) = placed.get(id) {
                    return self.output_of(step, id, output, at);
                }
                // One that could not be lowered has said why.
                if !ops.contains_key(id) {
                    self.problem(at, &format!("its result `{id}` is none of its operations"));
                }
                None
            }
            None => {
                self.problem(at, "names no result");
                None
            }
        }
    }

    /// An operation the project defines laid in: its graph, its `input` boxes given what its inputs take, its
    /// `setting` boxes the layer's settings over their defaults - read too where a value is `{ setting = "<name>" }` -
    /// and what its `output` boxes are given its outputs, by name: each its own step, not made where `active` says so.
    /// Its first output's step.
    fn inline(
        &mut self,
        definition: &Arc<Definition>,
        settings: &Table,
        inputs: Vec<Vec<usize>>,
        active: Option<usize>,
        at: &str,
    ) -> Option<usize> {
        if self.inlining.contains(&definition.uid) {
            self.problem(at, &format!("`{}` uses itself, through others or not", definition.label));
            return None;
        }
        let here = format!("{at} ({})", definition.label);
        let mut given: HashMap<String, Vec<usize>> = HashMap::new();
        for ((port, from), id) in definition.inputs.iter().zip(inputs).zip(&definition.input_boxes) {
            let mut bound = Vec::new();
            for &step in &from {
                let made = self.plan.steps[step].ty.clone();
                if !port.ty.unify(&made, &mut bound) {
                    self.problem(at, &format!("its `{}` takes {}, and is given {made}", port.name, port.ty));
                }
            }
            given.insert(id.clone(), from);
        }
        let values = definition.values(settings);
        let setting_op = self.ops.get(SETTING).cloned()?;
        for (property, id) in &definition.settings {
            let Some(value) = values.get(&property.name) else {
                self.problem(at, &format!("its setting `{}` is not set, and has no default", property.name));
                return None;
            };
            if let Some(why) = property.kind.refuses(value) {
                self.problem(at, &format!("its setting `{}`: {why}", property.name));
            }
            let set = Table::from_iter([("value".to_string(), value.clone())]);
            let step = self.step(&setting_op, SETTING, set, Vec::new(), &format!("{here}.ops.{id}"));
            given.insert(id.clone(), vec![step]);
        }
        let inside: Table = definition
            .ops
            .iter()
            .filter(|(id, _)| !given.contains_key(id.as_str()))
            .map(|(id, op)| (id.clone(), op.clone()))
            .collect();
        let ops = match substituted(&Toml::Table(inside), &values) {
            Ok(Toml::Table(ops)) => ops,
            Ok(_) => return None,
            Err(why) => {
                self.problem(at, &format!("`{}`: {why}", definition.label));
                return None;
            }
        };
        self.inlining.push(definition.uid.clone());
        self.collecting.push(Vec::new());
        self.graph_of(&ops, &given, &here);
        let made = self.collecting.pop().unwrap_or_default();
        self.inlining.pop();
        let merge = self.ops.get(MERGE).cloned()?;
        let mut outputs = Vec::new();
        for (name, _) in &definition.outputs {
            let Some(&(_, from)) = made.iter().find(|(made, _)| made == name) else {
                self.problem(at, &format!("`{}`: its output `{name}` is given nothing", definition.label));
                return None;
            };
            // Each its own step: what tells this operation's outputs from others', and what says whether they are made.
            let step = self.step(&merge, MERGE, Table::new(), vec![vec![from]], at);
            self.plan.steps[step].ty = self.plan.steps[from].ty.clone();
            self.activate(step, active, at);
            outputs.push((name.clone(), step));
        }
        let first = outputs.first()?.1;
        self.groups.insert(first, outputs);
        Some(first)
    }

    /// A graph's operations, after those they take from - what it is given by name (`above`, an operation's inputs) -
    /// and its result's step, where it has a sound one.
    fn graph_of(&mut self, ops: &Table, given: &HashMap<String, Vec<usize>>, at: &str) -> HashMap<String, usize> {
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
            let ports = self.ops.ports_of(name);
            let names = ports.iter().map(|port| port.name.as_str()).chain([ACTIVE]);
            // What says whether it runs last, after its ports.
            let ins = names
                .map(|port| match from.and_then(|f| f.get(port)) {
                    Some(Toml::String(id)) => vec![id.as_str()],
                    Some(Toml::Array(ids)) => ids.iter().filter_map(Toml::as_str).collect(),
                    _ => Vec::new(),
                })
                .collect();
            wiring.insert(id, (op, ins));
        }
        // Kahn's order: each operation once all it takes from are settled; what is left is in a cycle. One that cannot be
        // lowered has said why, and what takes from it has nothing to say.
        let mut placed: HashMap<&str, usize> = HashMap::new();
        let mut failed: HashSet<&str> = HashSet::new();
        let mut left: Vec<&str> = wiring.keys().copied().collect();
        loop {
            let ready = left.iter().position(|id| {
                wiring[id].1.iter().flatten().map(|from| from.split_once('.').map_or(*from, |(id, _)| id)).all(|from| {
                    given.contains_key(from)
                        || placed.contains_key(from)
                        || failed.contains(from)
                        || !wiring.contains_key(from)
                })
            });
            let Some(i) = ready else { break };
            let id = left.remove(i);
            let (op, ins) = &wiring[id];
            let here = format!("{at}.ops.{id}");
            let settled =
                !ins.iter().flatten().any(|from| failed.contains(from.split_once('.').map_or(*from, |(id, _)| id)));
            match settled.then(|| self.graph_op(op, ins, &placed, given, &here)).flatten() {
                Some(step) => {
                    placed.insert(id, step);
                }
                None => {
                    failed.insert(id);
                }
            }
        }
        for id in &left {
            self.problem(&format!("{at}.ops.{id}"), "is in a cycle: it takes, through others, from itself");
        }
        placed.into_iter().map(|(id, step)| (id.to_string(), step)).collect()
    }

    /// The steps a graph's operation takes from by an id: what the graph is given by that name, or one of its
    /// operations - one of its outputs where the id names one (`id.<name>`).
    fn source(
        &mut self,
        id: &str,
        placed: &HashMap<&str, usize>,
        given: &HashMap<String, Vec<usize>>,
        at: &str,
    ) -> Option<Vec<usize>> {
        if let Some(steps) = given.get(id) {
            return Some(steps.clone());
        }
        let (base, output) = id.split_once('.').map_or((id, None), |(base, o)| (base, Some(o)));
        let step = *placed.get(base)?;
        self.output_of(step, base, output, at).map(|step| vec![step])
    }

    fn graph_op(
        &mut self,
        op: &Table,
        ins: &[Vec<&str>],
        placed: &HashMap<&str, usize>,
        given: &HashMap<String, Vec<usize>>,
        at: &str,
    ) -> Option<usize> {
        let Some(name) = op.get("op").and_then(Toml::as_str) else {
            self.problem(at, "names no operation (`op`)");
            return None;
        };
        if name == GRAPH {
            self.problem(at, "is a graph inside a graph");
            return None;
        }
        let defined = self.ops.definitions.get(name).cloned();
        let def = self.ops.get(name).cloned();
        if def.is_none() && defined.is_none() {
            self.problem(at, &format!("`{name}`: no extension provides it"));
            return None;
        }
        if [INPUT, SETTING, GIVEN].contains(&name) {
            self.problem(at, &format!("`{name}` is a box of an operation the project defines, not of a graph layer"));
            return None;
        }
        let ports = self.ops.ports_of(name);
        let mut inputs = Vec::new();
        for (port, ids) in ports.iter().zip(ins) {
            let mut steps = Vec::new();
            for id in ids {
                let Some(step) = self.source(id, placed, given, at) else {
                    self.problem(at, &format!("its `{}` takes from `{id}`, which makes nothing", port.name));
                    return None;
                };
                steps.extend(step);
            }
            if !port.many && steps.len() != 1 {
                self.problem(at, &format!("its `{}` takes one value, and is given {}", port.name, steps.len()));
                return None;
            }
            inputs.push(steps);
        }
        // In an operation the project defines, an output is one of its own.
        if name == OUTPUT
            && let Some(collected) = self.collecting.last_mut()
        {
            let id = at.rsplit_once(".ops.").map_or(at, |(_, id)| id);
            let output = op.get("name").and_then(Toml::as_str).unwrap_or(id).to_string();
            collected.push((output, inputs[0][0]));
            return None;
        }
        if name == OUTPUT {
            self.output(op, inputs[0][0], at, &inputs[1..]);
            return None;
        }
        if name == PREVIEW {
            self.preview(op, inputs[0][0], at);
            return None;
        }
        if name == SIGNAL {
            let Some(event) = op.get("event").and_then(Toml::as_str) else {
                self.problem(at, "signals no `event`");
                return None;
            };
            let when = self.triggers(&Table::new(), "when", &inputs[0], at);
            self.plan.rules.push(Rule { when, event: event.to_string(), at: at.to_string() });
            return None;
        }
        let active = match ins.get(ports.len()).map(Vec::as_slice) {
            None | Some([]) => None,
            Some([id]) => {
                let steps = self.source(id, placed, given, at);
                let Some([step]) = steps.as_deref() else {
                    self.problem(at, &format!("its `{ACTIVE}` takes from `{id}`, which makes no one value"));
                    return None;
                };
                Some(*step)
            }
            Some(ids) => {
                self.problem(at, &format!("its `{ACTIVE}` takes one value, and is given {}", ids.len()));
                return None;
            }
        };
        let settings = settings_of(op, &["op", "from", layers::POS]);
        let Some(def) = def else {
            let defined = defined?;
            return self.inline(&defined, &settings, inputs, active, at);
        };
        let step = self.step(&def, name, settings, inputs, at);
        self.activate(step, active, at);
        Some(step)
    }
}

/// A stack's import: the stored file on top - its source - and the operation below it, with that operation's settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Import<'a> {
    pub source: &'a str,
    pub op: &'a str,
    pub settings: Table,
}

/// A node's import, of its fields as the editor holds them (`import_of`): its source file's name and its importer.
#[must_use]
pub fn import_of_fields(fields: &pe2_value::Table) -> Option<(String, String)> {
    use pe2_value::Value as V;
    let stack = fields.get("stack")?.as_values()?;
    let top = stack.first()?.as_table()?;
    let source = top.get("file")?.as_str().filter(|_| top.get("op").and_then(V::as_str) == Some(FILE))?;
    let below = stack.get(1)?.as_table()?;
    let op = below.get("op")?.as_str().filter(|op| ![FILE, CONSTANT, OUTPUT, PREVIEW, GRAPH].contains(op))?;
    Some((source.to_string(), op.to_string()))
}

/// A declaration's import, where its stack begins with a stored file and an operation taking it.
#[must_use]
pub fn import_of(fields: &Table) -> Option<Import<'_>> {
    let stack = fields.get("stack")?.as_array()?;
    let top = stack.first()?.as_table()?;
    let source = top.get("file")?.as_str().filter(|_| top.get("op").and_then(Toml::as_str) == Some(FILE))?;
    let below = stack.get(1)?.as_table()?;
    let op = below.get("op")?.as_str().filter(|op| ![FILE, CONSTANT, OUTPUT, PREVIEW, GRAPH].contains(op))?;
    Some(Import { source, op, settings: settings_of(below, &["op"]) })
}

#[cfg(test)]
mod tests;
