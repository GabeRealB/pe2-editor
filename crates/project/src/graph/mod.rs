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
        let mut ops = Ops { defs: HashMap::new(), viewers: Arc::new(Vec::new()) };
        ops.add(OpDef::new(FILE, "Stored file", "1").makes(Type::File(Box::new(Type::Any)), |call| {
            let name = call.setting_str("file")?.to_string();
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
    matches!(key, "op" | "id" | "pos" | "ops" | "result")
        || (op == FILE && matches!(key, "file" | "optional"))
        || (op == REFERENCES && matches!(key, "field" | "key" | "output"))
}

/// Whether a layer holds settings of its node's: an output says how it is kept, not what its node is.
fn holds_settings(op: &str) -> bool {
    ![OUTPUT, PREVIEW, GRAPH, BELOW, CONCATENATE].contains(&op)
}

/// A node's declaration with the settings on its layers among its fields: what each operation is set to, as the node's
/// settings, for whatever reads them by name. Its `stack` stays as it is.
#[must_use]
pub fn with_settings(mut fields: Table) -> Table {
    let mut found = Vec::new();
    for layer in fields.get("stack").and_then(Toml::as_array).into_iter().flatten() {
        let op = layer.get("op").and_then(Toml::as_str).unwrap_or_default();
        let held = layer.as_table().filter(|_| holds_settings(op)).into_iter().flatten();
        found.extend(
            held.filter(|(key, value)| !structural(op, key) && wire_of(value).is_none())
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    for (key, value) in found {
        fields.entry(key).or_insert(value);
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
        self.plan.steps.push(Step { op: op.to_string(), settings, inputs, ty, at: at.to_string() });
        self.plan.steps.len() - 1
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
                    let Some(def) = self.ops.get(op).cloned() else {
                        self.problem(&at, &format!("`{op}`: no extension provides it"));
                        above = None;
                        continue;
                    };
                    let Some(inputs) = self.inputs_of(&def, layer, above, &at) else {
                        above = None;
                        continue;
                    };
                    let settings = layer
                        .iter()
                        .filter(|(key, value)| !matches!(key.as_str(), "op" | "id") && wire_of(value).is_none());
                    let settings = settings.map(|(key, value)| (key.clone(), value.clone())).collect();
                    let step = self.step(&def, op, settings, inputs, &at);
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
    fn inputs_of(&mut self, def: &OpDef, layer: &Table, above: Option<usize>, at: &str) -> Option<Vec<Vec<usize>>> {
        let mut inputs = Vec::with_capacity(def.inputs.len());
        for (place, port) in def.inputs.iter().enumerate() {
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
        self.plan.steps.push(Step { op: PICK.to_string(), settings, inputs: vec![vec![step]], ty, at: at.to_string() });
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
        // Kahn's order: each operation once all it takes from are settled; what is left is in a cycle. One that cannot be
        // lowered has said why, and what takes from it has nothing to say.
        let mut placed: HashMap<&str, usize> = HashMap::new();
        let mut failed: HashSet<&str> = HashSet::new();
        let mut left: Vec<&str> = wiring.keys().copied().collect();
        loop {
            let ready = left.iter().position(|id| {
                wiring[id].1.iter().flatten().all(|from| {
                    *from == ABOVE || placed.contains_key(from) || failed.contains(from) || !wiring.contains_key(from)
                })
            });
            let Some(i) = ready else { break };
            let id = left.remove(i);
            let (op, ins) = &wiring[id];
            let here = format!("{at}.ops.{id}");
            let settled = !ins.iter().flatten().any(|from| failed.contains(from));
            match settled.then(|| self.graph_op(op, ins, &placed, above, &here)).flatten() {
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

    fn graph_op(
        &mut self,
        op: &Table,
        ins: &[Vec<&str>],
        placed: &HashMap<&str, usize>,
        above: Option<usize>,
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
        Some(self.step(&def, name, settings_of(op, &["op", "from", layers::POS]), inputs, at))
    }
}

/// A stack's import: the stored file on top - its source - and the operation below it, with that operation's settings.
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
    let op = below.get("op")?.as_str().filter(|op| ![FILE, CONSTANT, OUTPUT, PREVIEW, GRAPH].contains(op))?;
    Some(Import { source, op, settings: settings_of(below, &["op"]) })
}

#[cfg(test)]
mod tests;
