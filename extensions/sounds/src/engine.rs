//! A sound script played as the game plays it: a port of the game's own sound script interpreter and of its voice
//! upkeep, over the SPU's voices (`spu`), one audio update at a time - 60 a second, as on NTSC,
//! each:
//!
//! 1. voices whose envelope has ended are let go, their script's voice detached - a voice keyed on whose envelope is
//!    silent is let go only after five updates, the time its attack takes to be heard;
//! 2. the script runs: its commands until one waits - a note keyed on once the script's clock reaches its delay, a loop,
//!    a wait - then each of its voices: its gate counted down to its key-off, its pitch envelope stepped;
//! 3. what was asked of the voices is done: key-offs, then pitches, then key-ons.
//!
//! A note's voice takes its layer's sample, volume, pan, ADSR and tuning, which the note may override; its volume is
//! the master volume (full), the script's and its own, through the engine's gain and pan curves. Reverb is not heard;
//! a note naming another bank plays this one's. The tables the engine reads are computed: they are formulas.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "the engine's registers and tables: keys, pitches, volumes and pans of 7 to 16 bits, clocks of 16.16 - each value within what it is cast to, as the engine casts it"
)]

use std::sync::Arc;

use formats::spk::{Bank, Chunk, Command, Controls, Envelope, Layer, PoolItem, Voice as Note};

use crate::spu::{self, Spu, Voice};

/// Audio updates a second (NTSC's field rate), and the samples each plays.
pub const UPDATES: f64 = 59.94;
/// A gate that never closes (a note held until the script stops).
const HELD: i32 = 0x7FFF_FFFF;
/// The reverb's depth once the game has started (`spuTickReverbWarmup`).
pub const DEPTH: u16 = 0x2800;
/// The area's reverb setting the game starts with and most areas have; two areas a stage have 2, the Mine Gorge 3.
pub const ROOM: i8 = 1;

/// Where a script is heard: the reverb mode the game sets, if known, and the area's reverb setting.
#[derive(Clone, Debug)]
pub struct Room {
    pub mode: Option<crate::reverb::Mode>,
    pub setting: i8,
}

impl Default for Room {
    fn default() -> Room {
        Room { mode: None, setting: ROOM }
    }
}

/// Whether a note is sent to the reverb in an area of this setting (`_sndScriptUseReverb`): level-3 notes at settings
/// of 2 and above, setting 3 nothing else, otherwise a note of a level up to the setting.
fn sent(level: i8, setting: i8) -> bool {
    if level == 3 && setting >= 2 {
        true
    } else if level != 3 && setting == 3 {
        false
    } else {
        level >= 0 && setting >= level
    }
}

/// Updates a voice keyed on is kept while its envelope is silent.
const GRACE: u8 = 5;
const UNITY: i32 = 127;

/// The engine's pitch tables, as one run of memory: the semitones' (96), then the fine steps' (128), then the gains'
/// (128) - read past the first where a key runs beyond it, as the engine would.
fn table_word(index: usize) -> u32 {
    match index {
        0..96 => (1024.0 * 2f64.powf((index as f64 - 72.0) / 12.0)).floor() as u32,
        96..224 => fine(index - 96),
        224..352 => gain(index - 224),
        _ => 0,
    }
}

fn fine(index: usize) -> u32 {
    (1024.0 * 2f64.powf(index as f64 / 1536.0)).floor() as u32
}

/// The gain for a volume of 0..127: its square, to 16383 at full.
fn gain(index: usize) -> u32 {
    let i = index as u32;
    i * i * 16383 / 16129
}

/// The share of a volume a side takes at a pan step, in 1/4096: a quarter cosine over 0..126.
fn pan_gain(index: usize) -> u32 {
    (4096.0 * (index as f64 / 126.0 * std::f64::consts::FRAC_PI_2).cos()).floor().max(0.0) as u32
}

