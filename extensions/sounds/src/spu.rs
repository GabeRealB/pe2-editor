//! The SPU's voices as the game drives them, without reverb: each plays a sample's SPU-ADPCM body at its pitch through
//! its ADSR envelope and its two volumes, and the voices are summed at 44100 Hz.
//!
//! As the hardware does it (Sony's documentation of the SPU, as Martin Korth's describes it):
//!
//! - A body is 16-byte blocks of 28 samples: a shift and a filter, flags, and 4-bit codes. A block flagged as the loop's
//!   start sets where the voice goes back to; one flagged as the end goes back there, and where it is not flagged to
//!   repeat, its envelope is silenced at once.
//! - The pitch is a step in 1/4096 of a sample (0x1000 plays the body at 44100 Hz); four samples about where it is are
//!   weighed together. The hardware weighs them by a table of a bell curve, which is approximated here by one: a
//!   Gaussian of 0.55 samples, its four weights summing to one.
//! - The envelope runs attack, decay, sustain and release, each a rate - a shift and a step, linear or exponential - and
//!   the sustain level; the level moves by the step every so many samples.
//! - A voice's output is its sample times its envelope's level, times each volume (direct mode, as the game sets it).

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "the SPU's registers: 4-bit codes, 16-bit samples, pitches and volumes, 15-bit levels, and a table's indices - each value within what it is cast to"
)]

/// The rate the SPU plays at.
pub const RATE: u32 = 44100;
/// The voices it has.
pub const VOICES: usize = 24;

const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

/// Where an envelope is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Attack,
    Decay,
    Sustain,
    Release,
}

/// What one voice is set to and where it is.
#[derive(Clone, Debug)]
pub struct Voice {
    body: std::sync::Arc<Vec<u8>>,
    /// The block decoded, the block to go back to, and the samples decoded of the block.
    block: usize,
    repeat: usize,
    decoded: [i32; 28],
    at: usize,
    history: (i32, i32),
    /// The last four samples, newest last, and the pitch counter (in 1/4096 of a sample).
    recent: [i32; 4],
    counter: u32,
    pub pitch: u16,
    pub adsr1: u16,
    pub adsr2: u16,
    pub left: i16,
    pub right: i16,
    phase: Phase,
    level: i32,
    wait: u32,
    /// Keyed on and not yet off.
    pub keyed: bool,
    /// Sent to the reverb.
    pub reverb: bool,
}

impl Voice {
    /// A voice keyed on: the body from its first block, its envelope from silence.
    #[must_use]
    pub fn keyed_on(body: std::sync::Arc<Vec<u8>>, pitch: u16, adsr: (u16, u16), volume: (i16, i16)) -> Voice {
        let mut voice = Voice {
            body,
            block: 0,
            repeat: 0,
            decoded: [0; 28],
            at: 28,
            history: (0, 0),
            recent: [0; 4],
            counter: 0,
            pitch,
            adsr1: adsr.0,
            adsr2: adsr.1,
            left: volume.0,
            right: volume.1,
            phase: Phase::Attack,
            level: 0,
            wait: 0,
            keyed: true,
            reverb: false,
        };
        voice.decode_block();
        voice
    }

    /// The envelope's level: 0 is silence.
    #[must_use]
    pub fn level(&self) -> i32 {
        self.level
    }

    pub fn key_off(&mut self) {
        self.keyed = false;
        self.phase = Phase::Release;
        self.wait = 0;
    }

    fn decode_block(&mut self) {
        let start = self.block * 16;
        let Some(frame) = self.body.get(start..start + 16) else {
            // Past the body: silence, as memory there would be read.
            self.decoded = [0; 28];
            self.at = 0;
            return;
        };
        let mut shift = u32::from(frame[0] & 0xF);
        if shift > 12 {
            shift = 9;
        }
        let (f0, f1) = FILTERS[usize::from((frame[0] >> 4) & 7).min(4)];
        if frame[1] & 4 != 0 {
            self.repeat = self.block;
        }
        let (mut s1, mut s2) = self.history;
        for i in 0..28 {
            let nibble = i32::from((frame[2 + i / 2] >> (4 * (i & 1))) & 0xF);
            let code = i32::from((nibble << 12) as i16) >> shift;
            let sample = (code + ((s1 * f0 + s2 * f1 + 32) >> 6)).clamp(-32768, 32767);
            (s2, s1) = (s1, sample);
            self.decoded[i] = sample;
        }
        self.history = (s1, s2);
        self.at = 0;
    }

    fn next_block(&mut self) {
        let flags = self.body.get(self.block * 16 + 1).copied().unwrap_or(0);
        if flags & 1 != 0 {
            self.block = self.repeat;
            if flags & 2 == 0 {
                self.phase = Phase::Release;
                self.level = 0;
            }
        } else {
            self.block += 1;
        }
        self.decode_block();
    }

