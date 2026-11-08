//! A bank as its nodes' files, and back, byte for byte: what a bank binds - its instruments at their program numbers,
//! its scripts at their slots, its samples in its pool - and what each of those is.
//!
//! - A sample is a WAV (`wav`): the input Sony's encoder turns into its stored body (`formats::vag`), at the rate its
//!   declaration states, in the mode it states where normal does not reproduce it.
//! - An instrument (`instrument.toml`) is a program: its volume and pan, and its layers, each a sample of the
//!   instrument's own list and how it plays.
//! - A script (what its node is set to, its generated `settings.toml`) is an entry: its controls and its commands, each voice naming an instrument of the
//!   script's own list and stating its ADSR override and pitch envelope as values.
//! - A bank (`bank.toml`) is its id, the slots (each the script at that place of its list, or none), and its image's
//!   layout: its chunks in order - its scripts, ADSR overrides and pitch envelopes - and which chunk a voice names
//!   where that is not the first of equal values. Which instruments, scripts and samples it binds, and in what order,
//!   is its declaration's: positions in those lists are what the files name.

use anyhow::{Context, Result, anyhow, bail};
use formats::spk::{self, Bank, Chunk, Command, Controls, Envelope, Layer, PoolItem, Program, Voice};
use toml::{Table, Value};

/// What the declarations say a bank binds, by position: what makes its files' references.
#[derive(Clone, Debug, Default)]
pub struct Binding {
    /// Each instrument (program, in order): the pool positions of its samples, in its own list's order.
    pub instruments: Vec<Vec<usize>>,
    /// Each script (entry, in image order): the program numbers of its instruments, in its own list's order.
    pub scripts: Vec<Vec<usize>>,
}

impl Binding {
    /// What a bank binds when each instrument and script is its own: an instrument's samples, those its layers
    /// play, in order of first use; a script's instruments, those its voices play.
    #[must_use]
    pub fn own(bank: &Bank) -> Binding {
        let instruments = bank
            .programs
            .iter()
            .map(|p| {
                let mut samples = Vec::new();
                for l in &p.layers {
                    if !samples.contains(&l.sample) {
                        samples.push(l.sample);
                    }
                }
                samples
            })
            .collect();
        let scripts = entries(bank)
            .map(|(_, _, commands)| {
                let mut programs = Vec::new();
                for c in commands {
                    if let Command::Voice(v) = c
                        && !programs.contains(&usize::from(v.program))
                    {
                        programs.push(usize::from(v.program));
                    }
                }
                programs
            })
            .collect();
        Binding { instruments, scripts }
    }
}

fn entries(bank: &Bank) -> impl Iterator<Item = (usize, &Controls, &Vec<Command>)> {
    bank.chunks.iter().enumerate().filter_map(|(i, c)| match c {
        Chunk::Entry { controls, commands } => Some((i, controls, commands)),
        _ => None,
    })
}

fn int(v: impl Into<i64>) -> Value {
    Value::Integer(v.into())
}

/// A position, as a file states it.
fn at(v: usize) -> Value {
    Value::Integer(i64::try_from(v).unwrap_or(i64::MAX))
}

fn get(t: &Table, key: &str) -> Result<i64> {
    t.get(key).and_then(Value::as_integer).with_context(|| format!("no {key}"))
}

fn narrow<T: TryFrom<i64>>(t: &Table, key: &str) -> Result<T> {
    T::try_from(get(t, key)?).map_err(|_| anyhow!("{key} is out of range"))
}

fn adsr_value(adsr1: u16, adsr2: u16) -> Value {
    Value::Array(vec![int(adsr1), int(adsr2)])
}

fn envelope_table(e: &Envelope) -> Table {
    let mut t = Table::new();
    for (k, v) in [
        ("delay", i64::from(e.delay)),
        ("attack_level", i64::from(e.attack_level)),
        ("attack_updates", i64::from(e.attack_updates)),
        ("hold", i64::from(e.hold)),
        ("decay_updates", i64::from(e.decay_updates)),
        ("sustain_level", i64::from(e.sustain_level)),
        ("release_level", i64::from(e.release_level)),
        ("release_updates", i64::from(e.release_updates)),
    ] {
        t.insert(k.into(), int(v));
    }
    t
}