/// The SPU pitch of a key and its Q8 fraction, for a layer's root key and fine tune (`spuCalcPitch`).
fn pitch(key: i32, fraction: i32, root: i32, fine_tune: i32) -> u16 {
    let mut coordinate = (fraction as u32).wrapping_add((key as u32) << 8);
    coordinate = coordinate.wrapping_sub(((root as u32) << 8).wrapping_sub((fine_tune as u32) << 1));
    coordinate = coordinate.wrapping_add(72 << 8);
    let fine_index = ((coordinate & 0xFF) >> 1) as usize;
    let semitone = ((coordinate & 0xFFFF) >> 8) as usize;
    let value = (table_word(semitone) * fine(fine_index)) >> 8;
    if value & 0xFFFF >= 0x4000 { 0x3FFF } else { value as u16 }
}

/// A voice's volumes for its scaled volume and pan (`_sndVoiceCalcMixVolumes`, `spuCalcPanVolumes`), unattenuated.
fn volumes(scaled: i32, pan: i32) -> (i16, i16) {
    let index = (scaled * UNITY / UNITY).clamp(0, UNITY) as usize;
    let volume = gain(index);
    let at = if pan > 0 { (pan - 1).min(126) as usize } else { 0 };
    let left = (volume * pan_gain(at)) >> 12;
    let right = (volume * pan_gain(126 - at)) >> 12;
    (left.min(0x3FFF) as i16, right.min(0x3FFF) as i16)
}

/// A ramp's step per update, as the authoring tool stored it.
fn slope(delta: i32, updates: i32) -> i32 {
    if updates == 0 {
        return delta;
    }
    let step = delta / updates;
    if step != 0 || delta == 0 { step } else { delta.signum() }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Delay,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
    ReleaseHold,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Release {
    Held,
    Pending,
    Started,
}

/// A voice's pitch envelope as it plays (`_SndVoiceEnvelope`), its offsets in 1/256 semitone.
struct PitchEnvelope {
    envelope: Envelope,
    attack_slope: i32,
    decay_slope: i32,
    release_slope: i32,
    stage: Stage,
    release: Release,
    keyed: i32,
    root: i32,
    fine_tune: i32,
    updates: i32,
    release_step: i32,
    ramp: i32,
    attack: i32,
    decay: i32,
    releasing: i32,
}

impl PitchEnvelope {
    fn new(envelope: &Envelope, keyed: i32, layer: &Layer) -> PitchEnvelope {
        let e = envelope.clone();
        let (attack, sustain, release) =
            (i32::from(e.attack_level), i32::from(e.sustain_level), i32::from(e.release_level));
        PitchEnvelope {
            attack_slope: slope(attack, i32::from(e.attack_updates)),
            decay_slope: slope(sustain - attack, i32::from(e.decay_updates)),
            release_slope: slope(release - sustain, i32::from(e.release_updates)),
            envelope: e,
            stage: Stage::Delay,
            release: Release::Held,
            keyed,
            root: i32::from(layer.root_key),
            fine_tune: i32::from(layer.fine_tune),
            updates: 0,
            release_step: 0,
            ramp: 0,
            attack: 0,
            decay: 0,
            releasing: 0,
        }
    }

    /// One update (`_sndVoiceTickPitchEnvelope`): the pitch it sets, if any.
    fn tick(&mut self) -> Option<u16> {
        let e = &self.envelope;
        let release_level = i32::from(e.release_level);
        if self.release == Release::Pending {
            self.stage = Stage::Release;
            self.release_step = if (self.ramp - release_level) * self.release_slope > 0 {
                -self.release_slope
            } else {
                self.release_slope
            };
            self.updates = 0;
            self.release = Release::Started;
            self.releasing = self.ramp;
        }
        let base = self.keyed << 1;
        let mut stage = self.stage;
        let q8 = loop {
            match stage {
                Stage::Delay => {
                    if self.updates < i32::from(e.delay) {
                        self.updates += 1;
                        return None;
                    }
                    self.stage = Stage::Attack;
                    (self.updates, self.attack, self.ramp) = (0, 0, 0);
                    stage = Stage::Attack;
                }
                Stage::Attack => {
                    let q8 = base + self.attack;
                    if self.updates < i32::from(e.attack_updates) {
                        self.updates += 1;
                        self.attack += self.attack_slope;
                        self.ramp = self.attack;
                        break q8;
                    }
                    (self.stage, self.updates) = (Stage::Hold, 0);
                    stage = Stage::Hold;
                }
                Stage::Hold => {
                    let q8 = base + i32::from(e.attack_level);
                    if self.updates < i32::from(e.hold) {
                        self.updates += 1;
                        break q8;
                    }
                    self.stage = Stage::Decay;
                    self.decay = i32::from(e.attack_level);
                    self.updates = 0;
                    self.ramp = self.decay;
                    stage = Stage::Decay;
                }
                Stage::Decay => {
                    let q8 = base + self.decay;
                    if self.updates < i32::from(e.decay_updates) {
                        self.updates += 1;
                        self.decay += self.decay_slope;
                        self.ramp = self.decay;
                        break q8;
                    }
                    self.stage = Stage::Sustain;
                    stage = Stage::Sustain;
                }
                Stage::Sustain => break base + i32::from(e.sustain_level),
                Stage::Release => {
                    if (self.ramp - release_level) * self.release_slope >= 0 {
                        self.stage = Stage::ReleaseHold;
                    } else {
                        self.releasing += self.release_step;
                        self.ramp = self.releasing;
                    }
                    break base + self.releasing;
                }
                Stage::ReleaseHold => break base + release_level,
            }
        };
        Some(pitch((q8 >> 8) & 0xFFFF, q8 & 0xFF, self.root, self.fine_tune))
    }
}

/// A voice of the script (`_SndVoice`): the SPU voice it holds, its gate, its pitch envelope.
struct Held {
    spu: usize,
    gate: i32,
    envelope: Option<PitchEnvelope>,
    /// What it is of what was played.
    span: usize,
}

/// A note as it was played: the command that keyed it on, and the updates it was keyed on, keyed off and let go at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub command: usize,
    pub start: usize,
    pub off: Option<usize>,
    pub end: Option<usize>,
}

