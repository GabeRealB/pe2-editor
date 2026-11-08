//! A sound bank (`hSPK`, a sound-script bank) as its parts, and back, byte for byte.
//!
//! A bank is, in file order:
//!
//! ```text
//! 0x000  header (0x14), the program table (4 bytes each), the layer table (0x14 each); zeros to 0x800
//! 0x800  the script image: `hONE`, bank id, slot count, the slot table (padded to 4 bytes), then its chunks
//!        (`oneC` entry headers, `oneV` voices, `Loop`/`Wait`/`endL`/`endC` flow, `oneA` ADSR overrides, `oneE`
//!        pitch envelopes); zeros to the next 0x800 boundary
//! pool   the samples back to back, each a VAG body as Sony's encoder writes it (`vag`); `waveBytes` long;
//!        zeros to the file's end, the pool end's sector floor + 0x1000 - 16
//! ```
//!
//! The parts: programs and their layers (each naming a sample of the pool), the image's chunks - an entry (an event:
//! its controls and its commands, voices playing a program's layer, loops and waits), an ADSR override, a pitch
//! envelope - the slots (the events a sound id names, by entry number), and the pool. Everything else is derived, and
//! reading checks it is what the derivation gives: counts, sizes, offsets, wave addresses, padding, the header's
//! sector counts and sample count; an envelope's slopes (it is authored as levels and durations); bytes always 0 or
//! 0xFF. Stored and read by nothing: an envelope's release updates, and an entry's byte 4 (a voice count written once
//! and never updated). An entry the tool laid out with no commands has no `endC`. A pool slot the tool reserved and
//! never wrote is a raw item.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "the bank's fields: counts and sectors of a byte, offsets and sizes of a halfword or a word, halfwords and \
              bytes the game reads as signed - each value within its field, as the bank stores it"
)]

use std::collections::BTreeMap;

const HEADER: usize = 0x14;
const IMAGE_AT: usize = 0x800;
const SECTOR: usize = 0x800;

fn align(n: usize, a: usize) -> usize {
    (n + a - 1) & !(a - 1)
}

/// A program's layer: a sample of the pool and how it plays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    pub sample: usize,
    pub volume: u8,
    pub pan: u8,
    pub root_key: u8,
    pub fine_tune: u8,
    pub priority: u16,
    pub key_min: u8,
    pub key_max: u8,
    pub bend_down: u8,
    pub bend_up: u8,
    pub adsr1: u16,
    pub adsr2: u16,
    pub reverb: u8,
}

/// A program: its layers, and the volume and pan they play at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub volume: u8,
    pub pan: u8,
    pub layers: Vec<Layer>,
}

/// An event's controls (`oneC`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Controls {
    /// A voice count written once and read by nothing.
    pub unknown_4: u8,
    pub volume: u8,
    pub pan: u8,
    pub max_instances: u8,
    pub retrigger: i16,
    pub priority: u16,
    pub flags: u16,
}

/// A voice (`oneV`): a program's layer played, its ADSR and pitch envelope chunks where it has its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Voice {
    pub bank: u16,
    pub program: u8,
    pub layer: u8,
    pub delay: u16,
    pub gate: u16,
    pub pan: i8,
    pub volume: i8,
    pub reverb: i8,
    pub priority: u16,
    pub adsr: Option<usize>,
    pub pitch: i16,
    pub envelope: Option<usize>,
}

/// What an event does, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Voice(Voice),
    Loop { repeat: u8, delay: u16 },
    Wait { ticks: i32 },
    EndLoop,
}

/// A pitch envelope as authored: levels and durations, its slopes derived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub delay: i16,
    pub attack_level: i16,
    pub attack_updates: u16,
    pub hold: i16,
    pub decay_updates: u16,
    pub sustain_level: i16,
    pub release_level: i16,
    pub release_updates: u16,
}

/// A chunk of the script image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chunk {
    /// An event: what a sound id plays.
    Entry {
        controls: Controls,
        commands: Vec<Command>,
    },
    Adsr {
        adsr1: u16,
        adsr2: u16,
    },
    Envelope(Envelope),
}

/// An item of the pool: a sample's VAG body, or raw bytes where something else sits between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolItem {
    Sample(Vec<u8>),
    Raw(Vec<u8>),
}

impl PoolItem {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        match self {
            PoolItem::Sample(b) | PoolItem::Raw(b) => b,
        }
    }
}

