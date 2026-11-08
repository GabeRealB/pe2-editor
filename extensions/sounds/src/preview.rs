//! A script and a bank's slot played as the game plays them (`engine`): a script with its instruments and samples
//! alone, as a bank of them; a bank's slot with the bank they are in, as the game loads it. What each node names it
//! reads through its references: a bank's instruments, scripts and samples, an instrument's samples, a script's
//! instruments, each its native output and the references that came with it.

use pe2_ext::media::{AudioData, MediaBuffer, SampleFormat};
use pe2_ext::{Asset, Call, ContentRow, Contents, Data, Generator, Icon, Result, Table, Tint, Typed, Value, anyhow};

use formats::spk::Bank;

use crate::{bank, engine};

/// How long a script that does not end is played before it is stopped, in seconds.
pub const LIMIT: f64 = 10.0;

/// Where a script is heard: the project's reverb mode (the first `reverb` node), and the area's reverb setting this
/// preview is set to (`room`, the game's default where none is).
fn room(call: &Call<'_>) -> Result<engine::Room> {
    let setting = call.settings.get("room").and_then(Value::as_integer);
    let setting = setting.and_then(|s| i8::try_from(s.clamp(-1, 3)).ok()).unwrap_or(engine::ROOM);
    let mode = match call.assets_of(crate::Reverb::NODE)?.first() {
        Some((path, _)) => Some(crate::reverb::Mode::of(&call.node(path)?.0)?),
        None => None,
    };
    Ok(engine::Room { mode, setting })
}

/// Where the nodes a script or a bank names are read: by an operation, their outputs; by a view, their files.
pub trait Nodes {
    /// A node's file, as a table, and the references it holds.
    ///
    /// # Errors
    /// It cannot be read.
    fn node(&self, reference: &str) -> Result<(Table, Table)>;
    /// A sample's body, by its node.
    ///
    /// # Errors
    /// It cannot be read or made.
    fn body(&self, reference: &str) -> Result<formats::spk::PoolItem>;
}

impl Nodes for Call<'_> {
    fn node(&self, reference: &str) -> Result<(Table, Table)> {
        let Data::Typed(typed) = self.output_of(reference, "native")? else { anyhow::bail!("{reference}: no file") };
        // A node holding only what it is set to (a script) is that.
        if typed.files.is_empty() {
            return Ok((typed.settings.clone(), typed.settings));
        }
        Ok((table_of(self, &typed)?, typed.settings))
    }

    fn body(&self, reference: &str) -> Result<formats::spk::PoolItem> {
        let Data::Typed(typed) = self.output_of(reference, "native")? else { anyhow::bail!("{reference}: no body") };
        Ok(formats::spk::PoolItem::Sample(self.read(typed.file()?)?))
    }
}

/// The nodes as a view reads them: their stored files, a sample's WAV made into its body as its import makes it.
pub struct Looked<'a>(pub &'a dyn pe2_ext::Project);

impl Looked<'_> {
    fn asset(&self, reference: &str) -> Result<Asset<'_>> {
        self.0.resolve(&Value::String(reference.to_string())).ok_or_else(|| anyhow!("no node {reference}"))
    }
}

impl Nodes for Looked<'_> {
    fn node(&self, reference: &str) -> Result<(Table, Table)> {
        let asset = self.asset(reference)?;
        // A node holding only what it is set to (a script) is that.
        let Some(name) = FILES.iter().find(|name| asset.has_file(name)) else {
            return Ok((asset.fields().clone(), asset.fields().clone()));
        };
        let table = toml::from_str(&String::from_utf8(asset.read(name)?)?)?;
        Ok((table, asset.fields().clone()))
    }

    fn body(&self, reference: &str) -> Result<formats::spk::PoolItem> {
        let asset = self.asset(reference)?;
        let mode = asset.field("mode").and_then(Value::as_str).unwrap_or("normal");
        Ok(formats::spk::PoolItem::Sample(crate::body_of_wav(&asset.read("sound.wav")?, mode)?))
    }
}