/// One script played on its own SPU.
pub struct Player {
    layers: Vec<Layer>,
    first_layer: Vec<usize>,
    bodies: Vec<Option<Arc<Vec<u8>>>>,
    chunks: Vec<Chunk>,
    controls: Controls,
    commands: Vec<Command>,
    cursor: usize,
    clock: i32,
    loops: Vec<(u8, usize)>,
    ended: bool,
    done: bool,
    held: Vec<Held>,
    spu: Spu,
    allocated: [bool; spu::VOICES],
    grace: [u8; spu::VOICES],
    key_on: Vec<(usize, Voice)>,
    key_off: Vec<usize>,
    pitches: Vec<(usize, u16)>,
    owner: [bool; spu::VOICES],
    owed: f64,
    setting: i8,
    /// The updates played, and the notes.
    updates: usize,
    pub spans: Vec<Span>,
}

impl Player {
    /// The script at a chunk of a bank (an entry), ready to play.
    ///
    /// # Errors
    /// The chunk is no entry.
    pub fn new(bank: &Bank, entry: usize, room: &Room) -> anyhow::Result<Player> {
        let Some(Chunk::Entry { controls, commands }) = bank.chunks.get(entry) else {
            anyhow::bail!("chunk {entry} is no script");
        };
        let mut first_layer = Vec::new();
        let mut layers = Vec::new();
        for program in &bank.programs {
            first_layer.push(layers.len());
            layers.extend(program.layers.iter().cloned());
        }
        let bodies = bank
            .pool
            .iter()
            .map(|item| match item {
                PoolItem::Sample(body) => Some(Arc::new(body.clone())),
                PoolItem::Raw(_) => None,
            })
            .collect();
        Ok(Player {
            layers,
            first_layer,
            bodies,
            chunks: bank.chunks.clone(),
            controls: controls.clone(),
            commands: commands.clone(),
            cursor: 0,
            clock: 0,
            loops: Vec::new(),
            ended: false,
            done: false,
            held: Vec::new(),
            spu: Spu::default(),
            allocated: [false; spu::VOICES],
            grace: [0; spu::VOICES],
            key_on: Vec::new(),
            key_off: Vec::new(),
            pitches: Vec::new(),
            owner: [false; spu::VOICES],
            owed: 0.0,
            setting: room.setting,
            updates: 0,
            spans: Vec::new(),
        })
        .map(|mut player| {
            player.spu.reverb = room.mode.as_ref().map(|mode| crate::reverb::Reverb::new(mode, DEPTH));
            player
        })
    }