fn envelope_of(t: &Table) -> Result<Envelope> {
    Ok(Envelope {
        delay: narrow(t, "delay")?,
        attack_level: narrow(t, "attack_level")?,
        attack_updates: narrow(t, "attack_updates")?,
        hold: narrow(t, "hold")?,
        decay_updates: narrow(t, "decay_updates")?,
        sustain_level: narrow(t, "sustain_level")?,
        release_level: narrow(t, "release_level")?,
        release_updates: narrow(t, "release_updates")?,
    })
}

/// An instrument's file: a program, its layers' samples as positions in its own list.
fn instrument(program: &Program, samples: &[usize]) -> Result<Table> {
    let mut t = Table::new();
    t.insert("volume".into(), int(program.volume));
    t.insert("pan".into(), int(program.pan));
    let mut layers = Vec::new();
    for l in &program.layers {
        let mut lt = Table::new();
        let sample = samples.iter().position(|&s| s == l.sample).context("a layer's sample is not the instrument's")?;
        lt.insert("sample".into(), at(sample));
        for (k, v) in [
            ("key_min", i64::from(l.key_min)),
            ("key_max", i64::from(l.key_max)),
            ("root_key", i64::from(l.root_key)),
            ("fine_tune", i64::from(l.fine_tune)),
            ("volume", i64::from(l.volume)),
            ("pan", i64::from(l.pan)),
            ("priority", i64::from(l.priority)),
            ("bend_down", i64::from(l.bend_down)),
            ("bend_up", i64::from(l.bend_up)),
            ("adsr1", i64::from(l.adsr1)),
            ("adsr2", i64::from(l.adsr2)),
            ("reverb", i64::from(l.reverb)),
        ] {
            lt.insert(k.into(), int(v));
        }
        layers.push(Value::Table(lt));
    }
    t.insert("layers".into(), Value::Array(layers));
    Ok(t)
}