    /// The step of the envelope's phase now: its rate (shift and step) and whether it is exponential, decreasing.
    fn rate(&self) -> (u32, i32, bool, bool, Option<i32>) {
        let (a1, a2) = (u32::from(self.adsr1), u32::from(self.adsr2));
        match self.phase {
            Phase::Attack => ((a1 >> 10) & 0x1F, 7 - ((a1 >> 8) & 3) as i32, a1 & 0x8000 != 0, false, None),
            Phase::Decay => {
                let sustain = (((a1 & 0xF) + 1) * 0x800) as i32;
                ((a1 >> 4) & 0xF, -8, true, true, Some(sustain))
            }
            Phase::Sustain => {
                let decreasing = a2 & 0x4000 != 0;
                let bits = ((a2 >> 6) & 3) as i32;
                ((a2 >> 8) & 0x1F, if decreasing { -8 + bits } else { 7 - bits }, a2 & 0x8000 != 0, decreasing, None)
            }
            Phase::Release => (a2 & 0x1F, -8, a2 & 0x20 != 0, true, None),
        }
    }

    fn step_envelope(&mut self) {
        let (shift, step, exponential, decreasing, sustain) = self.rate();
        let mut cycles = 1_u32 << shift.saturating_sub(11);
        let mut step = step << 11_u32.saturating_sub(shift);
        if exponential && !decreasing && self.level > 0x6000 {
            cycles *= 4;
        }
        if exponential && decreasing {
            step = (step * self.level) >> 15;
        }
        self.wait += 1;
        if self.wait < cycles {
            return;
        }
        self.wait = 0;
        self.level = (self.level + step).clamp(0, 0x7FFF);
        match self.phase {
            Phase::Attack if self.level >= 0x7FFF => self.phase = Phase::Decay,
            Phase::Decay if sustain.is_some_and(|s| self.level <= s) => self.phase = Phase::Sustain,
            _ => {}
        }
    }

    /// The next output sample, left and right.
    fn sample(&mut self, weights: &[[i32; 4]; 256]) -> (i32, i32) {
        self.step_envelope();
        let w = &weights[((self.counter >> 4) & 0xFF) as usize];
        let interpolated =
            (self.recent[0] * w[0] + self.recent[1] * w[1] + self.recent[2] * w[2] + self.recent[3] * w[3]) >> 15;
        self.counter += u32::from(self.pitch.min(0x3FFF));
        while self.counter >= 0x1000 {
            self.counter -= 0x1000;
            if self.at >= 28 {
                self.next_block();
            }
            self.recent = [self.recent[1], self.recent[2], self.recent[3], self.decoded[self.at]];
            self.at += 1;
        }
        let enveloped = (interpolated * self.level) >> 15;
        ((enveloped * i32::from(self.left) * 2) >> 15, (enveloped * i32::from(self.right) * 2) >> 15)
    }
}

/// The four weights for each of 256 places between two samples, oldest first, in 1/32768: a Gaussian of 0.55 samples
/// about the place, the hardware's table approximated.
fn weights() -> [[i32; 4]; 256] {
    let mut table = [[0; 4]; 256];
    for (i, row) in table.iter_mut().enumerate() {
        let f = i as f64 / 256.0;
        // The place lies between the second and third of the four, `f` past the second.
        let distances = [1.0 + f, f, 1.0 - f, 2.0 - f];
        let raw = distances.map(|d: f64| (-(d * d) / (2.0 * 0.55 * 0.55)).exp());
        let sum: f64 = raw.iter().sum();
        *row = raw.map(|w| (w / sum * 32768.0).round() as i32);
    }
    table
}

/// The SPU's voices, played together, and its reverb where it has one.
pub struct Spu {
    pub voices: Vec<Option<Voice>>,
    pub reverb: Option<crate::reverb::Reverb>,
    weights: Box<[[i32; 4]; 256]>,
}

impl Default for Spu {
    fn default() -> Spu {
        Spu { voices: vec![None; VOICES], reverb: None, weights: Box::new(weights()) }
    }
}

impl Spu {
    /// `count` samples of every voice, summed: left and right, interleaved.
    pub fn render(&mut self, count: usize, out: &mut Vec<i16>) {
        for _ in 0..count {
            let (mut left, mut right, mut sent_left, mut sent_right) = (0, 0, 0, 0);
            for voice in self.voices.iter_mut().flatten() {
                let (l, r) = voice.sample(&self.weights);
                left += l;
                right += r;
                if voice.reverb {
                    sent_left += l;
                    sent_right += r;
                }
            }
            if let Some(reverb) = self.reverb.as_mut() {
                let (l, r) = reverb.sample(sent_left, sent_right);
                left += l;
                right += r;
            }
            out.push(left.clamp(-32768, 32767) as i16);
            out.push(right.clamp(-32768, 32767) as i16);
        }
    }
}