    /// Whether it has ended and nothing sounds.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.done && self.spu.voices.iter().flatten().all(|v| v.level() == 0)
    }

    /// Stop it as the game stops a script (`_sndScriptReleaseVoices`): its voices released fast, its pitch envelopes
    /// released, nothing more started.
    pub fn stop(&mut self) {
        self.ended = true;
        self.cursor = self.commands.len();
        for held in &mut self.held {
            if let Some(voice) = self.spu.voices[held.spu].as_mut() {
                voice.adsr2 = (voice.adsr2 & !0x1F) | 5;
            }
            self.key_off.push(held.spu);
            if let Some(envelope) = held.envelope.as_mut() {
                envelope.release = Release::Pending;
            }
            held.gate = 0;
            self.spans[held.span].off.get_or_insert(self.updates);
        }
    }

    /// One audio update, and the sound it makes: left and right, interleaved.
    pub fn update(&mut self, out: &mut Vec<i16>) {
        self.tick_voices();
        if !self.done {
            while self.exec() {}
            self.tick_held();
            if self.held.is_empty() && self.ended {
                self.done = true;
            }
        }
        self.flush();
        self.owed += f64::from(spu::RATE) / UPDATES;
        let count = self.owed.floor();
        self.owed -= count;
        self.spu.render(count as usize, out);
        self.updates += 1;
    }

    /// `spuTickVoices`: voices whose envelope ended let go, and their script's voices detached.
    fn tick_voices(&mut self) {
        for index in 0..spu::VOICES {
            self.grace[index] = self.grace[index].saturating_sub(1);
            if let Some(voice) = &self.spu.voices[index] {
                let sounding = voice.level() > 0;
                if sounding {
                    continue;
                }
                if voice.keyed && self.grace[index] != 0 {
                    continue;
                }
                if voice.keyed {
                    self.key_off.push(index);
                }
            }
            if self.allocated[index] || self.owner[index] {
                self.allocated[index] = false;
                if self.owner[index] {
                    self.owner[index] = false;
                    for held in self.held.iter().filter(|held| held.spu == index) {
                        let span = &mut self.spans[held.span];
                        span.off.get_or_insert(self.updates);
                        span.end.get_or_insert(self.updates);
                    }
                    self.held.retain(|held| held.spu != index);
                }
            }
        }
    }

    /// `_sndScriptExecCommand`: whether to run the next command at once.
    fn exec(&mut self) -> bool {
        let Some(command) = self.commands.get(self.cursor).cloned() else {
            self.ended = true;
            return false;
        };
        match command {
            Command::Loop { repeat, delay } => {
                if self.loops.len() >= 8 {
                    self.ended = true;
                    return false;
                }
                if self.clock >> 16 >= i32::from(delay) {
                    self.cursor += 1;
                    self.loops.push((repeat, self.cursor));
                    self.clock -= i32::from(delay) << 16;
                    true
                } else {
                    self.clock += 0x10000;
                    false
                }
            }
            Command::EndLoop => {
                let Some(&(count, start)) = self.loops.last() else {
                    self.ended = true;
                    return false;
                };
                if count == 1 {
                    self.cursor += 1;
                    self.loops.pop();
                } else {
                    self.cursor = start;
                    if count != 0
                        && let Some(top) = self.loops.last_mut()
                    {
                        top.0 -= 1;
                    }
                }
                true
            }
            Command::Wait { ticks } => {
                if self.clock >> 16 < ticks {
                    self.clock += 0x10000;
                    return false;
                }
                self.clock -= ticks << 16;
                self.cursor += 1;
                true
            }
            Command::Voice(note) => {
                if self.clock >> 16 < i32::from(note.delay) {
                    self.clock += 0x10000;
                    return false;
                }
                self.key_on_note(&note);
                self.clock -= i32::from(note.delay) << 16;
                self.cursor += 1;
                true
            }
        }
    }

    /// A note keyed on: a free voice set up as its layer and the note say, held by the script.
    fn key_on_note(&mut self, note: &Note) {
        let Some(index) = (0..spu::VOICES).find(|&i| !self.allocated[i]) else { return };
        let at = self.first_layer.get(usize::from(note.program)).map(|first| first + usize::from(note.layer));
        let Some(layer) = at.and_then(|at| self.layers.get(at)).cloned() else { return };
        let Some(body) = self.bodies.get(layer.sample).cloned().flatten() else { return };
        self.allocated[index] = true;
        let base_volume = if note.volume < 0 { i32::from(layer.volume) } else { i32::from(note.volume as u8) };
        let scaled = i32::from(((UNITY * i32::from(self.controls.volume) * base_volume) / (UNITY * UNITY)) as i8);
        let pan = if note.pan < 0 { i32::from(layer.pan) } else { i32::from(note.pan) };
        let pan = i32::from((pan + (i32::from(self.controls.pan) - 64)) as i16).clamp(0, 127);
        let adsr = match note.adsr.and_then(|c| self.chunks.get(c)) {
            Some(Chunk::Adsr { adsr1, adsr2 }) => (*adsr1, *adsr2),
            _ => (layer.adsr1, layer.adsr2),
        };
        let keyed = i32::from((note.pitch as u16).wrapping_add(u16::from(layer.key_min) << 7));
        let tuning = pitch(keyed >> 7, (keyed & 0x7F) * 2, i32::from(layer.root_key), i32::from(layer.fine_tune));
        let mut voice = Voice::keyed_on(body, tuning, adsr, volumes(scaled, pan));
        voice.reverb = sent(note.reverb, self.setting);
        self.grace[index] = GRACE;
        self.key_off.retain(|&i| i != index);
        self.key_on.retain(|(i, _)| *i != index);
        self.key_on.push((index, voice));
        let gate = if note.gate == 0 { HELD } else { i32::from(note.gate) << 16 };
        let envelope = match note.envelope.and_then(|c| self.chunks.get(c)) {
            Some(Chunk::Envelope(e)) => Some(PitchEnvelope::new(e, keyed, &layer)),
            _ => None,
        };
        self.owner[index] = true;
        self.spans.push(Span { command: self.cursor, start: self.updates, off: None, end: None });
        self.held.push(Held { spu: index, gate, envelope, span: self.spans.len() - 1 });
    }

    /// `_sndVoiceTick` for each of the script's voices.
    fn tick_held(&mut self) {
        for held in &mut self.held {
            if held.gate <= 0 {
                held.gate = 0;
                self.key_off.push(held.spu);
                self.spans[held.span].off.get_or_insert(self.updates);
                if let Some(envelope) = held.envelope.as_mut()
                    && envelope.release == Release::Held
                {
                    envelope.release = Release::Pending;
                }
            } else if held.gate < HELD {
                held.gate -= 0x10000;
            }
            if let Some(pitch) = held.envelope.as_mut().and_then(PitchEnvelope::tick) {
                self.pitches.push((held.spu, pitch));
            }
        }
    }

    /// `spuFlushVoiceUpdates`: key-offs (and the voices about to be keyed on), then pitches, then key-ons.
    fn flush(&mut self) {
        for index in self.key_off.drain(..).chain(self.key_on.iter().map(|(i, _)| *i)) {
            if let Some(voice) = self.spu.voices[index].as_mut() {
                voice.key_off();
            }
        }
        let mut keyed = std::mem::take(&mut self.key_on);
        for (index, pitch) in self.pitches.drain(..) {
            if let Some((_, voice)) = keyed.iter_mut().find(|(i, _)| *i == index) {
                voice.pitch = pitch;
            } else if let Some(voice) = self.spu.voices[index].as_mut() {
                voice.pitch = pitch;
            }
        }
        for (index, voice) in keyed {
            self.spu.voices[index] = Some(voice);
        }
    }
}