/// A bank as its parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bank {
    pub bank_id: u16,
    pub programs: Vec<Program>,
    pub chunks: Vec<Chunk>,
    /// The events sound ids name, by entry number: the chunk of each, none where the slot is empty.
    pub slots: Vec<Option<usize>>,
    pub pool: Vec<PoolItem>,
}

fn slope(delta: i32, updates: u16) -> i16 {
    if updates == 0 {
        return delta as i16;
    }
    let step = delta / i32::from(updates);
    let step = if step != 0 || delta == 0 {
        step
    } else if delta > 0 {
        1
    } else {
        -1
    };
    step as i16
}

fn envelope_words(e: &Envelope) -> [u16; 12] {
    let w = |v: i16| v as u16;
    [
        w(e.delay),
        w(e.attack_level),
        w(slope(i32::from(e.attack_level), e.attack_updates)),
        e.attack_updates,
        w(e.hold),
        e.decay_updates,
        w(slope(i32::from(e.sustain_level) - i32::from(e.attack_level), e.decay_updates)),
        w(e.sustain_level),
        w(e.release_level),
        w(slope(i32::from(e.release_level) - i32::from(e.sustain_level), e.release_updates)),
        e.release_updates,
        0,
    ]
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn i16_at(b: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([b[at], b[at + 1]])
}

fn i32_at(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn expect<T: PartialEq + std::fmt::Debug>(what: &str, got: &T, want: &T) -> Result<(), String> {
    if got == want { Ok(()) } else { Err(format!("{what} is {got:?}, not the derived {want:?}")) }
}

/// The pool as VAG bodies, and raw bytes where something else sits between them.
fn pool(blob: &[u8], start: usize, size: usize) -> Vec<PoolItem> {
    let (mut items, mut at, end) = (Vec::new(), start, start + size);
    let zeros = [0_u8; 16];
    while at < end {
        if blob.get(at..at + 16) == Some(&zeros[..]) {
            let mut q = at + 16;
            while q < end && blob[q + 1] & 1 == 0 {
                q += 16;
            }
            q += 16;
            if blob.get(q..q + 16) == Some(&crate::vag::END_BLOCK[..]) {
                q += 16;
            }
            if q <= end {
                items.push(PoolItem::Sample(blob[at..q].to_vec()));
                at = q;
                continue;
            }
        }
        let next = (at + 16..end).step_by(16).find(|&o| blob.get(o..o + 16) == Some(&zeros[..])).unwrap_or(end);
        items.push(PoolItem::Raw(blob[at..next].to_vec()));
        at = next;
    }
    items
}

const CHUNK_SIZE: [(&[u8; 4], usize); 8] = [
    (b"oneC", 0x10),
    (b"oneV", 0x18),
    (b"Loop", 0x08),
    (b"Wait", 0x08),
    (b"endL", 0x04),
    (b"endC", 0x04),
    (b"oneA", 0x08),
    (b"oneE", 0x1C),
];

/// A bank's parts.
///
/// # Errors
/// It is no sound-script bank, or something derived is not what it is derived to be.
#[expect(clippy::too_many_lines, reason = "the bank, in file order")]
pub fn decode(blob: &[u8]) -> Result<Bank, String> {
    if blob.get(..4) != Some(b"hSPK") || blob.get(6) != Some(&2) {
        return Err("not a sound-script bank (hSPK, image kind 2)".into());
    }
    let bank_id = u16_at(blob, 4);
    let (groups, layers, sample_count) = (usize::from(blob[7]), usize::from(blob[8]), blob[9]);
    let wave_block = u16_at(blob, 10);
    let (sectors, head_sectors) = (blob[12], blob[13]);
    let image_bytes = usize::from(u16_at(blob, 14));
    let wave_bytes = usize::try_from(i32_at(blob, 0x10)).map_err(|e| e.to_string())?;
    if wave_block != 0 {
        return Err("a pool uploaded at an offset (waveBlockOffset) is not handled".into());
    }
    let pool_at = align(IMAGE_AT + image_bytes, SECTOR);
    let pool = pool(blob, pool_at, wave_bytes);
    let samples = pool.iter().filter(|i| matches!(i, PoolItem::Sample(_))).count();
    expect("header byte 9 (samples)", &usize::from(sample_count), &samples)?;
    expect("header byte 0x0D (sectors before the pool)", &usize::from(head_sectors), &(pool_at / SECTOR))?;
    expect("transferSectors", &usize::from(sectors), &(align(pool_at + wave_bytes, SECTOR) / SECTOR))?;
    let mut offset_of = BTreeMap::new();
    let mut at = 0;
    for (i, item) in pool.iter().enumerate() {
        offset_of.insert(at, i);
        at += item.bytes().len();
    }

    let mut programs = Vec::new();
    let lt = HEADER + 4 * groups;
    let mut k = 0;
    for g in 0..groups {
        let o = HEADER + 4 * g;
        let (count, byte1, volume, pan) = (blob[o], blob[o + 1], blob[o + 2], blob[o + 3]);
        expect("a program's byte 1", &byte1, &0xFF)?;
        let mut program = Program { volume, pan, layers: Vec::new() };
        for _ in 0..count {
            let o = lt + 0x14 * k;
            expect("a layer's byte 2", &blob[o + 2], &0)?;
            let wave = usize::try_from(i32_at(blob, o + 0x10)).map_err(|e| e.to_string())?;
            program.layers.push(Layer {
                reverb: blob[o],
                pan: blob[o + 1],
                volume: blob[o + 3],
                root_key: blob[o + 4],
                fine_tune: blob[o + 5],
                priority: u16_at(blob, o + 6),
                key_min: blob[o + 8],
                key_max: blob[o + 9],
                bend_down: blob[o + 10],
                bend_up: blob[o + 11],
                adsr1: u16_at(blob, o + 12),
                adsr2: u16_at(blob, o + 14),
                sample: *offset_of.get(&wave).ok_or_else(|| format!("layer {k}: no sample at {wave:#x}"))?,
            });
            k += 1;
        }
        programs.push(program);
    }
    expect("the layers the programs hold", &k, &layers)?;

    let image = blob.get(IMAGE_AT..IMAGE_AT + image_bytes).ok_or("the script image runs past the bank")?;
    if image.get(..4) != Some(b"hONE") {
        return Err("no hONE script image".into());
    }
    expect("the script image's bank", &u16_at(image, 4), &bank_id)?;
    let count = usize::from(u16_at(image, 6));
    let table: Vec<usize> = (0..count).map(|i| usize::from(u16_at(image, 8 + 2 * i))).collect();
    let (mut chunks, mut starts) = (Vec::new(), BTreeMap::new());
    let mut entry: Option<usize> = None;
    let mut at = align(8 + 2 * count, 4);
    // Voices' ADSR and envelope references, by image offset until every chunk's start is known.
    let mut references: Vec<(usize, usize, i16, i16)> = Vec::new();
    while at < image.len() {
        let tag = &image[at..at + 4];
        let size = CHUNK_SIZE
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, s)| *s)
            .ok_or_else(|| format!("image offset {at:#x}: unknown chunk {:?}", String::from_utf8_lossy(tag)))?;
        let body = &image[at + 4..at + size];
        if matches!(tag, b"oneC" | b"oneA" | b"oneE")
            && let Some(e) = entry
        {
            if let Chunk::Entry { commands, .. } = &chunks[e]
                && !commands.is_empty()
            {
                return Err(format!("image offset {at:#x}: an entry with commands and no endC"));
            }
            entry = None;
        }
        match tag {
            b"oneC" => {
                expect("an entry's bytes 0x0A..0x0B", &u16_at(body, 6), &0)?;
                starts.insert(at, chunks.len());
                entry = Some(chunks.len());
                chunks.push(Chunk::Entry {
                    controls: Controls {
                        unknown_4: body[0],
                        volume: body[1],
                        pan: body[2],
                        max_instances: body[3],
                        retrigger: i16_at(body, 4),
                        priority: u16_at(body, 8),
                        flags: u16_at(body, 10),
                    },
                    commands: Vec::new(),
                });
            }
            b"oneA" => {
                starts.insert(at, chunks.len());
                chunks.push(Chunk::Adsr { adsr1: u16_at(body, 0), adsr2: u16_at(body, 2) });
            }
            b"oneE" => {
                starts.insert(at, chunks.len());
                let words: Vec<u16> = (0..12).map(|i| u16_at(body, 2 * i)).collect();
                let envelope = Envelope {
                    delay: words[0] as i16,
                    attack_level: words[1] as i16,
                    attack_updates: words[3],
                    hold: words[4] as i16,
                    decay_updates: words[5],
                    sustain_level: words[7] as i16,
                    release_level: words[8] as i16,
                    release_updates: words[10],
                };
                expect("an envelope's words", &words.as_slice(), &&envelope_words(&envelope)[..])?;
                chunks.push(Chunk::Envelope(envelope));
            }
            _ => {
                let e = entry.ok_or_else(|| format!("image offset {at:#x}: a command outside an entry"))?;
                let Chunk::Entry { commands, .. } = &mut chunks[e] else { unreachable!("an entry") };
                match tag {
                    b"oneV" => {
                        expect("a voice's byte 0x0F", &body[11], &0)?;
                        references.push((e, commands.len(), i16_at(body, 14), i16_at(body, 18)));
                        commands.push(Command::Voice(Voice {
                            bank: u16_at(body, 0),
                            program: body[2],
                            layer: body[3],
                            delay: u16_at(body, 4),
                            gate: u16_at(body, 6),
                            pan: body[8] as i8,
                            volume: body[9] as i8,
                            reverb: body[10] as i8,
                            priority: u16_at(body, 12),
                            adsr: None,
                            pitch: i16_at(body, 16),
                            envelope: None,
                        }));
                    }
                    b"Loop" => {
                        expect("a loop's byte 1", &body[1], &0)?;
                        commands.push(Command::Loop { repeat: body[0], delay: u16_at(body, 2) });
                    }
                    b"Wait" => commands.push(Command::Wait { ticks: i32_at(body, 0) }),
                    b"endL" => commands.push(Command::EndLoop),
                    _ => entry = None,
                }
            }
        }
        at += size;
    }
    // References by chunk, not by offset.
    for (e, c, adsr, envelope) in references {
        let chunk = |offset: i16| -> Result<Option<usize>, String> {
            if offset == -1 {
                return Ok(None);
            }
            let offset = usize::try_from(offset).map_err(|e| e.to_string())?;
            starts.get(&offset).copied().map(Some).ok_or_else(|| format!("no chunk at {offset:#x}"))
        };
        let (adsr, envelope) = (chunk(adsr)?, chunk(envelope)?);
        if let Chunk::Entry { commands, .. } = &mut chunks[e]
            && let Command::Voice(voice) = &mut commands[c]
        {
            voice.adsr = adsr;
            voice.envelope = envelope;
        }
    }
    let slots = table
        .into_iter()
        .map(|offset| {
            if offset == 0 {
                Ok(None)
            } else {
                starts.get(&offset).copied().map(Some).ok_or(format!("no entry at {offset:#x}"))
            }
        })
        .collect::<Result<_, String>>()?;
    Ok(Bank { bank_id, programs, chunks, slots, pool })
}

