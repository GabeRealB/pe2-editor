//! Operations run in steps: what an operation reads of other nodes is requested as it goes, and made between its steps,
//! with nothing of it held meanwhile; what it does for many items at once is spawned, each its own micro op, and joined
//! before the next step.
//!
//! An operation written `#[pe2_ext::steps]` is cut into steps where a `let` is marked, wherever a statement stands in its
//! body's blocks, `if`s, `match`es and loops:
//!
//! ```ignore
//! #[pe2_ext::steps]
//! fn inter(call: &mut Call) -> Result<Typed> {
//!     let stages: Vec<String> = stages_of(&call.given());
//!     #[request]
//!     let layouts: Vec<Table> = call.tables_of(stages.iter().map(|stage| (stage.as_str(), SETTINGS)))?;
//!     #[each]
//!     let sizes: Vec<u64> = layouts.into_iter().map(|layout: Table| size_of(&layout)).collect();
//!     let mut at: String = first_of(&sizes);
//!     loop {
//!         #[request]
//!         let found: Table = call.table_of(&at, SETTINGS)?;
//!         let Some(next) = next_of(&found) else { break };
//!         at = next;
//!     }
//!     #[call]
//!     let joined: Data = join(call, at, 2)?;
//!     // ...
//! }
//!
//! #[pe2_ext::steps]
//! fn join(call: &mut Call, from: String, times: u32) -> Result<Data> { ... }
//! ```
//!
//! - `#[request]` - `call.tables_of`, `outputs_of`, `table_of` or `output_of` - ends the step: the host makes what it
//!   names, and the next step begins with it bound.
//! - `#[each]` - `items.map(closure).collect()` - ends the step spawning the closure once for each item, each a micro op
//!   of its own, kept by what it was given, at the same time where threads are free; the next step begins with what they
//!   made, in the items' order. A map naming a `#[steps]` function runs it for each item, which may read in steps itself.
//! - `#[call]` - `helper(call, args...)?` - ends the step running another `#[steps]` function with these arguments, and
//!   the next step begins with what it made. A helper is registered once, by its name, so it may call itself.
//!
//! What a step reads is only what the steps before it requested: another read fails by name, as every operation's does.
//! A loop holding a cut runs a step at a time for as long as it goes on, each turn a level of the host's micro ops, so it
//! is for what goes a few levels deep - a base's base - and what goes wide is an `#[each]`.
//!
//! What lives across a cut - a variable bound before it and named after it - is carried to the next step, so it is written
//! with its type (`let stages: Vec<String> = ...`) and that type serializes; so are a helper's arguments. An `#[each]`
//! closure sees only its item and `call`: it runs elsewhere, so what it needs of the step is put in its items.
//!
//! What is carried is CBOR (RFC 8949): a binary encoding of serde's data model that describes itself, as reading back a
//! TOML table needs (`deserialize_any`), and whose bytes are the same for the same values, as a step's key needs.
//!
//! As the host runs them: the operation's run is its first step. A step ends at a cut, its value a record of it (which
//! cut, what it carries, requests and spawns, and the operations running the next step and its items), or finishes, its
//! value the operation's. The host goes on from a record (`continuation`): it makes what was requested, runs what was
//! spawned, each item kept by what it was given, and runs the next step, given the record, what the items made and the
//! operation's ports, until a step finishes. Nothing of a step is held between: what the next needs is in the record.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::registry::{OpBuilder, Registry};
use crate::{Call, Data, File, Need, Result, Table, Typed, Value, anyhow};

/// A value as carried, read before it is known as a type.
pub type Carried = ciborium::Value;

/// What a step is handed: what the steps before carried, what was requested for it, what was spawned for it, and the
/// item it runs for, where it does.
pub struct Handed {
    state: Carried,
    got: Vec<Result<Data>>,
    needs: Vec<Need>,
    joined: Vec<Vec<u8>>,
    item: Option<Vec<u8>>,
}

impl Handed {
    /// What the step before carried, as its type.
    ///
    /// # Errors
    /// It does not read as `T`.
    pub fn state<T: DeserializeOwned>(&self) -> Result<T> {
        self.state.deserialized().map_err(|e| anyhow!("what the step before carried does not read back: {e}"))
    }

