//! The SPU's reverb: the voices sent to it are filtered through a delay network in a work area of sound memory, as the
//! hardware does it (Sony's documentation of the SPU, as Martin Korth's describes it) - each step, at 22050 Hz:
//!
//! - its input, scaled (`vLIN`, `vRIN`), feeds the same-side and cross-side reflections (`mLSAME`, `mRDIFF`, ...),
//!   each read back from a delay (`dLSAME`, ...), scaled by the wall (`vWALL`) and smoothed (`vIIR`);
//! - four combs (`mLCOMB1`..4, `vCOMB1`..4) are summed and passed through two all-pass filters (`mLAPF1`, `dAPF1`,
//!   `vAPF1`, ...);
//! - what comes out is scaled by the depth the game sets.
//!
//! Its registers are what the game sets by its one reverb mode (libspu's preset, read from the executable by the
//! `reverb` node): addresses in 8-byte units, the work area from its start to the end of sound memory. Here the input is
//! the average of two samples and the output is held for two: the hardware filters both more finely.

use anyhow::{Context, Result};
use toml::{Table, Value};

/// The registers, in the hardware's order.
pub const REGISTERS: [&str; 32] = [
    "dAPF1", "dAPF2", "vIIR", "vCOMB1", "vCOMB2", "vCOMB3", "vCOMB4", "vWALL", "vAPF1", "vAPF2", "mLSAME", "mRSAME",
    "mLCOMB1", "mRCOMB1", "mLCOMB2", "mRCOMB2", "dLSAME", "dRSAME", "mLDIFF", "mRDIFF", "mLCOMB3", "mRCOMB3",
    "mLCOMB4", "mRCOMB4", "dLDIFF", "dRDIFF", "mLAPF1", "mRAPF1", "mLAPF2", "mRAPF2", "vLIN", "vRIN",
];

/// Sound memory's size, in bytes.
const MEMORY: usize = 0x8_0000;

/// A reverb mode: its registers and where its work area starts (in 8-byte units).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mode {
    pub registers: [u16; 32],
    pub work_area: u16,
}

impl Mode {
    /// A mode from its file (`reverb.toml`): `work_area` and each register by name.
    ///
    /// # Errors
    /// A register is missing or out of range.
    pub fn of(table: &Table) -> Result<Mode> {
        let field = |name: &str| -> Result<u16> {
            let value = table.get(name).and_then(Value::as_integer).with_context(|| format!("no {name}"))?;
            u16::try_from(value).with_context(|| format!("{name} is out of range"))
        };
        let mut registers = [0; 32];
        for (register, name) in registers.iter_mut().zip(REGISTERS) {
            *register = field(name)?;
        }
        Ok(Mode { registers, work_area: field("work_area")? })
    }

    /// Its file.
    #[must_use]
    pub fn table(&self) -> Table {
        let mut table = Table::new();
        table.insert("work_area".into(), Value::Integer(i64::from(self.work_area)));
        for (register, name) in self.registers.iter().zip(REGISTERS) {
            table.insert(name.into(), Value::Integer(i64::from(*register)));
        }
        table
    }

    fn get(&self, name: &str) -> u16 {
        REGISTERS.iter().position(|n| *n == name).map_or(0, |at| self.registers[at])
    }
}

fn saturate(value: i32) -> i32 {
    value.clamp(-0x8000, 0x7FFF)
}

/// A volume register times a sample.
fn scaled(volume: u16, sample: i32) -> i32 {
    (i32::from(volume.cast_signed()) * sample) >> 15
}

/// The registers by name.
#[expect(non_snake_case, reason = "the hardware's names")]
#[derive(Clone, Copy)]
struct Registers {
    dAPF: [usize; 2],
    vIIR: u16,
    vCOMB: [u16; 4],
    vWALL: u16,
    vAPF: [u16; 2],
    mSAME: [usize; 2],
    mCOMB: [[usize; 4]; 2],
    dSAME: [usize; 2],
    mDIFF: [usize; 2],
    dDIFF: [usize; 2],
    mAPF: [[usize; 2]; 2],
    vIN: [u16; 2],
}