/// A script played through: its sound, left and right interleaved at `spu::RATE`, in updates' runs - until it ends and
/// nothing sounds, or until `limit` seconds, when it is stopped as the game stops one and its release heard out; then
/// its reverb's tail.
///
/// # Errors
/// The chunk is no entry.
pub fn play(bank: &Bank, entry: usize, limit: f64, room: &Room) -> anyhow::Result<Vec<Vec<i16>>> {
    let mut updates = Vec::new();
    let mut player = run(bank, entry, limit, room, |out| updates.push(out))?;
    // The reverb's tail, heard out: until an update of it is all but silent, for four seconds at most.
    if player.spu.reverb.is_some() {
        for _ in 0..(4.0 * UPDATES) as usize {
            let mut out = Vec::new();
            player.update(&mut out);
            let quiet = out.iter().all(|s| s.unsigned_abs() < 8);
            updates.push(out);
            if quiet {
                break;
            }
        }
    }
    Ok(updates)
}

/// A script played through as `play` plays it, its sound given to `heard` an update at a time; the player as it ended.
fn run(bank: &Bank, entry: usize, limit: f64, room: &Room, mut heard: impl FnMut(Vec<i16>)) -> anyhow::Result<Player> {
    let mut player = Player::new(bank, entry, room)?;
    let most = (limit * UPDATES) as usize;
    let mut stopped = false;
    while !player.finished() {
        if player.updates >= most {
            if stopped {
                break;
            }
            player.stop();
            stopped = true;
        }
        let mut out = Vec::new();
        player.update(&mut out);
        heard(out);
        if stopped && player.updates >= most + (3.0 * UPDATES) as usize {
            break;
        }
    }
    Ok(player)
}