/// The files the nodes a script or bank names are stored as.
const FILES: [&str; 3] = ["instrument.toml", "bank.toml", "reverb.toml"];

/// A node's file, as an operation is given it, as a table.
///
/// # Errors
/// It is no TOML.
pub fn table_of(call: &Call<'_>, typed: &Typed) -> Result<Table> {
    Ok(toml::from_str(&String::from_utf8(call.read(typed.file()?)?)?)?)
}

/// The references a node holds under a key, as uids.
#[must_use]
pub fn references(settings: &Table, key: &str) -> Vec<String> {
    settings
        .get(key)
        .and_then(Value::as_array)
        .map_or_default(|items| items.iter().filter_map(|v| pe2_ext::target_of(v).map(str::to_string)).collect())
}

/// Where each of `named` is in `among`: the first place it is.
fn places(named: &[String], among: &[String], what: &str) -> Result<Vec<usize>> {
    named
        .iter()
        .map(|n| among.iter().position(|a| a == n).ok_or_else(|| anyhow!("{n} is none of the {what} it is with")))
        .collect()
}

/// A script with its own instruments and samples, as a bank of them, and the chunk it is.
///
/// # Errors
/// A node it names cannot be read.
pub fn script_bank(nodes: &dyn Nodes, script: Table, settings: &Table) -> Result<(Bank, usize)> {
    let mut instruments = Vec::new();
    let mut samples: Vec<String> = Vec::new();
    let mut binding = bank::Binding::default();
    for reference in references(settings, "instruments") {
        let (instrument, its) = nodes.node(&reference)?;
        let own = references(&its, "samples");
        for sample in &own {
            if !samples.contains(sample) {
                samples.push(sample.clone());
            }
        }
        binding.instruments.push(places(&own, &samples, "samples")?);
        instruments.push(instrument);
    }
    binding.scripts.push((0..instruments.len()).collect());
    let pool = samples.iter().map(|s| nodes.body(s)).collect::<Result<_>>()?;
    let mut file = Table::new();
    file.insert("bank_id".into(), Value::Integer(0));
    file.insert("slots".into(), Value::Array(vec![Value::Integer(0)]));
    let bank = bank::join(&file, &instruments, &[script], &binding, pool)?;
    let entry = bank.slots.first().copied().flatten().ok_or_else(|| anyhow!("no script"))?;
    Ok((bank, entry))
}

/// A bank with what it binds, and the chunk a slot plays.
///
/// # Errors
/// The bank has no such slot, or a node it names cannot be read.
pub fn slot_bank(nodes: &dyn Nodes, file: &Table, settings: &Table, slot: usize) -> Result<(Bank, usize)> {
    let (instrument_refs, script_refs, sample_refs) =
        (references(settings, "instruments"), references(settings, "scripts"), references(settings, "samples"));
    let mut binding = bank::Binding::default();
    let mut instruments = Vec::new();
    for reference in &instrument_refs {
        let (instrument, its) = nodes.node(reference)?;
        binding.instruments.push(places(&references(&its, "samples"), &sample_refs, "samples")?);
        instruments.push(instrument);
    }
    let mut scripts = Vec::new();
    for reference in &script_refs {
        let (script, its) = nodes.node(reference)?;
        binding.scripts.push(places(&references(&its, "instruments"), &instrument_refs, "instruments")?);
        scripts.push(script);
    }
    let pool = sample_refs.iter().map(|s| nodes.body(s)).collect::<Result<_>>()?;
    let bank = bank::join(file, &instruments, &scripts, &binding, pool)?;
    let entry = bank.slots.get(slot).copied().flatten().ok_or_else(|| anyhow!("slot {slot} plays nothing"))?;
    Ok((bank, entry))
}

/// A script played with its own instruments and samples, as a bank of them.
///
/// # Errors
/// A node it names cannot be read.
pub fn script(call: &Call<'_>, typed: &Typed) -> Result<Vec<Vec<i16>>> {
    let settings = pe2_ext::merged(&typed.settings, call.settings);
    let (bank, entry) = script_bank(call, settings.clone(), &settings)?;
    engine::play(&bank, entry, LIMIT, &room(call)?)
}