/// A bank's bytes from its parts.
#[must_use]
#[expect(clippy::too_many_lines, reason = "the bank, in file order")]
pub fn encode(bank: &Bank) -> Vec<u8> {
    let count = bank.slots.len();
    let mut offset = Vec::with_capacity(bank.chunks.len());
    let mut at = align(8 + 2 * count, 4);
    for chunk in &bank.chunks {
        offset.push(at);
        at += match chunk {
            Chunk::Entry { commands, .. } => {
                0x10 + commands
                    .iter()
                    .map(|c| match c {
                        Command::Voice(_) => 0x18,
                        Command::Loop { .. } | Command::Wait { .. } => 0x08,
                        Command::EndLoop => 0x04,
                    })
                    .sum::<usize>()
                    + if commands.is_empty() { 0 } else { 4 }
            }
            Chunk::Adsr { .. } => 0x08,
            Chunk::Envelope(_) => 0x1C,
        };
    }
    let mut image = b"hONE".to_vec();
    image.extend(bank.bank_id.to_le_bytes());
    image.extend((count as u16).to_le_bytes());
    for slot in &bank.slots {
        image.extend((slot.map_or(0, |s| offset[s]) as u16).to_le_bytes());
    }
    image.resize(align(image.len(), 4), 0);
    let reference = |c: Option<usize>| c.map_or(-1_i16, |c| offset[c] as i16);
    for chunk in &bank.chunks {
        match chunk {
            Chunk::Entry { controls: c, commands } => {
                image.extend(b"oneC");
                image.extend([c.unknown_4, c.volume, c.pan, c.max_instances]);
                image.extend(c.retrigger.to_le_bytes());
                image.extend([0, 0]);
                image.extend(c.priority.to_le_bytes());
                image.extend(c.flags.to_le_bytes());
                for command in commands {
                    match command {
                        Command::Voice(v) => {
                            image.extend(b"oneV");
                            image.extend(v.bank.to_le_bytes());
                            image.extend([v.program, v.layer]);
                            image.extend(v.delay.to_le_bytes());
                            image.extend(v.gate.to_le_bytes());
                            image.extend([v.pan as u8, v.volume as u8, v.reverb as u8, 0]);
                            image.extend(v.priority.to_le_bytes());
                            image.extend(reference(v.adsr).to_le_bytes());
                            image.extend(v.pitch.to_le_bytes());
                            image.extend(reference(v.envelope).to_le_bytes());
                        }
                        Command::Loop { repeat, delay } => {
                            image.extend(b"Loop");
                            image.extend([*repeat, 0]);
                            image.extend(delay.to_le_bytes());
                        }
                        Command::Wait { ticks } => {
                            image.extend(b"Wait");
                            image.extend(ticks.to_le_bytes());
                        }
                        Command::EndLoop => image.extend(b"endL"),
                    }
                }
                if !commands.is_empty() {
                    image.extend(b"endC");
                }
            }
            Chunk::Adsr { adsr1, adsr2 } => {
                image.extend(b"oneA");
                image.extend(adsr1.to_le_bytes());
                image.extend(adsr2.to_le_bytes());
            }
            Chunk::Envelope(e) => {
                image.extend(b"oneE");
                for word in envelope_words(e) {
                    image.extend(word.to_le_bytes());
                }
            }
        }
    }
    let mut address = Vec::with_capacity(bank.pool.len());
    let mut wave_bytes = 0;
    for item in &bank.pool {
        address.push(wave_bytes);
        wave_bytes += item.bytes().len();
    }
    let layers: Vec<&Layer> = bank.programs.iter().flat_map(|p| &p.layers).collect();
    let pool_at = align(IMAGE_AT + image.len(), SECTOR);
    let sectors = align(pool_at + wave_bytes, SECTOR) / SECTOR;
    let samples = bank.pool.iter().filter(|i| matches!(i, PoolItem::Sample(_))).count();
    let mut out = b"hSPK".to_vec();
    out.extend(bank.bank_id.to_le_bytes());
    out.extend([2, bank.programs.len() as u8, layers.len() as u8, samples as u8]);
    out.extend(0_u16.to_le_bytes());
    out.extend([sectors as u8, (pool_at / SECTOR) as u8]);
    out.extend((image.len() as u16).to_le_bytes());
    out.extend((wave_bytes as i32).to_le_bytes());
    for p in &bank.programs {
        out.extend([p.layers.len() as u8, 0xFF, p.volume, p.pan]);
    }
    for l in &layers {
        out.extend([l.reverb, l.pan, 0, l.volume, l.root_key, l.fine_tune]);
        out.extend(l.priority.to_le_bytes());
        out.extend([l.key_min, l.key_max, l.bend_down, l.bend_up]);
        out.extend(l.adsr1.to_le_bytes());
        out.extend(l.adsr2.to_le_bytes());
        out.extend((address[l.sample] as i32).to_le_bytes());
    }
    out.resize(IMAGE_AT, 0);
    out.extend(&image);
    out.resize(pool_at, 0);
    for item in &bank.pool {
        out.extend(item.bytes());
    }
    let end = ((pool_at + wave_bytes) & !(SECTOR - 1)) + 2 * SECTOR - 16;
    out.resize(end, 0);
    out
}

#[cfg(test)]
mod tests {
    /// The banks in a folder (`PE2_SOUND_BANKS`, each file a bank as a disc stores it): each read into its parts and
    /// written back byte for byte, and each sample decoded to input re-encoding to it.
    #[test]
    fn the_games_banks_are_their_parts_and_their_samples_decode() {
        let Ok(folder) = std::env::var("PE2_SOUND_BANKS") else { return };
        let mut bodies = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(folder).unwrap() {
            let blob = std::fs::read(entry.unwrap().path()).unwrap();
            let bank = super::decode(&blob).unwrap();
            assert_eq!(super::encode(&bank), blob);
            for item in &bank.pool {
                if let super::PoolItem::Sample(body) = item {
                    bodies.insert(body.clone());
                }
            }
        }
        let bodies: Vec<Vec<u8>> = bodies.into_iter().collect();
        let failed: Vec<String> = bodies
            .iter()
            .enumerate()
            .filter_map(|(i, body)| crate::vag::decode(body).err().map(|why| format!("sample {i}: {why}")))
            .collect();
        assert!(failed.is_empty(), "{} of {} samples: {failed:?}", failed.len(), bodies.len());
    }
}