    /// What the step before requested, made.
    ///
    /// # Errors
    /// It is not what `T` takes.
    pub fn requested<T: FromOutputs>(&mut self) -> Result<T> {
        T::from_outputs(std::mem::take(&mut self.got), &self.needs)
    }

    /// What the items the step before spawned made, in their order.
    ///
    /// # Errors
    /// One does not read as `T`.
    pub fn joined<T: DeserializeOwned>(&self) -> Result<Vec<T>> {
        self.joined.iter().map(|made| from_bytes(made)).collect()
    }

    /// What the helper the step before called made.
    ///
    /// # Errors
    /// It made nothing, or not a `T`.
    pub fn called<T: DeserializeOwned>(&self) -> Result<T> {
        from_bytes(self.joined.first().ok_or_else(|| anyhow!("the helper called made nothing"))?)
    }

    /// The item this runs for, where it was spawned: its arguments, for a helper.
    ///
    /// # Errors
    /// It was spawned for none, or it does not read as `T`.
    pub fn item<T: DeserializeOwned>(&self) -> Result<T> {
        from_bytes(self.item.as_deref().ok_or_else(|| anyhow!("it was spawned for no item"))?)
    }
}

/// How a step ends at a cut: which, what it carries on, and what it requests or spawns.
pub struct Ended {
    cut: u32,
    state: Carried,
    needs: Vec<Need>,
    items: Vec<Carried>,
}

impl Ended {
    /// The step ends at a cut asking for these outputs, carrying `state`.
    ///
    /// # Errors
    /// `state` does not serialize.
    pub fn request<S: Serialize>(cut: u32, state: &S, needs: Vec<Need>) -> Result<Ended> {
        Ok(Ended { cut, state: carried(state)?, needs, items: Vec::new() })
    }

    /// The step ends at a cut spawning one micro op for each item, carrying `state`.
    ///
    /// # Errors
    /// `state` or an item does not serialize.
    pub fn spawn<S: Serialize, I: Serialize>(cut: u32, state: &S, items: impl IntoIterator<Item = I>) -> Result<Ended> {
        let items = items.into_iter().map(|item| carried(&item)).collect::<Result<_>>()?;
        Ok(Ended { cut, state: carried(state)?, needs: Vec::new(), items })
    }
}

/// How a step ends: at a cut, or with what the operation makes.
pub enum Flow<R> {
    Cut(Ended),
    Done(R),
}

/// A step: what runs from the start, or from a cut, to the next cut or the end.
pub type Entry<R> = dyn Fn(&mut Call<'_>, Handed) -> Result<Flow<R>> + Send + Sync;
/// What runs for each item a cut spawns: given it as CBOR, what it makes as CBOR.
pub type Each = dyn Fn(&mut Call<'_>, &[u8]) -> Result<Vec<u8>> + Send + Sync;

/// What a cut's items run: a closure, or a helper in steps, by its name and how its steps are made.
pub enum Spawn {
    Each(Arc<Each>),
    Steps { name: String, make: Arc<dyn Fn() -> Steps<Data> + Send + Sync> },
}

/// An operation's steps as `#[steps]` writes them: the step from the start, then the step after each cut, in order; and
/// for each cut, what its items run, where it spawns.
pub struct Steps<R> {
    entries: Vec<Arc<Entry<R>>>,
    spawns: Vec<Option<Spawn>>,
}

impl<R: 'static> Default for Steps<R> {
    fn default() -> Self {
        Steps { entries: Vec::new(), spawns: Vec::new() }
    }
}

impl<R: 'static> Steps<R> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The next step: from the start, the first; then from each cut in order.
    #[must_use]
    pub fn entry(mut self, entry: impl Fn(&mut Call<'_>, Handed) -> Result<Flow<R>> + Send + Sync + 'static) -> Self {
        self.entries.push(Arc::new(entry));
        self
    }

    /// The next cut: a request.
    #[must_use]
    pub fn cut(mut self) -> Self {
        self.spawns.push(None);
        self
    }

    /// The next cut: spawning what `each` runs.
    #[must_use]
    pub fn cut_each(mut self, each: impl Fn(&mut Call<'_>, &[u8]) -> Result<Vec<u8>> + Send + Sync + 'static) -> Self {
        self.spawns.push(Some(Spawn::Each(Arc::new(each))));
        self
    }

    /// The next cut: spawning a helper in steps, the function `#[steps]` made of it, for each item.
    #[must_use]
    pub fn cut_steps<S: Serialize + 'static, F: Fn() -> Steps<S> + Send + Sync + 'static>(mut self, helper: F) -> Self {
        let name = std::any::type_name_of_val(&helper).replace("::", ".");
        let make = move || helper().map(|made| Ok(made_value(to_bytes(&made)?)));
        self.spawns.push(Some(Spawn::Steps { name, make: Arc::new(make) }));
        self
    }

    /// What it makes, made into another value.
    #[must_use]
    pub fn map<S: 'static>(self, into: impl Fn(R) -> Result<S> + Send + Sync + 'static) -> Steps<S> {
        let into = Arc::new(into);
        let entries = self
            .entries
            .into_iter()
            .map(|entry| {
                let into = into.clone();
                Arc::new(move |call: &mut Call<'_>, handed| match entry(call, handed)? {
                    Flow::Cut(ended) => Ok(Flow::Cut(ended)),
                    Flow::Done(made) => Ok(Flow::Done(into(made)?)),
                }) as Arc<Entry<S>>
            })
            .collect();
        Steps { entries, spawns: self.spawns }
    }
}