fn program_of(t: &Table, samples: &[usize]) -> Result<Program> {
    let layers = t.get("layers").and_then(Value::as_array).context("no layers")?;
    let layers = layers
        .iter()
        .map(|l| {
            let l = l.as_table().context("a layer is no table")?;
            let at: usize = narrow(l, "sample")?;
            Ok(Layer {
                sample: *samples.get(at).with_context(|| format!("no sample {at} in the instrument's list"))?,
                key_min: narrow(l, "key_min")?,
                key_max: narrow(l, "key_max")?,
                root_key: narrow(l, "root_key")?,
                fine_tune: narrow(l, "fine_tune")?,
                volume: narrow(l, "volume")?,
                pan: narrow(l, "pan")?,
                priority: narrow(l, "priority")?,
                bend_down: narrow(l, "bend_down")?,
                bend_up: narrow(l, "bend_up")?,
                adsr1: narrow(l, "adsr1")?,
                adsr2: narrow(l, "adsr2")?,
                reverb: narrow(l, "reverb")?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Program { volume: narrow(t, "volume")?, pan: narrow(t, "pan")?, layers })
}

/// A script's file: an entry, its voices' instruments as positions in its own list (`programs`, their program
/// numbers), their ADSR overrides and envelopes as values.
fn script(bank: &Bank, controls: &Controls, commands: &[Command], programs: &[usize]) -> Result<Table> {
    let mut t = Table::new();
    for (k, v) in [
        ("volume", i64::from(controls.volume)),
        ("pan", i64::from(controls.pan)),
        ("instances", i64::from(controls.max_instances)),
        ("retrigger", i64::from(controls.retrigger)),
        ("priority", i64::from(controls.priority)),
        ("flags", i64::from(controls.flags)),
        ("unknown_4", i64::from(controls.unknown_4)),
    ] {
        t.insert(k.into(), int(v));
    }
    let mut out = Vec::new();
    for c in commands {
        let mut ct = Table::new();
        match c {
            Command::Voice(v) => {
                let vt = &mut ct;
                vt.insert("op".into(), Value::String("voice".into()));
                let instrument = programs
                    .iter()
                    .position(|&p| p == usize::from(v.program))
                    .context("a voice's program is not the script's")?;
                vt.insert("instrument".into(), at(instrument));
                for (k, val) in [
                    ("layer", i64::from(v.layer)),
                    ("delay", i64::from(v.delay)),
                    ("gate", i64::from(v.gate)),
                    ("pan", i64::from(v.pan)),
                    ("volume", i64::from(v.volume)),
                    ("reverb", i64::from(v.reverb)),
                    ("priority", i64::from(v.priority)),
                    ("pitch", i64::from(v.pitch)),
                ] {
                    vt.insert(k.into(), int(val));
                }
                if v.bank != 0 {
                    vt.insert("bank".into(), int(v.bank));
                }
                if let Some(a) = v.adsr {
                    let Chunk::Adsr { adsr1, adsr2 } = &bank.chunks[a] else {
                        bail!("a voice's ADSR is no ADSR chunk")
                    };
                    vt.insert("adsr".into(), adsr_value(*adsr1, *adsr2));
                }
                if let Some(e) = v.envelope {
                    let Chunk::Envelope(e) = &bank.chunks[e] else { bail!("a voice's envelope is no envelope chunk") };
                    vt.insert("envelope".into(), Value::Table(envelope_table(e)));
                }
            }
            Command::Loop { repeat, delay } => {
                ct.insert("op".into(), Value::String("loop".into()));
                ct.insert("repeat".into(), int(*repeat));
                ct.insert("delay".into(), int(*delay));
            }
            Command::Wait { ticks } => {
                ct.insert("op".into(), Value::String("wait".into()));
                ct.insert("ticks".into(), int(*ticks));
            }
            Command::EndLoop => {
                ct.insert("op".into(), Value::String("end_loop".into()));
            }
        }
        out.push(Value::Table(ct));
    }
    t.insert("commands".into(), Value::Array(out));
    Ok(t)
}

/// A chunk's value, to tell an ADSR override or an envelope by.
#[derive(Clone, PartialEq)]
enum Shared {
    Adsr(u16, u16),
    Envelope(Envelope),
}

fn shared_of(chunk: &Chunk) -> Option<Shared> {
    match chunk {
        Chunk::Adsr { adsr1, adsr2 } => Some(Shared::Adsr(*adsr1, *adsr2)),
        Chunk::Envelope(e) => Some(Shared::Envelope(e.clone())),
        Chunk::Entry { .. } => None,
    }
}

/// The bank's own file: its id, slots, layout and the voices naming another chunk than the first equal one.
fn bank_file(bank: &Bank) -> Table {
    let entry_of: Vec<Option<usize>> = {
        let mut n = 0;
        bank.chunks
            .iter()
            .map(|c| {
                matches!(c, Chunk::Entry { .. }).then(|| {
                    n += 1;
                    n - 1
                })
            })
            .collect()
    };
    let mut t = Table::new();
    t.insert("bank_id".into(), int(bank.bank_id));
    t.insert(
        "slots".into(),
        Value::Array(bank.slots.iter().map(|s| s.and_then(|c| entry_of[c]).map_or(Value::Integer(-1), at)).collect()),
    );
    let mut layout = Vec::new();
    for (i, c) in bank.chunks.iter().enumerate() {
        let mut lt = Table::new();
        match c {
            Chunk::Entry { .. } => {
                lt.insert("script".into(), at(entry_of[i].expect("an entry")));
            }
            Chunk::Adsr { adsr1, adsr2 } => {
                lt.insert("adsr".into(), adsr_value(*adsr1, *adsr2));
            }
            Chunk::Envelope(e) => {
                lt.insert("envelope".into(), Value::Table(envelope_table(e)));
            }
        }
        layout.push(Value::Table(lt));
    }
    // Written where it is not what the scripts' values make it.
    let scripts: Vec<Vec<Option<Values>>> = entries(bank)
        .map(|(_, _, commands)| {
            commands
                .iter()
                .map(|c| match c {
                    Command::Voice(v) => Some(Values {
                        adsr: v.adsr.and_then(|a| shared_of(&bank.chunks[a])),
                        envelope: v.envelope.and_then(|e| shared_of(&bank.chunks[e])),
                    }),
                    _ => None,
                })
                .collect()
        })
        .collect();
    let items: Vec<Item> = bank
        .chunks
        .iter()
        .enumerate()
        .map(|(i, c)| shared_of(c).map_or_else(|| Item::Script(entry_of[i].expect("an entry")), Item::Value))
        .collect();
    if items != default_layout(&scripts) {
        t.insert("layout".into(), Value::Array(layout));
    }
    // Voices whose chunk is not the first of its value.
    let first = |chunk: usize| {
        let value = shared_of(&bank.chunks[chunk]);
        bank.chunks.iter().position(|c| shared_of(c) == value).unwrap_or(chunk)
    };
    let mut apart = Vec::new();
    for (i, _, commands) in entries(bank) {
        for (n, c) in commands.iter().enumerate() {
            if let Command::Voice(v) = c {
                for (key, chunk) in [("adsr", v.adsr), ("envelope", v.envelope)] {
                    if let Some(chunk) = chunk.filter(|&c| first(c) != c) {
                        let mut voice = Table::new();
                        voice.insert("script".into(), at(entry_of[i].expect("an entry")));
                        voice.insert("command".into(), at(n));
                        voice.insert(key.into(), at(chunk));
                        apart.push(Value::Table(voice));
                    }
                }
            }
        }
    }
    if !apart.is_empty() {
        t.insert("voice_chunks".into(), Value::Array(apart));
    }
    t
}

/// A bank's nodes' files, by the binding its declarations give: the bank's, each instrument's and each script's, as
/// tables; the samples' bodies, by pool position.
///
/// # Errors
/// The binding does not fit the bank.
pub fn split(bank: &Bank, binding: &Binding) -> Result<(Table, Vec<Table>, Vec<Table>)> {
    anyhow::ensure!(binding.instruments.len() == bank.programs.len(), "the binding's instruments are not the bank's");
    let instruments =
        bank.programs.iter().zip(&binding.instruments).map(|(p, s)| instrument(p, s)).collect::<Result<_>>()?;
    let scripts: Vec<Table> = entries(bank)
        .zip(&binding.scripts)
        .map(|((_, controls, commands), programs)| script(bank, controls, commands, programs))
        .collect::<Result<_>>()?;
    anyhow::ensure!(scripts.len() == binding.scripts.len(), "the binding's scripts are not the bank's");
    Ok((bank_file(bank), instruments, scripts))
}

/// A voice's ADSR override and envelope as values, before they are placed among the bank's chunks.
struct Values {
    adsr: Option<Shared>,
    envelope: Option<Shared>,
}

fn values_of(v: &Table) -> Result<Values> {
    let adsr = match v.get("adsr") {
        None => None,
        Some(Value::Array(a)) => {
            let p =
                |i: usize| a.get(i).and_then(Value::as_integer).and_then(|v| u16::try_from(v).ok()).context("an ADSR");
            Some(Shared::Adsr(p(0)?, p(1)?))
        }
        Some(_) => bail!("an ADSR is two halfwords"),
    };
    let envelope = match v.get("envelope") {
        None => None,
        Some(Value::Table(e)) => Some(Shared::Envelope(envelope_of(e)?)),
        Some(_) => bail!("an envelope is a table"),
    };
    Ok(Values { adsr, envelope })
}

/// A command of a script's file, its voice's values apart.
fn command_of(c: &Value, programs: &[usize]) -> Result<(Command, Option<Values>)> {
    let c = c.as_table().context("a command is no table")?;
    let op = c.get("op").and_then(Value::as_str).context("a command has no op")?;
    Ok(match op {
        "voice" => {
            let instrument: usize = narrow(c, "instrument")?;
            let voice = Voice {
                bank: c.get("bank").and_then(Value::as_integer).map_or(Ok(0), u16::try_from)?,
                program: u8::try_from(*programs.get(instrument).context("no such instrument in the script's list")?)?,
                layer: narrow(c, "layer")?,
                delay: narrow(c, "delay")?,
                gate: narrow(c, "gate")?,
                pan: narrow(c, "pan")?,
                volume: narrow(c, "volume")?,
                reverb: narrow(c, "reverb")?,
                priority: narrow(c, "priority")?,
                adsr: None,
                pitch: narrow(c, "pitch")?,
                envelope: None,
            };
            (Command::Voice(voice), Some(values_of(c)?))
        }
        "loop" => (Command::Loop { repeat: narrow(c, "repeat")?, delay: narrow(c, "delay")? }, None),
        "wait" => (Command::Wait { ticks: narrow(c, "ticks")? }, None),
        "end_loop" => (Command::EndLoop, None),
        other => bail!("{other} is none of voice, loop, wait, end_loop"),
    })
}

/// What a bank's image holds, in order: a script, or an ADSR override or envelope by its value.
#[derive(Clone, PartialEq)]
enum Item {
    Script(usize),
    Value(Shared),
}

/// The layout a bank has unless its file says otherwise: its scripts in order, then each ADSR override and envelope
/// its voices name, in order of first use.
fn default_layout(scripts: &[Vec<Option<Values>>]) -> Vec<Item> {
    let mut layout: Vec<Item> = (0..scripts.len()).map(Item::Script).collect();
    for values in scripts.iter().flatten().flatten() {
        for value in [&values.adsr, &values.envelope].into_iter().flatten() {
            let item = Item::Value(value.clone());
            if !layout.contains(&item) {
                layout.push(item);
            }
        }
    }
    layout
}

fn layout_item(item: &Value) -> Result<Item> {
    let item = item.as_table().context("a layout item is no table")?;
    if let Some(s) = item.get("script").and_then(Value::as_integer) {
        return Ok(Item::Script(usize::try_from(s)?));
    }
    let values = values_of(item)?;
    values.adsr.or(values.envelope).map(Item::Value).context("a layout item is none of script, adsr, envelope")
}

/// A bank from its nodes' files, by the same binding, and its pool.
///
/// # Errors
/// A file does not say what it should.
pub fn join(
    file: &Table,
    instruments: &[Table],
    scripts: &[Table],
    binding: &Binding,
    pool: Vec<PoolItem>,
) -> Result<Bank> {
    let programs = instruments
        .iter()
        .zip(&binding.instruments)
        .map(|(t, samples)| program_of(t, samples))
        .collect::<Result<Vec<_>>>()?;
    let mut parsed = Vec::new();
    for (t, programs) in scripts.iter().zip(&binding.scripts) {
        // A script set to nothing yet is a silent one: full volume, centered, one instance, no commands.
        let or = |key: &str, default: i64| -> Result<i64> { t.get(key).map_or(Ok(default), |_| get(t, key)) };
        let controls = Controls {
            volume: u8::try_from(or("volume", 127)?)?,
            pan: u8::try_from(or("pan", 64)?)?,
            max_instances: u8::try_from(or("instances", 1)?)?,
            retrigger: i16::try_from(or("retrigger", 0)?)?,
            priority: u16::try_from(or("priority", 0)?)?,
            flags: u16::try_from(or("flags", 0)?)?,
            unknown_4: u8::try_from(or("unknown_4", 0)?)?,
        };
        let commands = t.get("commands").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        let commands = commands.iter().map(|c| command_of(c, programs)).collect::<Result<Vec<_>>>()?;
        parsed.push((controls, commands));
    }
    let values: Vec<Vec<Option<Values>>> = parsed
        .iter()
        .map(|(_, commands)| {
            commands
                .iter()
                .map(|(_, v)| v.as_ref().map(|v| Values { adsr: v.adsr.clone(), envelope: v.envelope.clone() }))
                .collect()
        })
        .collect();
    let layout = match file.get("layout").and_then(Value::as_array) {
        Some(items) => items.iter().map(layout_item).collect::<Result<Vec<_>>>()?,
        None => default_layout(&values),
    };
    let apart: Vec<(usize, usize, String, usize)> = file
        .get("voice_chunks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_table)
        .map(|t| {
            let key = if t.contains_key("adsr") { "adsr" } else { "envelope" };
            Ok((narrow(t, "script")?, narrow(t, "command")?, key.to_string(), narrow(t, key)?))
        })
        .collect::<Result<_>>()?;
    let chunk_of = |s: usize, n: usize, key: &str, value: Option<&Shared>| -> Result<Option<usize>> {
        if let Some(&(_, _, _, chunk)) = apart.iter().find(|a| a.0 == s && a.1 == n && a.2 == key) {
            return Ok(Some(chunk));
        }
        let Some(value) = value else { return Ok(None) };
        let at = layout.iter().position(|i| *i == Item::Value(value.clone()));
        at.map(Some).with_context(|| format!("no {key} chunk of a voice's value in the layout"))
    };
    let mut entries: Vec<Option<Chunk>> = parsed
        .into_iter()
        .enumerate()
        .map(|(s, (controls, commands))| {
            let commands = commands
                .into_iter()
                .enumerate()
                .map(|(n, (command, values))| {
                    Ok(match (command, values) {
                        (Command::Voice(mut v), Some(values)) => {
                            v.adsr = chunk_of(s, n, "adsr", values.adsr.as_ref())?;
                            v.envelope = chunk_of(s, n, "envelope", values.envelope.as_ref())?;
                            Command::Voice(v)
                        }
                        (command, _) => command,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Some(Chunk::Entry { controls, commands }))
        })
        .collect::<Result<_>>()?;
    let mut chunks = Vec::new();
    let mut entry_chunks = vec![0; entries.len()];
    for item in &layout {
        chunks.push(match item {
            Item::Script(s) => {
                *entry_chunks.get_mut(*s).with_context(|| format!("no script {s}"))? = chunks.len();
                entries.get_mut(*s).and_then(Option::take).with_context(|| format!("script {s} is laid out twice"))?
            }
            Item::Value(Shared::Adsr(adsr1, adsr2)) => Chunk::Adsr { adsr1: *adsr1, adsr2: *adsr2 },
            Item::Value(Shared::Envelope(e)) => Chunk::Envelope(e.clone()),
        });
    }
    anyhow::ensure!(entries.iter().all(Option::is_none), "a script is not laid out");
    let slots = file
        .get("slots")
        .and_then(Value::as_array)
        .context("no slots")?
        .iter()
        .map(|s| {
            let s = s.as_integer().context("a slot is no number")?;
            Ok(if s < 0 { None } else { Some(*entry_chunks.get(usize::try_from(s)?).context("no such script")?) })
        })
        .collect::<Result<_>>()?;
    Ok(Bank { bank_id: narrow(file, "bank_id")?, programs, chunks, slots, pool })
}

/// A file's text: its plain values, then each list of tables one table a line.
#[must_use]
pub fn text(table: &Table) -> String {
    let mut lines = Vec::new();
    for (k, v) in table {
        match v {
            Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_table) => {
                lines.push(format!("{k} = ["));
                lines.extend(items.iter().map(|item| format!("    {item},")));
                lines.push("]".into());
            }
            other => lines.push(format!("{k} = {other}")),
        }
    }
    lines.join("\n") + "\n"
}

/// A bank's bytes, read into its parts and checked to write back the same.
///
/// # Errors
/// It is no bank, or does not write back as it is.
pub fn read(blob: &[u8]) -> Result<Bank> {
    let bank = spk::decode(blob).map_err(|e| anyhow!(e))?;
    anyhow::ensure!(spk::encode(&bank) == blob, "the bank does not write back as it is");
    Ok(bank)
}

#[cfg(test)]
mod tests {
    use super::{Binding, join, read, split};

    /// The banks in a folder (`PE2_SOUND_BANKS`): each split into its nodes' files and joined back byte for byte.
    #[test]
    fn a_bank_is_its_nodes_files() {
        let Ok(folder) = std::env::var("PE2_SOUND_BANKS") else { return };
        for entry in std::fs::read_dir(folder).unwrap() {
            let blob = std::fs::read(entry.unwrap().path()).unwrap();
            let bank = read(&blob).unwrap();
            let binding = Binding::own(&bank);
            let (file, instruments, scripts) = split(&bank, &binding).unwrap();
            // Through text, as the files are written.
            let text = |t: &toml::Table| toml::from_str::<toml::Table>(&super::text(t)).unwrap();
            let instruments: Vec<_> = instruments.iter().map(text).collect();
            let scripts: Vec<_> = scripts.iter().map(text).collect();
            let joined = join(&text(&file), &instruments, &scripts, &binding, bank.pool.clone()).unwrap();
            assert_eq!(formats::spk::encode(&joined), blob);
        }
    }
}