/// What a script plays, as a score: each note's span, and the updates it all takes - notes still sounding at its end
/// ending there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Score {
    pub updates: usize,
    pub spans: Vec<Span>,
    /// Whether it was stopped at the limit rather than ending by itself.
    pub stopped: bool,
}

/// A script's score, as `play` plays it (without reverb, which lengthens only what is heard).
///
/// # Errors
/// The chunk is no entry.
pub fn score(bank: &Bank, entry: usize, limit: f64) -> anyhow::Result<Score> {
    let player = run(bank, entry, limit, &Room { mode: None, setting: ROOM }, drop)?;
    let updates = player.updates;
    let stopped = updates >= (limit * UPDATES) as usize;
    let spans = player
        .spans
        .into_iter()
        .map(|span| Span { off: span.off.or(Some(updates)), end: span.end.or(Some(updates)), ..span })
        .collect();
    Ok(Score { updates, spans, stopped })
}

#[cfg(test)]
mod tests {
    use super::{gain, pan_gain, pitch, table_word};

    #[test]
    fn the_tables_are_the_engines() {
        // Values the executable's tables hold.
        assert_eq!([table_word(0), table_word(72), table_word(95)], [16, 1024, 3866]);
        assert_eq!([gain(8), gain(127)], [65, 16383]);
        assert_eq!([pan_gain(0), pan_gain(63), pan_gain(126)], [4096, 2896, 0]);
        // The root key at no fine tune plays the sample as recorded.
        assert_eq!(pitch(60, 0, 60, 0), 0x1000);
    }
}