/// What a `#[request]` binds what it read as: all of it, failing where one read did, or one read's own result.
pub trait FromOutputs: Sized {
    /// The outputs, each made for its need - or why it could not be - in order.
    ///
    /// # Errors
    /// One could not be made, or is not what this takes.
    fn from_outputs(outputs: Vec<Result<Data>>, needs: &[Need]) -> Result<Self>;
}

/// An output as a table, where it is one.
fn table_of(output: Data, need: &Need) -> Result<Table> {
    match output {
        Data::Table(table) => Ok(table),
        other => Err(anyhow!("{}: {} is {}, no table", need.node, need.address, other.ty())),
    }
}

/// The one output a single read made, or why not.
fn one(outputs: Vec<Result<Data>>, needs: &[Need]) -> Result<(Result<Data>, Need)> {
    let mut outputs = outputs.into_iter();
    match (outputs.next(), outputs.next(), needs) {
        (Some(one), None, [need]) => Ok((one, need.clone())),
        _ => Err(anyhow!("one output is read here, of {} requested", needs.len())),
    }
}

impl FromOutputs for Vec<Data> {
    fn from_outputs(outputs: Vec<Result<Data>>, _: &[Need]) -> Result<Self> {
        outputs.into_iter().collect()
    }
}

impl FromOutputs for Vec<Table> {
    fn from_outputs(outputs: Vec<Result<Data>>, needs: &[Need]) -> Result<Self> {
        outputs.into_iter().zip(needs).map(|(output, need)| table_of(output?, need)).collect()
    }
}

impl FromOutputs for Data {
    fn from_outputs(outputs: Vec<Result<Data>>, needs: &[Need]) -> Result<Self> {
        one(outputs, needs)?.0
    }
}

impl FromOutputs for Table {
    fn from_outputs(outputs: Vec<Result<Data>>, needs: &[Need]) -> Result<Self> {
        let (output, need) = one(outputs, needs)?;
        table_of(output?, &need)
    }
}

/// Each read's own result, in order (`Call::outputs_tried`).
impl FromOutputs for Vec<Result<Data>> {
    fn from_outputs(outputs: Vec<Result<Data>>, _: &[Need]) -> Result<Self> {
        Ok(outputs)
    }
}

/// One read's own result: what it made, or why it could not be - for a read that may find nothing.
impl FromOutputs for Result<Data> {
    fn from_outputs(outputs: Vec<Result<Data>>, needs: &[Need]) -> Result<Self> {
        Ok(one(outputs, needs)?.0)
    }
}