impl Registers {
    /// Addresses as halfwords from where the reverb is (8-byte units, four halfwords each).
    fn of(mode: &Mode) -> Registers {
        let v = |name: &str| mode.get(name);
        let a = |name: &str| usize::from(mode.get(name)) * 4;
        Registers {
            dAPF: [a("dAPF1"), a("dAPF2")],
            vIIR: v("vIIR"),
            vCOMB: [v("vCOMB1"), v("vCOMB2"), v("vCOMB3"), v("vCOMB4")],
            vWALL: v("vWALL"),
            vAPF: [v("vAPF1"), v("vAPF2")],
            mSAME: [a("mLSAME"), a("mRSAME")],
            mCOMB: [
                [a("mLCOMB1"), a("mLCOMB2"), a("mLCOMB3"), a("mLCOMB4")],
                [a("mRCOMB1"), a("mRCOMB2"), a("mRCOMB3"), a("mRCOMB4")],
            ],
            dSAME: [a("dLSAME"), a("dRSAME")],
            mDIFF: [a("mLDIFF"), a("mRDIFF")],
            // Each side's cross reflection reads the other side's delay.
            dDIFF: [a("dRDIFF"), a("dLDIFF")],
            mAPF: [[a("mLAPF1"), a("mLAPF2")], [a("mRAPF1"), a("mRAPF2")]],
            vIN: [v("vLIN"), v("vRIN")],
        }
    }
}

/// The reverb running: its work area and where it is in it.
pub struct Reverb {
    registers: Registers,
    depth: u16,
    work: Vec<i32>,
    at: usize,
    /// The input of the half-step before, and the output held.
    pending: Option<(i32, i32)>,
    out: (i32, i32),
}

impl Reverb {
    #[must_use]
    pub fn new(mode: &Mode, depth: u16) -> Reverb {
        let start = usize::from(mode.work_area) * 8;
        let halfwords = MEMORY.saturating_sub(start).max(2) / 2;
        Reverb { registers: Registers::of(mode), depth, work: vec![0; halfwords], at: 0, pending: None, out: (0, 0) }
    }

    /// A place in the work area: `offset` halfwords from where it is, `back` before that.
    fn place(&self, offset: usize, back: usize) -> usize {
        let len = self.work.len();
        (self.at + offset % len + len - back % len) % len
    }

    fn read(&self, offset: usize, back: usize) -> i32 {
        self.work[self.place(offset, back)]
    }

    fn write(&mut self, offset: usize, value: i32) {
        let at = self.place(offset, 0);
        self.work[at] = saturate(value);
    }

    /// One sample's input, left and right; the reverb's output for it.
    pub fn sample(&mut self, left: i32, right: i32) -> (i32, i32) {
        match self.pending.take() {
            None => self.pending = Some((left, right)),
            Some((l, r)) => self.out = self.step([i32::midpoint(l, left), i32::midpoint(r, right)]),
        }
        self.out
    }

    /// One step at 22050 Hz, both sides.
    fn step(&mut self, input: [i32; 2]) -> (i32, i32) {
        let r = self.registers;
        let (iir, wall) = (r.vIIR, r.vWALL);
        let mut writes = Vec::with_capacity(4);
        for side in 0..2 {
            let scaled_in = scaled(r.vIN[side], saturate(input[side]));
            for (at, delayed) in [(r.mSAME[side], r.dSAME[side]), (r.mDIFF[side], r.dDIFF[side])] {
                let back = self.read(at, 1);
                writes.push((at, scaled(iir, scaled_in + scaled(wall, self.read(delayed, 0)) - back) + back));
            }
        }
        for (at, value) in writes {
            self.write(at, value);
        }
        let mut out = [0; 2];
        for (side, out) in out.iter_mut().enumerate() {
            let mut sum: i32 = (0..4).map(|n| scaled(r.vCOMB[n], self.read(r.mCOMB[side][n], 0))).sum();
            for n in 0..2 {
                let (volume, delay, at) = (r.vAPF[n], r.dAPF[n], r.mAPF[side][n]);
                let delayed = self.read(at, delay);
                let fed = saturate(sum - scaled(volume, delayed));
                self.write(at, fed);
                sum = saturate(scaled(volume, fed) + delayed);
            }
            *out = sum;
        }
        self.at = (self.at + 1) % self.work.len();
        (scaled(self.depth, out[0]), scaled(self.depth, out[1]))
    }
}