#[cfg(test)]
mod banks {
    /// Every slot of the banks in a folder (`PE2_SOUND_BANKS`) played: each is heard, and each ends by itself or is a
    /// script held or looped until stopped.
    #[test]
    fn every_slot_plays() {
        let Ok(folder) = std::env::var("PE2_SOUND_BANKS") else { return };
        let (mut played, mut silent, mut held) = (0, Vec::new(), Vec::new());
        let mut longest = (0, String::new());
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            let bank = crate::bank::read(&std::fs::read(&path).unwrap()).unwrap();
            let name = path.file_name().unwrap().to_string_lossy()[..8].to_string();
            for (slot, entry) in bank.slots.iter().enumerate() {
                let Some(entry) = *entry else { continue };
                let updates = super::play(&bank, entry, 20.0, &super::Room::default()).unwrap();
                played += 1;
                let peak = updates.iter().flatten().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                if peak < 64 {
                    silent.push(format!("{name}/{slot}"));
                }
                if updates.len() >= (20.0 * super::UPDATES) as usize {
                    held.push(format!("{name}/{slot}"));
                }
                if updates.len() > longest.0 {
                    longest = (updates.len(), format!("{name}/{slot}"));
                }
            }
        }
        eprintln!("{played} slots played; silent {silent:?}; stopped at the limit {held:?}; longest {longest:?}");
        assert_eq!(silent, Vec::<String>::new());
    }
}

#[cfg(test)]
mod reverb {
    /// The executable's Studio B mode (`PE2_EXECUTABLE`) read as its node does, and the banks (`PE2_SOUND_BANKS`) played
    /// through it in the default room: what the reverb adds is heard, bounded, and dies away.
    #[test]
    fn the_banks_play_through_the_games_reverb() {
        let (Ok(exe), Ok(folder)) = (std::env::var("PE2_EXECUTABLE"), std::env::var("PE2_SOUND_BANKS")) else { return };
        let mut args = toml::Table::new();
        args.insert("registers".into(), toml::Value::Integer(0x8006_87A4));
        args.insert("work_area".into(), toml::Value::Integer(0x8006_8160));
        args.insert("node".into(), toml::Value::String("reverb".into()));
        let files = crate::extract_reverb(&std::fs::read(exe).unwrap(), &args).unwrap();
        let mode =
            crate::reverb::Mode::of(&toml::from_str(std::str::from_utf8(&files[0].1).unwrap()).unwrap()).unwrap();
        assert_eq!((mode.registers[2], mode.registers[30], mode.work_area), (0x70F0, 0x8000, 0xF6F8));
        let room = super::Room { mode: Some(mode), setting: super::ROOM };
        let (mut longer, mut slots, mut tails) = (0, 0, 0);
        for entry in std::fs::read_dir(folder).unwrap() {
            let bank = crate::bank::read(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap();
            for entry in bank.slots.iter().flatten() {
                let dry = super::play(&bank, *entry, 10.0, &super::Room::default()).unwrap();
                let wet = super::play(&bank, *entry, 10.0, &room).unwrap();
                slots += 1;
                longer += usize::from(wet.len() > dry.len() + 1);
                tails += usize::from(wet.len() < dry.len() + (4.0 * super::UPDATES) as usize);
            }
        }
        eprintln!("{slots} slots: {longer} ring on past their voices, {tails} die away within four seconds");
        assert_eq!(tails, slots);
    }
}