impl FromOutputs for Result<Table> {
    fn from_outputs(outputs: Vec<Result<Data>>, needs: &[Need]) -> Result<Self> {
        let (output, need) = one(outputs, needs)?;
        Ok(output.and_then(|output| table_of(output, &need)))
    }
}

/// The outputs a `#[request]` names: `(node, address)` pairs.
#[must_use]
pub fn needs<N: AsRef<str>, A: AsRef<str>>(pairs: impl IntoIterator<Item = (N, A)>) -> Vec<Need> {
    pairs.into_iter().map(|(node, address)| Need::new(node.as_ref(), address.as_ref())).collect()
}

/// A value as carried between steps: CBOR.
///
/// # Errors
/// It does not serialize.
pub fn to_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes).map_err(|e| anyhow!("it cannot be carried to the next step: {e}"))?;
    Ok(bytes)
}

/// A value carried between steps, read back.
///
/// # Errors
/// It does not read as `T`.
pub fn from_bytes<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    ciborium::from_reader(bytes).map_err(|e| anyhow!("what the step before carried does not read back: {e}"))
}

fn carried<T: Serialize + ?Sized>(value: &T) -> Result<Carried> {
    Carried::serialized(value).map_err(|e| anyhow!("it cannot be carried to the next step: {e}"))
}

/// The settings key an item a step spawned is given in, as CBOR in base64; the step's own code never sees it.
pub const ITEM: &str = "~item";
/// What a step ending at a cut makes: a value of this type, its one file the record (`continuation`).
pub const RECORD: &str = "stepped@record";
/// The ports a step after the start is given before the operation's: the record of the cut it begins at, and what the
/// cut's items made.
pub const OWN: usize = 2;

/// What a step ending at a cut hands on, for the host to go on from: which cut, what it carries, what it requests, what it
/// spawns and runs it; and the operations of its extension running the next step and its items.
#[derive(Serialize, Deserialize)]
struct Record {
    cut: u32,
    state: Carried,
    needs: Vec<(String, String)>,
    /// Each item spawned, as its settings hold it (`ITEM`).
    items: Vec<String>,
    next: String,
    spawn: Option<String>,
}

/// How the host goes on from a step that ended at a cut: it makes `needs`, runs `spawn` for each of `items` - each given
/// its item in its settings (`ITEM`) and the operation's ports after two empty ones - then runs `next`, given this record,
/// what the items made, in order, and the operation's ports. Operations are named within the extension.
pub struct Continuation {
    pub needs: Vec<Need>,
    pub items: Vec<String>,
    pub spawn: Option<String>,
    pub next: String,
}

/// What a value says of how to go on, where it is a step's record: none where it is what an operation made.
///
/// # Errors
/// It is a record, and does not read as one.
pub fn continuation(data: &Data, read: impl FnOnce(&File) -> Result<Vec<u8>>) -> Result<Option<Continuation>> {
    let Data::Typed(typed) = data else { return Ok(None) };
    if typed.ty != RECORD {
        return Ok(None);
    }
    let record: Record = from_bytes(&read(typed.file()?)?)?;
    let needs = record.needs.into_iter().map(|(node, address)| Need { node, address }).collect();
    Ok(Some(Continuation { needs, items: record.items, spawn: record.spawn, next: record.next }))
}

/// What an item made, as a value the host moves and keeps: one file.
fn made_value(bytes: Vec<u8>) -> Data {
    Data::Typed(Typed::new(crate::data::BYTES, vec![File::new("made.cbor", bytes)]))
}

/// The bytes of the one file a value holds.
fn bytes_of(call: &Call<'_>, data: &Data) -> Result<Vec<u8>> {
    let Data::Typed(typed) = data else {
        return Err(anyhow!("a step was handed {}, not what a step makes", data.ty()));
    };
    call.read(typed.file()?)
}

fn is_record(data: &Data) -> bool {
    matches!(data, Data::Typed(typed) if typed.ty == RECORD)
}

/// The record a step was handed: none for the first.
fn record(call: &Call<'_>) -> Result<Option<Record>> {
    match call.inputs.first().and_then(|port| port.first()) {
        Some(data) if is_record(data) => Ok(Some(from_bytes(&bytes_of(call, data)?)?)),
        Some(data) => Err(anyhow!("a step was handed {}, not a step's record", data.ty())),
        None => Ok(None),
    }
}