/// A bank's slot played with the bank its script is in.
///
/// # Errors
/// The bank has no such slot, or a node it names cannot be read.
pub fn slot(call: &Call<'_>, typed: &Typed, slot: usize) -> Result<Vec<Vec<i16>>> {
    let settings = pe2_ext::merged(&typed.settings, call.settings);
    let (bank, entry) = slot_bank(call, &table_of(call, typed)?, &settings, slot)?;
    engine::play(&bank, entry, LIMIT, &room(call)?)
}

/// The slots of a bank that play a script, as its file has them: each its number, and the script's place in the bank's
/// list.
pub fn slots(asset: &Asset) -> Vec<(usize, usize)> {
    let Some(file) = asset.read("bank.toml").ok().and_then(|b| String::from_utf8(b).ok()) else { return vec![] };
    let Ok(file) = toml::from_str::<Table>(&file) else { return vec![] };
    let slots = file.get("slots").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    slots.iter().enumerate().filter_map(|(n, s)| Some((n, usize::try_from(s.as_integer()?).ok()?))).collect()
}

/// A bank's contents: its slots, each shown as the script it plays; choosing one plays it.
pub fn contents(asset: &Asset, project: &dyn pe2_ext::Project) -> Contents {
    let scripts = asset.array("scripts");
    let mut rows = vec![ContentRow::heading("slots", "Slots")];
    for (slot, script) in slots(asset) {
        let named = scripts.get(script).and_then(|r| project.resolve(r));
        let label = named.as_ref().map_or_else(
            || format!("script {script}"),
            |a| a.path().rsplit('/').next().unwrap_or_default().to_string(),
        );
        let mut row = ContentRow::new(&format!("slot/{slot}"), &format!("Slot {slot}"))
            .depth(1)
            .icon(Icon::Music, Tint::Sound)
            .detail(&label)
            .look(&slot.to_string());
        if let Some(named) = named {
            row = row.link(named.path());
        }
        rows.push(row);
    }
    Contents { rows, ..Contents::default() }
}

/// What was played, an audio update an item: stereo at the SPU's rate.
pub struct Played {
    updates: Vec<Vec<i16>>,
    at: usize,
    since: u64,
}

impl Played {
    #[must_use]
    pub fn new(updates: Vec<Vec<i16>>) -> Played {
        Played { updates, at: 0, since: 0 }
    }
}

impl Generator for Played {
    fn header(&self) -> Table {
        let mut header = Table::new();
        header.insert("rate".into(), Value::Float(engine::UPDATES));
        header.insert("count".into(), Value::Integer(i64::try_from(self.updates.len()).unwrap_or(0)));
        header.insert("holds".into(), Value::String("an audio update of what plays".into()));
        header
    }

    fn next(&mut self, _: &Call<'_>, count: usize) -> Result<Vec<Data>> {
        let mut items = Vec::new();
        while items.len() < count && self.at < self.updates.len() {
            let samples = &self.updates[self.at];
            let timestamp = self.since * 1_000_000 / u64::from(crate::spu::RATE);
            self.since += (samples.len() / 2) as u64;
            items.push(Data::Audio(AudioData {
                buffer: MediaBuffer::new(samples.iter().flat_map(|s| s.to_le_bytes()).collect()),
                format: SampleFormat::S16Interleaved,
                sample_rate: crate::spu::RATE,
                channels: 2,
                timestamp,
            }));
            self.at += 1;
        }
        Ok(items)
    }

    fn seek(&mut self, _: &Call<'_>, position: u64) -> Result<bool> {
        self.at = usize::try_from(position).unwrap_or(usize::MAX).min(self.updates.len());
        self.since = self.updates[..self.at].iter().map(|u| (u.len() / 2) as u64).sum();
        Ok(true)
    }
}