/// An item, as settings hold it: its CBOR, in base64.
fn item_text(item: &Carried) -> Result<String> {
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD_NO_PAD.encode(to_bytes(item)?))
}

fn item_of(settings: &Table) -> Result<Option<Vec<u8>>> {
    use base64::Engine as _;
    let Some(text) = settings.get(ITEM).and_then(Value::as_str) else { return Ok(None) };
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(text).map_err(|e| anyhow!("its item: {e}"))?;
    Ok(Some(bytes))
}

/// Run a step's code on its call as the code sees it: given the operation's ports alone (those after the step's own, where
/// it has them), with the operation's settings, without the item; what it makes besides its value (`Call::make`) is the
/// call's.
fn as_own<T>(call: &mut Call<'_>, after_own: bool, code: impl FnOnce(&mut Call<'_>) -> T) -> T {
    let mut settings = call.settings.clone();
    settings.remove(ITEM);
    let mut inputs = std::mem::take(&mut call.inputs);
    if after_own {
        inputs.drain(..OWN.min(inputs.len()));
    }
    let mut own = Call {
        carried: Call::carried_by(&inputs),
        inputs,
        settings: &settings,
        made: std::cell::RefCell::new(call.made.take()),
        parameter_text: call.parameter_text,
        host: call.host,
    };
    let done = code(&mut own);
    *call.made.borrow_mut() = own.made.take();
    done
}

/// The names of an operation's steps after the start (`<name>.step<n>`), and of what a cut's items run, where they are a
/// closure (`<name>.cut<n>.each`).
fn step_name(name: &str, at: usize) -> String {
    format!("{name}.step{at}")
}

fn each_name(name: &str, cut: u32) -> String {
    format!("{name}.cut{cut}.each")
}

/// What a step hands its code: what the cut it begins at carried, requested (read now) and spawned (made), and its item.
fn handed(call: &Call<'_>, after_own: bool) -> Result<Handed> {
    let record = if after_own { record(call)? } else { None };
    let joined: Vec<Vec<u8>> = if after_own {
        call.inputs.get(1).into_iter().flatten().map(|made| bytes_of(call, made)).collect::<Result<_>>()?
    } else {
        Vec::new()
    };
    let item = item_of(call.settings)?;
    let (state, needs) = match record {
        Some(record) => {
            let needs: Vec<Need> = record.needs.iter().map(|(node, address)| Need::new(node, address)).collect();
            (record.state, needs)
        }
        None => (Carried::Null, Vec::new()),
    };
    // Each read on its own: one that could not be made is said to what reads it, which may take that.
    let got = needs.iter().map(|need| call.output_of(&need.node, &need.address)).collect();
    Ok(Handed { state, got, needs, joined, item })
}

/// What runs a step - from the start (`after_own` false) or from a cut - as an operation runs: its value what the operation
/// makes, or, ending at a cut, the record the host goes on from.
fn step_run(
    entry: Arc<Entry<Data>>,
    after_own: bool,
    name: Arc<str>,
    spawns: Arc<[Option<String>]>,
    also: Arc<[String]>,
) -> impl Fn(&mut Call<'_>) -> Result<Data> + Send + Sync + 'static {
    move |call| {
        let handed = handed(call, after_own)?;
        match as_own(call, after_own, |own| entry(own, handed))? {
            Flow::Cut(ended) => {
                let at = ended.cut as usize;
                let record = Record {
                    cut: ended.cut,
                    state: ended.state,
                    needs: ended.needs.into_iter().map(|need| (need.node, need.address)).collect(),
                    items: ended.items.iter().map(item_text).collect::<Result<_>>()?,
                    next: step_name(&name, at + 1),
                    spawn: spawns.get(at).cloned().flatten(),
                };
                Ok(Data::Typed(Typed::new(RECORD, vec![File::new("record.cbor", to_bytes(&record)?)])))
            }
            Flow::Done(made) if also.is_empty() => Ok(made),
            Flow::Done(first) => {
                let mut made = call.made.take();
                let mut all = vec![first];
                for name in also.iter() {
                    all.push(made.remove(name).ok_or_else(|| anyhow!("it made no {name}"))?);
                }
                Ok(Data::List(all))
            }
        }
    }
}

/// An operation of what runs in steps after the start: given the record and what was joined, then whatever ports the
/// operation has.
fn stepping<'a>(registry: &'a mut Registry, name: &str, label: &str) -> OpBuilder<'a> {
    registry.op(name).label(label).port_any("record", false).port_any("joined", true)
}

/// Register what runs in steps: each step after the start an operation of its extension (`<name>.step<n>`), and what each
/// cut spawns - a closure's run (`<name>.cut<n>.each`), or a helper in steps, registered once by its own name. Gives the run
/// of its first step, which the operation (or the helper) runs as its own.
///
/// What the operation makes besides its first output (`also`) is what the step finishing it makes (`Call::make`), each in
/// its place after the first, as `OpBuilder::run` hands them on.
fn register(
    registry: &mut Registry,
    name: &str,
    label: &str,
    also: &[String],
    steps: Steps<Data>,
    first_after_own: bool,
) -> impl Fn(&mut Call<'_>) -> Result<Data> + Send + Sync + 'static {
    let spawns: Vec<Option<String>> = steps
        .spawns
        .iter()
        .enumerate()
        .map(|(cut, spawn)| {
            let cut = u32::try_from(cut).unwrap_or(u32::MAX);
            spawn.as_ref().map(|spawn| match spawn {
                Spawn::Each(_) => each_name(name, cut),
                Spawn::Steps { name, .. } => name.clone(),
            })
        })
        .collect();
    let spawns: Arc<[Option<String>]> = spawns.into();
    let (also, own): (Arc<[String]>, Arc<str>) = (also.into(), name.into());
    let mut entries = steps.entries.into_iter();
    let first = entries.next().unwrap_or_else(|| Arc::new(|_: &mut Call<'_>, _| Err(anyhow!("it has no steps"))));
    for (at, entry) in entries.enumerate().map(|(at, entry)| (at + 1, entry)) {
        let run = step_run(entry, true, own.clone(), spawns.clone(), also.clone());
        let _ = stepping(registry, &step_name(name, at), &format!("{label}: step {at}")).run_described(run);
    }
    for (cut, spawn) in steps.spawns.into_iter().enumerate() {
        let cut = u32::try_from(cut).unwrap_or(u32::MAX);
        match spawn {
            Some(Spawn::Each(run)) => {
                let label = format!("{label}: cut {cut}, each");
                let _ = stepping(registry, &each_name(name, cut), &label).run_described(move |call| {
                    let item = item_of(call.settings)?.unwrap_or_default();
                    Ok(made_value(as_own(call, true, |own| run(own, &item))?))
                });
            }
            // Its name taken first, so that a helper calling itself finds it there.
            Some(Spawn::Steps { name: helper, make }) if !registry.has_op(&helper) => {
                let _ = stepping(registry, &helper, &helper)
                    .stepped()
                    .run_described(|_| Err(anyhow!("its steps are not registered yet")));
                let run = register(registry, &helper, &helper, &[], make(), true);
                registry.set_run(&helper, Arc::new(run));
            }
            // A request, or a helper registered already.
            None | Some(Spawn::Steps { .. }) => {}
        }
    }
    step_run(first, first_after_own, own, spawns, also)
}

/// Register an operation in steps as its builder hands it: its steps, giving the run of its first, which is its own.
pub(crate) fn register_operation(
    registry: &mut Registry,
    name: &str,
    label: &str,
    also: &[String],
    steps: Steps<Data>,
) -> impl Fn(&mut Call<'_>) -> Result<Data> + Send + Sync + 'static {
    register(registry, name, label, also, steps, false)
}

/// Code that finds what it reads by running, in steps: run against what was read so far, a read of anything else noted
/// and failing; where anything was noted, the step ends asking for all of it - what it read before and what it found -
/// and the next runs it again, until a run notes nothing new. Its rounds are as many as its reads go deep, each running
/// it whole, so it is for what is cheap until it has what it reads and finds it as it goes, following one read to the
/// next (a token writer following references). What reads in levels is better written in steps (`#[steps]`), each level
/// requested whole and run once.
pub fn replayed<R: 'static>(run: impl Fn(&mut Call<'_>) -> Result<R> + Send + Sync + 'static) -> Steps<R> {
    let turn = move |call: &mut Call<'_>, mut handed: Handed| -> Result<Flow<R>> {
        let known: Vec<(String, String)> = handed.state().unwrap_or_default();
        let got: Vec<Result<Data>> = handed.requested()?;
        let read: std::collections::HashMap<(String, String), std::result::Result<Data, String>> =
            known.iter().cloned().zip(got.into_iter().map(|got| got.map_err(|e| format!("{e:#}")))).collect();
        let replaying = Replaying { inner: call.host, read, missed: std::sync::Mutex::default() };
        let mut replay = Call {
            inputs: std::mem::take(&mut call.inputs),
            settings: call.settings,
            carried: std::mem::take(&mut call.carried),
            made: std::cell::RefCell::new(call.made.take()),
            parameter_text: call.parameter_text,
            host: &replaying,
        };
        let made = run(&mut replay);
        *call.made.borrow_mut() = replay.made.take();
        let missed = replaying.missed.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
        if missed.is_empty() {
            return made.map(Flow::Done);
        }
        let mut wanted = known;
        for pair in missed {
            if !wanted.contains(&pair) {
                wanted.push(pair);
            }
        }
        let needs = wanted.iter().map(|(node, address)| Need::new(node, address)).collect();
        Ended::request(0, &wanted, needs).map(Flow::Cut)
    };
    let turn = Arc::new(turn);
    let again = turn.clone();
    Steps::new()
        .entry(move |call: &mut Call<'_>, handed| turn(call, handed))
        .entry(move |call, handed| again(call, handed))
        .cut()
}

/// What a replayed run is lent of the host: what was read so far served, anything else noted and failing.
struct Replaying<'a> {
    inner: &'a dyn crate::data::CallHost,
    read: std::collections::HashMap<(String, String), std::result::Result<Data, String>>,
    missed: std::sync::Mutex<Vec<(String, String)>>,
}

impl crate::data::CallHost for Replaying<'_> {
    fn path(&self) -> &str {
        self.inner.path()
    }

    fn node_type(&self) -> Option<&str> {
        self.inner.node_type()
    }

    fn read(&self, content: &crate::Content) -> Result<Vec<u8>> {
        self.inner.read(content)
    }

    fn read_range(&self, content: &crate::Content, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.inner.read_range(content, offset, len)
    }

    fn writer(&self, name: &str) -> Result<Box<dyn crate::data::FileWriter>> {
        self.inner.writer(name)
    }

    fn temp(&self) -> Result<String> {
        self.inner.temp()
    }

    fn keep(&self, path: &str, name: &str) -> Result<File> {
        self.inner.keep(path, name)
    }

    fn entries(&self, folder: &str) -> Result<Vec<String>> {
        self.inner.entries(folder)
    }

    fn read_entry(&self, path: &str) -> Result<Vec<u8>> {
        self.inner.read_entry(path)
    }

    fn assets_of(&self, node_type: &str) -> Result<Vec<(String, String)>> {
        self.inner.assets_of(node_type)
    }

    fn place(&self, path: &str) -> Option<String> {
        self.inner.place(path)
    }

    fn output_of(&self, path: &str, address: &str) -> Result<Data> {
        let pair = (path.to_string(), address.to_string());
        if let Some(read) = self.read.get(&pair) {
            return read.clone().map_err(|why| anyhow!("{why}"));
        }
        let mut missed = self.missed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !missed.contains(&pair) {
            missed.push(pair);
        }
        Err(anyhow!("{path}: {address}: not read yet"))
    }

    fn node_named(&self, reference: &str) -> Option<crate::data::NodeNamed> {
        self.inner.node_named(reference)
    }

    fn build_config(&self) -> Option<crate::data::BuildConfig> {
        self.inner.build_config()
    }

    fn path_of(&self, content: &crate::Content) -> Option<String> {
        self.inner.path_of(content)
    }
}
