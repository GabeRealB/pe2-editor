//! SPU-ADPCM sample bodies: Sony's encoder, and the search for input it turns into a given body.
//!
//! The encoder is ENCVAG's `EncVag` (Psy-Q's `ENCVAG.DLL`, and AIFF2VAG 3.2 and 3.3, which give identical output), in
//! its own float64 operation order. Per 28-sample frame: the input clamped to -30720..30719; each of the five filters'
//! residual peak over the frame, the filter the one with the smallest weighted peak (the mode's weights) unless filter
//! 0's own peak is 7 or less; the shift from the peak by ENCVAG's bit test; then each sample quantized with error
//! feedback, `(int)(s * 2^shift) + 0x800 & ~0xFFF`, saturating.
//!
//! A body as a bank stores it: a zero start block; the frames; and after a one-shot, the end block `00 07 77..`.
//! Flags: a one-shot's first frame 4, its last 1; a loop's frames 2, the loop start 6, the last 3.
//!
//! The decoder finds input the encoder turns into a stored body. Write Y = clamped input + quantization error: the
//! encoder's arithmetic gives Y[k] = Q[k] - f0 Y[k-1] - f1 Y[k-2], with Q the quantized value it feeds back - the
//! stored code shifted, except where a code 7 saturates (the encoder clamps to 32767, so it feeds back 32767 >> shift
//! rather than 28672 >> shift). So each input lies in an interval fixed by the codes and those saturation choices
//! alone, and inputs interact only through each frame's filter and shift selection: the stored filter's residual peak
//! must fall in the shift's range and the other filters' weighted peaks must not be smaller. Per frame it tries, in
//! order of cost: the nearest inputs with the stored filter's residual under the cap; the inputs closest to their
//! prediction; local repair of either; an exact solve over the frame and up to two before it (with the saturation
//! choices fixed the constraints are linear in three consecutive inputs, so the feasible pairs form a convex polygon,
//! carried forward and read back, with the other filters' conditions added as witness half-planes where a candidate
//! misses them); repair under each saturation choice; joint repair and seeded annealing with earlier frames - backing
//! up to an earlier frame's next candidate when a frame has none. A sample is solved in the most restrictive mode its
//! filters fit, then in normal mode from that solution; its mode is normal wherever that reproduces it.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "the encoder's arithmetic: inputs clamped to -30720..30719, codes of four bits, shifts under 13, frame \
              counts far under 2^52 - each value within what it is cast to"
)]
#![expect(
    clippy::many_single_char_names,
    reason = "the encoder's and the solve's quantities, named as their formulas are"
)]

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

/// The modes' filter weights: what each filter's peak is weighed by when choosing one.
pub const MODES: [(&str, [f64; 5]); 4] = [
    ("normal", [1.0, 1.0, 1.0, 1.0, 1.0]),
    ("high", [1.0, 1.0, 1000.0, 1000.0, 1.0]),
    ("low", [1.0, 1.0, 1000.0, 1.0, 1000.0]),
    ("4bit", [1.0, 1000.0, 1000.0, 1000.0, 1000.0]),
];
const F: [(f64, f64); 5] = [
    (0.0, 0.0),
    (-60.0 / 64.0, 0.0),
    (-115.0 / 64.0, 52.0 / 64.0),
    (-98.0 / 64.0, 55.0 / 64.0),
    (-122.0 / 64.0, 60.0 / 64.0),
];
const LO: i64 = -30720;
const HI: i64 = 30719;
const EPS: f64 = 1e-7;
/// The block after a one-shot sound's last frame.
pub const END_BLOCK: [u8; 16] =
    [0x00, 0x07, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77];

type Frame = [u8; 16];

/// A mode's weights, by name.
#[must_use]
pub fn weights(mode: &str) -> Option<[f64; 5]> {
    MODES.iter().find(|(name, _)| *name == mode).map(|(_, w)| *w)
}

fn allowed(mode: &str) -> &'static [usize] {
    match mode {
        "4bit" => &[0],
        "low" => &[0, 1, 3],
        "high" => &[0, 1, 4],
        _ => &[0, 1, 2, 3, 4],
    }
}

/// ENCVAG's shift for a peak, by its bit test.
fn shift_of(m: i64) -> u32 {
    let (mut mask, mut shift) = (0x4000_i64, 0_u32);
    while mask & (m + (mask >> 3)) == 0 {
        shift += 1;
        mask >>= 1;
        if shift >= 12 {
            break;
        }
    }
    shift
}

/// Each shift's peak ranges: the runs of peaks the bit test maps to it.
fn runs() -> &'static BTreeMap<u32, Vec<(i64, i64)>> {
    static RUNS: std::sync::OnceLock<BTreeMap<u32, Vec<(i64, i64)>>> = std::sync::OnceLock::new();
    RUNS.get_or_init(|| {
        let mut runs: BTreeMap<u32, Vec<(i64, i64)>> = BTreeMap::new();
        let mut previous = None;
        for p in 0..32768_i64 {
            let s = shift_of(p);
            let list = runs.entry(s).or_default();
            if previous == Some(s) {
                list.last_mut().expect("a run begun").1 = p;
            } else {
                list.push((p, p));
            }
            previous = Some(s);
        }
        runs
    })
}

fn run_list(shift: u32) -> &'static [(i64, i64)] {
    runs().get(&shift).map_or(&[], Vec::as_slice)
}

#[derive(Clone, Copy, Debug, Default)]
struct State {
    s1: f64,
    s2: f64,
    e1: f64,
    e2: f64,
}

fn clamp(v: i64) -> f64 {
    v.clamp(LO, HI) as f64
}

fn quantize(s0: f64, shift: u32) -> i64 {
    (((s0 * f64::from(1_u32 << shift)) as i64 + 0x800) & !0xFFF).clamp(-32768, 32767)
}

fn peaks(st: &State, x: &[i64]) -> [f64; 5] {
    let mut out = [0.0; 5];
    for (i, (f0, f1)) in F.iter().enumerate() {
        let (mut a, mut b, mut m) = (st.s1, st.s2, 0.0_f64);
        for &v in x {
            let s = clamp(v);
            let d = (s + a * f0 + b * f1).abs();
            if d > m {
                m = d;
            }
            b = a;
            a = s;
        }
        out[i] = m;
    }
    out
}

fn choose(pk: &[f64; 5], w: &[f64; 5]) -> usize {
    let (mut best, mut filt) = (1.8e307, 0);
    for (i, &m) in pk.iter().enumerate() {
        if w[i] * m < best {
            best = w[i] * m;
            filt = i;
        }
        if i == 0 && m <= 7.0 {
            return 0;
        }
    }
    filt
}

/// ENCVAG for one frame; advances `st`.
fn encode_frame(st: &mut State, x: &[i64], flags: u8, w: &[f64; 5]) -> Frame {
    let pk = peaks(st, x);
    let filt = choose(&pk, w);
    let (f0, f1) = F[filt];
    let m = (pk[filt] as i64).clamp(-32768, 32767);
    let shift = if m >= 0 { shift_of(m) } else { 0 };
    let mut codes = [0_u8; 28];
    let (mut a, mut b, mut e1, mut e2) = (st.s1, st.s2, st.e1, st.e2);
    for (k, &v) in x.iter().enumerate() {
        let s = clamp(v);
        let d = s + a * f0 + b * f1;
        b = a;
        a = s;
        let s0 = d + e1 * f0 + e2 * f1;
        let di = quantize(s0, shift);
        codes[k] = ((di >> 12) & 0xF) as u8;
        e2 = e1;
        e1 = (di >> shift) as f64 - s0;
    }
    *st = State { s1: a, s2: b, e1, e2 };
    let mut out = [0_u8; 16];
    out[0] = ((filt as u8) << 4) | shift as u8;
    out[1] = flags;
    for k in (0..28).step_by(2) {
        out[2 + k / 2] = codes[k] | (codes[k + 1] << 4);
    }
    out
}

/// A sound as the encoder writes it: its frames (`pcm`'s length a multiple of 28), flagged a one-shot (`loop_start`
/// none) or looping from a frame, between the zero start block and, after a one-shot, the end block.
///
/// # Panics
/// The length is not a multiple of 28.
#[must_use]
pub fn encode(pcm: &[i64], mode: &[f64; 5], loop_start: Option<usize>) -> Vec<u8> {
    assert!(pcm.len().is_multiple_of(28), "a sound's length is not a multiple of 28 samples");
    let frames = pcm.len() / 28;
    let mut st = State::default();
    let mut out = vec![0_u8; 16];
    for k in 0..frames {
        let flag = match loop_start {
            None if k == frames - 1 => 1,
            None if k == 0 => 4,
            None => 0,
            Some(_) if k == frames - 1 => 3,
            Some(l) if k == l => 6,
            Some(_) => 2,
        };
        out.extend_from_slice(&encode_frame(&mut st, &pcm[28 * k..28 * k + 28], flag, mode));
    }
    if loop_start.is_none() {
        out.extend_from_slice(&END_BLOCK);
    }
    out
}

fn nib(fr: &Frame, i: usize) -> i64 {
    let n = i64::from((fr[2 + i / 2] >> (4 * (i & 1))) & 0xF);
    if n & 8 != 0 { n - 16 } else { n }
}

fn body_frames(body: &[u8]) -> Vec<Frame> {
    let mut frames: Vec<Frame> = body.get(16..).unwrap_or_default().as_chunks::<16>().0.to_vec();
    if frames.last() == Some(&END_BLOCK) {
        frames.pop();
    }
    frames
}

fn residuals(s1: f64, s2: f64, x: &[i64]) -> [Vec<f64>; 5] {
    let xs: Vec<f64> = x.iter().map(|&v| clamp(v)).collect();
    std::array::from_fn(|j| {
        let (f0, f1) = F[j];
        (0..xs.len())
            .map(|k| {
                let a = if k >= 1 { xs[k - 1] } else { s1 };
                let b = if k >= 2 {
                    xs[k - 2]
                } else if k == 1 {
                    s1
                } else {
                    s2
                };
                xs[k] + a * f0 + b * f1
            })
            .collect()
    })
}

fn peak(r: &[f64]) -> f64 {
    r.iter().fold(0.0_f64, |m, v| m.max(v.abs()))
}

/// 0 when the encoder selects (filt, shift) on x; otherwise a penalty that falls with each residual moved the right
/// way: residuals over the shift's cap, the peak under its floor, the stored filter's residuals over another filter's
/// weighted peak, filter 0's peak at 7 or below.
fn violation(s1: f64, s2: f64, x: &[i64], filt: usize, shift: u32, w: &[f64; 5]) -> (f64, [Vec<f64>; 5], [f64; 5]) {
    let r = residuals(s1, s2, x);
    let pk: [f64; 5] = std::array::from_fn(|j| peak(&r[j]));
    let shift_pen = |rj: &[f64], m: f64| {
        let mut best: Option<f64> = None;
        for &(lo, hi) in run_list(shift) {
            let lim = hi as f64 + 1.0 - EPS;
            let mut pen = (lo as f64 - m).max(0.0);
            for v in rj {
                let d = v.abs();
                if d > lim {
                    pen += d - lim;
                }
            }
            best = Some(best.map_or(pen, |b: f64| b.min(pen)));
        }
        best.unwrap_or(0.0)
    };
    if filt == 0 && pk[0] <= 7.0 {
        let v = shift_pen(&r[0], pk[0]);
        return (v, r, pk);
    }
    let mut v = shift_pen(&r[filt], pk[filt]);
    let wf = w[filt];
    for j in 0..5 {
        if j != filt {
            let cap = w[j] * pk[j] - if j < filt { EPS } else { 0.0 };
            if wf * pk[filt] > cap {
                let mut sum = 0.0;
                for t in &r[filt] {
                    let d = wf * t.abs() - cap;
                    if d > 0.0 {
                        sum += d;
                    }
                }
                v += sum / wf;
            }
        }
    }
    if filt != 0 {
        v += (7.0 + EPS - pk[0]).max(0.0);
    }
    (v, r, pk)
}

/// Each input's interval and centre, the code-7 positions where both saturation branches are possible, the default
/// input and the state after.
#[derive(Clone, Debug)]
struct Bounds {
    lo: Vec<i64>,
    hi: Vec<i64>,
    cen: Vec<f64>,
    two: Vec<usize>,
    xs: Vec<i64>,
}

/// The bounds of a frame from a state, the code-7 positions in `sat` taken saturating; or the branch points so far
/// where a code cannot be met.
fn frame_bounds(fr: &Frame, st: &State, sat: &BTreeSet<usize>) -> Result<Bounds, Vec<usize>> {
    let filt = usize::from(fr[0] >> 4).min(4);
    let shift = u32::from(fr[0] & 15);
    let (f0, f1) = F[filt];
    let scale = f64::from(1_u32 << shift);
    let cap = run_list(shift).first().map_or(0, |r| r.1) as f64 + 1.0 - 1e-6;
    let (mut a, mut b, mut e1, mut e2) = (st.s1, st.s2, st.e1, st.e2);
    let mut out = Bounds { lo: vec![], hi: vec![], cen: vec![], two: vec![], xs: vec![] };
    for i in 0..28 {
        let c = nib(fr, i);
        let s0_of = |v: i64| clamp(v) + a * f0 + b * f1 + e1 * f0 + e2 * f1;
        let code = |v: i64| {
            let di = quantize(s0_of(v), shift);
            (di >> 12, di)
        };
        let c0 = a * f0 + b * f1;
        let fb = e1 * f0 + e2 * f1;
        let mut centre = ((c << 12) >> shift) as f64 - c0 - fb;
        let p = c0 + fb;
        let mut lo = if c == -8 { LO } else { LO.max(((c * 4096 - 2048) as f64 / scale - p).floor() as i64 - 1) };
        let mut hi = if c == 7 { HI } else { HI.min(((c * 4096 + 2048) as f64 / scale - p).ceil() as i64 + 1) };
        while lo <= hi && code(lo).0 != c {
            lo += 1;
        }
        while hi >= lo && code(hi).0 != c {
            hi -= 1;
        }
        if lo > hi {
            return Err(out.two);
        }
        if c == 7 {
            let mut t = lo.max((30720.0 / scale - p).floor() as i64 - 1);
            while t <= hi && code(t).1 != 32767 {
                t += 1;
            }
            while t > lo && code(t - 1).1 == 32767 {
                t -= 1;
            }
            if lo < t && t <= hi {
                out.two.push(i);
            }
            if (sat.contains(&i) && t <= hi) || t == lo {
                lo = t;
                centre = f64::from(32767 >> shift) - c0 - fb;
            } else {
                hi = t - 1;
            }
        }
        let l3 = lo.max((-cap - c0).ceil() as i64);
        let h3 = hi.min((cap - c0).floor() as i64);
        let near = centre.round_ties_even() as i64;
        let v = if l3 <= h3 { near.clamp(l3, h3) } else { near.clamp(lo, hi) };
        out.lo.push(lo);
        out.hi.push(hi);
        out.cen.push(centre);
        out.xs.push(v);
        let s0 = s0_of(v);
        let di = quantize(s0, shift);
        e2 = e1;
        e1 = (di >> shift) as f64 - s0;
        b = a;
        a = clamp(v);
    }
    Ok(out)
}

/// The second default: each input as close to its prediction as its interval allows, then, if the peak falls under
/// the shift's floor, the input with most room pushed out to meet it.
fn tight(fr: &Frame, st: &State, got: &Bounds) -> Vec<i64> {
    let filt = usize::from(fr[0] >> 4).min(4);
    let shift = u32::from(fr[0] & 15);
    let (f0, f1) = F[filt];
    let floor = run_list(shift).first().map_or(0, |r| r.0) as f64;
    let (mut a, mut b) = (st.s1, st.s2);
    let (mut x, mut reach) = (Vec::with_capacity(28), Vec::with_capacity(28));
    for k in 0..28 {
        let c0 = a * f0 + b * f1;
        let v = ((-c0).round_ties_even() as i64).clamp(got.lo[k], got.hi[k]);
        x.push(v);
        reach.push(((got.lo[k] as f64 + c0).abs()).max((got.hi[k] as f64 + c0).abs()));
        b = a;
        a = clamp(v);
    }
    let r = &residuals(st.s1, st.s2, &x)[filt];
    if peak(r) < floor {
        let mut k = 0;
        for i in 1..28 {
            if reach[i] > reach[k] {
                k = i;
            }
        }
        let a = if k == 0 { st.s1 } else { x[k - 1] as f64 };
        let b = if k == 0 {
            st.s2
        } else if k == 1 {
            st.s1
        } else {
            x[k - 2] as f64
        };
        let c0 = a * f0 + b * f1;
        let cand: Vec<i64> = [(floor - c0).ceil() as i64, (-floor - c0).floor() as i64]
            .into_iter()
            .filter(|&v| got.lo[k] <= v && v <= got.hi[k])
            .collect();
        if let Some(&v) = cand.iter().min_by_key(|&&v| (v - x[k]).abs()) {
            x[k] = v;
        }
    }
    x
}

/// A frame in a window being repaired: its header, intervals and centres, and its input.
#[derive(Clone)]
struct WinFrame {
    hdr: u8,
    lo: Vec<i64>,
    hi: Vec<i64>,
    cen: Vec<f64>,
    x: Vec<i64>,
}

/// Consecutive frames repaired together: the inputs before the first, and the frames.
struct Win<'w> {
    s1: f64,
    s2: f64,
    f: Vec<WinFrame>,
    w: &'w [f64; 5],
    vs: Vec<f64>,
}

type Info = Vec<(f64, [Vec<f64>; 5], [f64; 5])>;

impl Win<'_> {
    fn before(&self, i: usize) -> (f64, f64) {
        if i == 0 {
            (self.s1, self.s2)
        } else {
            let xp = &self.f[i - 1].x;
            (clamp(xp[27]), clamp(xp[26]))
        }
    }

    fn total(&mut self) -> (f64, Info) {
        let mut tot = 0.0;
        let mut info = Vec::with_capacity(self.f.len());
        for i in 0..self.f.len() {
            let (s1, s2) = self.before(i);
            let fr = &self.f[i];
            let got = violation(s1, s2, &fr.x, usize::from(fr.hdr >> 4).min(4), u32::from(fr.hdr & 15), self.w);
            tot += got.0;
            info.push(got);
        }
        self.vs = info.iter().map(|i| i.0).collect();
        (tot, info)
    }

    /// `total`'s penalty, given that only the inputs `moved` differ from those of the last total or commit.
    fn trial(&self, moved: &[(usize, usize)]) -> (f64, BTreeMap<usize, f64>) {
        let mut touched = BTreeSet::new();
        for &(w, k) in moved {
            touched.insert(w);
            if k >= 26 && w + 1 < self.f.len() {
                touched.insert(w + 1);
            }
        }
        let (mut tot, mut new) = (0.0, BTreeMap::new());
        for (i, &v) in self.vs.iter().enumerate() {
            let v = if touched.contains(&i) {
                let (s1, s2) = self.before(i);
                let fr = &self.f[i];
                let got = violation(s1, s2, &fr.x, usize::from(fr.hdr >> 4).min(4), u32::from(fr.hdr & 15), self.w).0;
                new.insert(i, got);
                got
            } else {
                v
            };
            tot += v;
        }
        (tot, new)
    }

    fn commit(&mut self, new: &BTreeMap<usize, f64>) {
        for (&i, &v) in new {
            self.vs[i] = v;
        }
    }

    fn prev_of(&self, w: usize, k: usize) -> (f64, f64) {
        let xw = &self.f[w].x;
        if k >= 2 {
            return (clamp(xw[k - 1]), clamp(xw[k - 2]));
        }
        let (s1, s2) = self.before(w);
        if k == 1 { (clamp(xw[0]), s1) } else { (s1, s2) }
    }
}

fn hot(win: &Win<'_>, info: &Info) -> Vec<(usize, usize)> {
    let mut hot = BTreeSet::new();
    for (w, (v, r, pk)) in info.iter().enumerate() {
        if *v == 0.0 {
            continue;
        }
        let filt = usize::from(win.f[w].hdr >> 4).min(4);
        let rf = &r[filt];
        let mut ks = BTreeSet::new();
        for rj in r {
            let mut best = 0;
            for k in 1..28 {
                if rj[k].abs() > rj[best].abs() {
                    best = k;
                }
            }
            ks.insert(best);
        }
        let low = pk.iter().copied().fold(f64::INFINITY, f64::min);
        for (k, val) in rf.iter().enumerate() {
            if val.abs() > low - 2.0 {
                ks.insert(k);
            }
        }
        for k in ks {
            for d in 0..3_usize {
                let (ww, kk) = if k >= d { (Some(w), k - d) } else { (w.checked_sub(1), k + 28 - d) };
                if let Some(ww) = ww {
                    hot.insert((ww, kk));
                }
            }
        }
    }
    hot.into_iter().collect()
}

/// Candidate inputs at k: the interval's ends and a few steps, and those putting a filter's residual exactly at a
/// target magnitude, given the two inputs before k.
fn values(fr: &WinFrame, k: usize, targets: &[(usize, f64)], prev: (f64, f64)) -> BTreeSet<i64> {
    let span = fr.hi[k] - fr.lo[k];
    let x = fr.x[k];
    let mut vals: BTreeSet<i64> = [
        fr.lo[k],
        fr.hi[k],
        x - 1,
        x + 1,
        x - (span / 4).max(1),
        x + (span / 4).max(1),
        fr.cen[k].round_ties_even() as i64,
        (fr.lo[k] + fr.hi[k]).div_euclid(2),
    ]
    .into_iter()
    .collect();
    let (a, b) = prev;
    for &(j, t) in targets {
        let p = a * F[j].0 + b * F[j].1;
        for sgn in [1.0, -1.0] {
            let v = sgn * t - p;
            vals.insert(v.floor() as i64);
            vals.insert(v.ceil() as i64);
        }
    }
    vals.retain(|&v| fr.lo[k] <= v && v <= fr.hi[k] && v != x);
    vals
}

/// A move of a repair: its penalty and how far it strays, and the inputs it sets.
type Move = ((f64, f64), Vec<(usize, usize, i64)>);

/// Greedy moves (single, then pairs) among the inputs feeding the violated peaks, until the selection holds.
fn repair(win: &mut Win<'_>, rounds: usize) -> bool {
    let (mut cur, mut info) = win.total();
    for _ in 0..rounds {
        if cur == 0.0 {
            return true;
        }
        let hot = hot(win, &info);
        let mut targets = Vec::new();
        for (wi, (v, _r, pk)) in info.iter().enumerate() {
            if *v == 0.0 {
                continue;
            }
            let filt = usize::from(win.f[wi].hdr >> 4).min(4);
            let shift = u32::from(win.f[wi].hdr & 15);
            for &(lo, hi) in run_list(shift) {
                targets.push((filt, hi as f64 + 0.5));
                targets.push((filt, lo as f64 + 0.5));
            }
            for (j, &pj) in pk.iter().enumerate() {
                targets.push((j, pk[filt] + 1.0));
                targets.push((filt, pj - 1.0));
            }
        }
        // The best single move: by penalty, then by distance from the centre.
        let mut best: Option<Move> = None;
        for &(w, k) in &hot {
            let old = win.f[w].x[k];
            let prev = win.prev_of(w, k);
            for v in values(&win.f[w], k, &targets, prev) {
                win.f[w].x[k] = v;
                let val = win.trial(&[(w, k)]).0;
                win.f[w].x[k] = old;
                let cen = win.f[w].cen[k];
                let key = (val, (v as f64 - cen).abs() - (old as f64 - cen).abs());
                if best.as_ref().is_none_or(|b| key.partial_cmp(&b.0) == Some(std::cmp::Ordering::Less)) {
                    best = Some((key, vec![(w, k, v)]));
                }
            }
        }
        if best.as_ref().is_none_or(|b| b.0.0 >= cur) {
            for (i, &(w1, k1)) in hot.iter().enumerate() {
                for &(w2, k2) in &hot[i + 1..] {
                    let o1 = win.f[w1].x[k1];
                    let o2 = win.f[w2].x[k2];
                    let prev1 = win.prev_of(w1, k1);
                    for v1 in values(&win.f[w1], k1, &targets, prev1) {
                        win.f[w1].x[k1] = v1;
                        let prev2 = win.prev_of(w2, k2);
                        for v2 in values(&win.f[w2], k2, &targets, prev2) {
                            win.f[w2].x[k2] = v2;
                            let val = win.trial(&[(w1, k1), (w2, k2)]).0;
                            win.f[w2].x[k2] = o2;
                            if val < cur && best.as_ref().is_none_or(|b| (val, 0.0) < b.0) {
                                best = Some(((val, 0.0), vec![(w1, k1, v1), (w2, k2, v2)]));
                            }
                        }
                        win.f[w1].x[k1] = o1;
                    }
                }
            }
            if best.as_ref().is_none_or(|b| b.0.0 >= cur) {
                return false;
            }
        }
        for (w, k, v) in best.expect("a move").1 {
            win.f[w].x[k] = v;
        }
        (cur, info) = win.total();
    }
    cur == 0.0
}

/// A small seeded generator: what is annealed is the same every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1_u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn gauss(&mut self, sigma: f64) -> f64 {
        let u1 = self.unit().max(1e-300);
        let u2 = self.unit();
        sigma * (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// Simulated annealing over the window's inputs, seeded; true when the penalty reaches 0.
fn anneal(win: &mut Win<'_>, iters: usize, seed: u64) -> bool {
    let mut rnd = Rng(seed);
    let (mut cur, _) = win.total();
    if cur == 0.0 {
        return true;
    }
    let pos: Vec<(usize, usize)> = (0..win.f.len())
        .flat_map(|w| (0..28).map(move |k| (w, k)))
        .filter(|&(w, k)| win.f[w].hi[k] > win.f[w].lo[k])
        .collect();
    if pos.is_empty() {
        return false;
    }
    let mut t = cur.max(1.0);
    let mut best = (cur, win.f.iter().map(|f| f.x.clone()).collect::<Vec<_>>());
    for _ in 0..iters {
        let (w, k) = pos[rnd.below(pos.len())];
        let (lo, hi, old) = (win.f[w].lo[k], win.f[w].hi[k], win.f[w].x[k]);
        let span = (hi - lo) as f64;
        let v = if rnd.unit() < 0.3 {
            if rnd.below(2) == 0 { lo } else { hi }
        } else {
            (old + rnd.gauss((span / 6.0).max(1.0)) as i64).clamp(lo, hi)
        };
        if v == old {
            continue;
        }
        win.f[w].x[k] = v;
        let (new, frames) = win.trial(&[(w, k)]);
        if new <= cur || rnd.unit() < ((cur - new) / t).exp() {
            cur = new;
            win.commit(&frames);
            if cur < best.0 {
                best = (cur, win.f.iter().map(|f| f.x.clone()).collect());
            }
            if cur == 0.0 {
                return true;
            }
        } else {
            win.f[w].x[k] = old;
        }
        t = (t * 0.9995).max(1e-3);
    }
    for (f, x) in win.f.iter_mut().zip(best.1) {
        f.x = x;
    }
    best.0 == 0.0
}

// The exact solve: convex polygons of (x[k-1], x[k]).

type Point = (f64, f64);
type Row3 = (f64, f64, f64, f64);

fn hull(pts: &[Point]) -> Vec<Point> {
    let r9 = |v: f64| (v * 1e9).round() / 1e9;
    let mut pts: Vec<Point> = pts.iter().map(|&(x, y)| (r9(x), r9(y))).collect();
    pts.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    pts.dedup();
    if pts.len() <= 2 {
        return pts;
    }
    let cross = |o: Point, a: Point, b: Point| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let (mut lower, mut upper): (Vec<Point>, Vec<Point>) = (vec![], vec![]);
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn halfplanes(poly: &[Point]) -> Vec<(f64, f64, f64)> {
    match poly {
        [(w, u)] => vec![(1.0, 0.0, *w), (-1.0, 0.0, -w), (0.0, 1.0, *u), (0.0, -1.0, -u)],
        [(w1, u1), (w2, u2)] => {
            let (dw, du) = (w2 - w1, u2 - u1);
            vec![
                (du, -dw, du * w1 - dw * u1),
                (-du, dw, -(du * w1 - dw * u1)),
                (dw, du, dw * w2 + du * u2),
                (-dw, -du, -(dw * w1 + du * u1)),
            ]
        }
        _ => (0..poly.len())
            .map(|i| {
                let ((w1, u1), (w2, u2)) = (poly[i], poly[(i + 1) % poly.len()]);
                let (a, b) = (u2 - u1, -(w2 - w1));
                (a, b, a * w1 + b * u1)
            })
            .collect(),
    }
}

fn det3(m: [[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// The polygon of (x[k-1], x[k]) from that of (x[k-2], x[k-1]): the input's interval and the residual slab, the earlier
/// input projected out; `extra` are more `a w + b u + c v <= d` rows.
fn poly_step(poly: &[Point], lo: f64, hi: f64, f0: f64, f1: f64, t: f64, extra: &[Row3]) -> Vec<Point> {
    let mut planes: Vec<Row3> = halfplanes(poly).into_iter().map(|(a, b, c)| (a, b, 0.0, c)).collect();
    planes.extend([(0.0, 0.0, 1.0, hi), (0.0, 0.0, -1.0, -lo), (f1, f0, 1.0, t), (-f1, -f0, -1.0, t)]);
    planes.extend_from_slice(extra);
    let m = planes.len();
    let mut pts = Vec::new();
    for i in 0..m {
        for j in i + 1..m {
            for k in j + 1..m {
                let rows = [planes[i], planes[j], planes[k]];
                let a = rows.map(|r| [r.0, r.1, r.2]);
                let bvec = rows.map(|r| r.3);
                let det = det3(a);
                if det.abs() < 1e-12 {
                    continue;
                }
                let col = |c: usize| {
                    let mut mm = a;
                    for r in 0..3 {
                        mm[r][c] = bvec[r];
                    }
                    det3(mm) / det
                };
                let (w, u, v) = (col(0), col(1), col(2));
                if planes.iter().all(|p| p.0 * w + p.1 * u + p.2 * v <= p.3 + 1e-6) {
                    pts.push((u, v));
                }
            }
        }
    }
    hull(&pts)
}

fn slice_w(poly: &[Point], u: f64) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let n = poly.len();
    for i in 0..n {
        let (w1, u1) = poly[i];
        let (w2, u2) = if n > 1 { poly[(i + 1) % n] } else { poly[i] };
        if (u1 - u).abs() < 1e-9 {
            lo = lo.min(w1);
            hi = hi.max(w1);
        }
        if (u1 - u) * (u2 - u) < 0.0 {
            let t = (u - u1) / (u2 - u1);
            let w = w1 + t * (w2 - w1);
            lo = lo.min(w);
            hi = hi.max(w);
        }
    }
    (lo, hi)
}

/// A step's record for reading the solution back: the polygon before it, the filter, the cap and the extra rows.
type Step = (Vec<Point>, f64, f64, f64, Vec<Row3>);

/// A place of the depth-first solve: the position, the polygon there, the last two feedback values, the steps so far.
type Node = (usize, Vec<Point>, f64, f64, Vec<Step>);

/// Continuous inputs for `frames` keeping every stored-filter residual within the shift's main cap less `margin`,
/// saturation branched depth-first; or none.
fn poly_window(frames: &[Frame], st: &State, witnesses: &BTreeMap<usize, Vec<Row3>>, margin: f64) -> Option<Vec<f64>> {
    let max_nodes = 20000;
    let mut seq = Vec::new();
    for fr in frames {
        let filt = usize::from(fr[0] >> 4).min(4);
        let shift = u32::from(fr[0] & 15);
        let t = run_list(shift).first().map_or(0, |r| r.1) as f64 + 1.0 - margin;
        for k in 0..28 {
            seq.push((fr, filt, shift, k, t));
        }
    }
    let mut stack: Vec<Node> = vec![(0, vec![(st.s2, st.s1)], st.s1 + st.e1, st.s2 + st.e2, vec![])];
    let mut nodes = 0;
    while let Some((pos, poly, y1, y2, hist)) = stack.pop() {
        nodes += 1;
        if nodes > max_nodes {
            return None;
        }
        if pos == seq.len() {
            return Some(poly_read_back(&hist, &poly));
        }
        let (fr, filt, shift, k, t) = seq[pos];
        let (f0, f1) = F[filt];
        let c = nib(fr, k);
        let scale = f64::from(1_u32 << shift);
        let p = f0 * y1 + f1 * y2;
        let branches: Vec<(f64, f64, i64)> = if c == 7 {
            vec![
                (26624.0 / scale - p + 0.5 / scale, 30720.0 / scale - p - 1.5 / scale, 28672 >> shift),
                (30720.0 / scale - p + 0.5 / scale, HI as f64, 32767 >> shift),
            ]
        } else if c == -8 {
            vec![(LO as f64, -30720.0 / scale - p - 1.5 / scale, -32768 >> shift)]
        } else {
            vec![(
                (c * 4096 - 2048) as f64 / scale - p + 1.5 / scale,
                (c * 4096 + 2048) as f64 / scale - p - 1.5 / scale,
                (c * 4096) >> shift,
            )]
        };
        let extra = witnesses.get(&pos).map_or(&[][..], Vec::as_slice);
        for (lo, hi, q) in branches {
            let (lo2, hi2) = (lo.max(LO as f64) + 0.5, hi.min(HI as f64) - 0.5);
            if lo2 > hi2 {
                continue;
            }
            let npoly = poly_step(&poly, lo2, hi2, f0, f1, t, extra);
            if !npoly.is_empty() {
                let mut h = hist.clone();
                h.push((poly.clone(), f0, f1, t, extra.to_vec()));
                stack.push((pos + 1, npoly, q as f64 - p, y1, h));
            }
        }
    }
    None
}

fn poly_read_back(hist: &[Step], last: &[Point]) -> Vec<f64> {
    let n = hist.len();
    let mut x = vec![0.0; n];
    let len = last.len() as f64;
    x[n - 1] = last.iter().map(|p| p.1).sum::<f64>() / len;
    if n >= 2 {
        x[n - 2] = last.iter().map(|p| p.0).sum::<f64>() / len;
    }
    for p in (2..n).rev() {
        let (poly, f0, f1, t, extra) = &hist[p];
        let (u, v) = (x[p - 1], x[p]);
        let (mut wl, mut wh) = slice_w(poly, u);
        if *f1 > 0.0 {
            wl = wl.max((-t - v - f0 * u) / f1);
            wh = wh.min((t - v - f0 * u) / f1);
        }
        for &(a, b, c, d) in extra {
            if a > 1e-12 {
                wh = wh.min((d - b * u - c * v) / a);
            } else if a < -1e-12 {
                wl = wl.max((d - b * u - c * v) / a);
            }
        }
        x[p - 2] = if wl <= wh {
            f64::midpoint(wl, wh)
        } else if wl == f64::INFINITY {
            0.0
        } else {
            wl
        };
    }
    x
}

/// Integer inputs reproducing `frames` from `st`, by the polygon solve plus witnesses for each condition a candidate
/// misses; or none.
fn poly_solve(frames: &[Frame], st: &State, w: &[f64; 5]) -> Option<Vec<Vec<i64>>> {
    let (tries, margin) = (24, 2.5);
    let mut witnesses: BTreeMap<usize, Vec<Row3>> = BTreeMap::new();
    for _ in 0..tries {
        let got = poly_window(frames, st, &witnesses, margin)?;
        let x: Vec<i64> = got.iter().map(|v| v.round_ties_even() as i64).collect();
        let mut t = *st;
        let mut bad = None;
        for (i, fr) in frames.iter().enumerate() {
            let xi = &x[28 * i..28 * i + 28];
            let before = t;
            if encode_frame(&mut t, xi, fr[1], w) != *fr {
                bad = Some((i, before, xi.to_vec()));
                break;
            }
        }
        let Some((i, before, xi)) = bad else {
            return Some((0..frames.len()).map(|i| x[28 * i..28 * i + 28].to_vec()).collect());
        };
        let fr = &frames[i];
        let filt = usize::from(fr[0] >> 4).min(4);
        let shift = u32::from(fr[0] & 15);
        let r = residuals(before.s1, before.s2, &xi);
        let pk: [f64; 5] = std::array::from_fn(|j| peak(&r[j]));
        let t_cap = run_list(shift).first().map_or(0, |r| r.1) as f64 + 1.0 - margin;
        let floor = run_list(shift).first().map_or(0, |r| r.0) as f64;
        let mut targets = Vec::new();
        if pk[filt] < floor {
            targets.push((filt, floor + 0.5));
        }
        for j in 0..5 {
            if j != filt && w[j] * pk[j] < w[filt] * pk[filt] + if j < filt { 1e-6 } else { 0.0 } {
                targets.push((j, t_cap * w[filt] / w[j] + 0.5));
            }
        }
        if filt != 0 && pk[0] <= 7.0 {
            targets.push((0, 7.5));
        }
        let mut added = false;
        for (j, thr) in targets {
            let mut k = 0;
            for kk in 1..28 {
                if r[j][kk].abs() > r[j][k].abs() {
                    k = kk;
                }
            }
            let sgn = if r[j][k] >= 0.0 { 1.0 } else { -1.0 };
            let row = (-sgn * F[j].1, -sgn * F[j].0, -sgn, -thr);
            let rows = witnesses.entry(28 * i + k).or_default();
            if !rows.contains(&row) {
                rows.push(row);
                added = true;
            }
        }
        if !added {
            return None;
        }
    }
    None
}

// The search.

/// A frame's input as found: the input, the state after, the earlier frames it changed, the saturation choices.
type Found = (Vec<i64>, State, BTreeMap<usize, (Vec<i64>, State)>, BTreeSet<usize>);

fn flag_sets(two: &[usize]) -> Vec<BTreeSet<usize>> {
    let mut out = vec![BTreeSet::new()];
    if !two.is_empty() {
        let all: BTreeSet<usize> = two.iter().copied().collect();
        out.push(all.clone());
        if two.len() > 1 {
            for &p in two {
                out.push(std::iter::once(p).collect());
            }
            for &p in two {
                let mut less = all.clone();
                less.remove(&p);
                out.push(less);
            }
        }
    }
    out
}

/// The stages a frame's candidates come from, each worked out when reached, in order of cost.
#[derive(Clone, Copy, Debug)]
enum Stage {
    Hint,
    Defaults,
    DefaultsRepaired,
    Polygon(usize),
    Flags(usize),
    /// The windows over earlier frames and the annealing, laid out once the flag sets they take are known.
    Later,
    Window(usize, usize),
    Anneal(usize, u64),
}

/// A frame's candidates, worked out stage by stage as they are asked for.
struct Candidates {
    k: usize,
    stages: VecDeque<Stage>,
    queue: VecDeque<Found>,
    seen: HashSet<Vec<i64>>,
    first: Result<Bounds, Vec<usize>>,
    flag_list: Vec<BTreeSet<usize>>,
    tried: Vec<(BTreeSet<usize>, Bounds)>,
}

struct Search<'a> {
    frames: &'a [Frame],
    w: &'a [f64; 5],
    hint: Option<&'a [Vec<i64>]>,
    xs: Vec<Vec<i64>>,
    after: Vec<State>,
    flagsets: Vec<BTreeSet<usize>>,
}

impl Search<'_> {
    fn before(&self, j: usize) -> State {
        if j == 0 { State::default() } else { self.after[j - 1] }
    }

    fn candidates(&self, k: usize, thorough: bool, cheap: bool) -> Candidates {
        let st = self.before(k);
        let first = frame_bounds(&self.frames[k], &st, &BTreeSet::new());
        let two = match &first {
            Ok(b) => b.two.clone(),
            Err(two) => two.clone(),
        };
        let flag_list = flag_sets(&two);
        let mut stages: VecDeque<Stage> = [Stage::Hint, Stage::Defaults, Stage::DefaultsRepaired].into();
        let polygons = (0..=2).filter(|&back| k >= back).map(Stage::Polygon);
        if !thorough {
            stages.extend(polygons.clone());
        }
        if !cheap {
            stages.extend((0..flag_list.len()).map(Stage::Flags));
            if thorough {
                stages.extend(polygons);
            }
            stages.push_back(Stage::Later);
        }
        Candidates { k, stages, queue: VecDeque::new(), seen: HashSet::new(), first, flag_list, tried: Vec::new() }
    }

    /// Whether x reproduces frame k from `st`: the state after.
    fn reproduces(&self, k: usize, st: &State, x: &[i64]) -> Option<State> {
        let mut t = *st;
        (encode_frame(&mut t, x, self.frames[k][1], self.w) == self.frames[k]).then_some(t)
    }

    fn next(&self, c: &mut Candidates) -> Option<Found> {
        loop {
            if let Some(found) = c.queue.pop_front() {
                return Some(found);
            }
            let stage = c.stages.pop_front()?;
            self.work(c, stage);
        }
    }

    /// An input of frame k as a candidate, where it reproduces the frame and was not tried.
    fn offer(&self, c: &mut Candidates, st: &State, x: Vec<i64>, flags: &BTreeSet<usize>) {
        if c.seen.contains(&x) {
            return;
        }
        if let Some(t) = self.reproduces(c.k, st, &x) {
            c.seen.insert(x.clone());
            c.queue.push_back((x, t, BTreeMap::new(), flags.clone()));
        }
    }

    /// Inputs of frames `k - back ..= k` from `base`, where they reproduce them: the last, the state after it and the
    /// earlier ones changed.
    fn joined(&self, k: usize, back: usize, base: State, xs: &[Vec<i64>]) -> Option<Found> {
        let mut t = base;
        let mut changed = BTreeMap::new();
        for (i, xi) in xs.iter().enumerate() {
            let j = k - back + i;
            if encode_frame(&mut t, xi, self.frames[j][1], self.w) != self.frames[j] {
                return None;
            }
            if j < k {
                changed.insert(j, (xi.clone(), t));
            }
        }
        Some((xs.last()?.clone(), t, changed, BTreeSet::new()))
    }

    #[expect(clippy::too_many_lines, reason = "the stages, in order")]
    fn work(&self, c: &mut Candidates, stage: Stage) {
        let k = c.k;
        let fr = self.frames[k];
        let st = self.before(k);
        let none = BTreeSet::new();
        match stage {
            Stage::Hint => {
                if let Some(hint) = self.hint {
                    c.seen.insert(hint[k].clone());
                    if let Some(t) = self.reproduces(k, &st, &hint[k]) {
                        c.queue.push_back((hint[k].clone(), t, BTreeMap::new(), BTreeSet::new()));
                    }
                }
            }
            Stage::Defaults => {
                if let Ok(first) = c.first.clone() {
                    for x0 in [first.xs.clone(), tight(&fr, &st, &first)] {
                        if c.seen.insert(x0.clone())
                            && let Some(t) = self.reproduces(k, &st, &x0)
                        {
                            c.queue.push_back((x0, t, BTreeMap::new(), BTreeSet::new()));
                        }
                    }
                }
            }
            Stage::DefaultsRepaired => {
                if let Ok(first) = c.first.clone() {
                    for x0 in [first.xs.clone(), tight(&fr, &st, &first)] {
                        let mut win = self.window(st, vec![frame_of(&fr, &first, x0)]);
                        if repair(&mut win, 80) {
                            self.offer(c, &st, win.f[0].x.clone(), &none);
                        }
                    }
                }
            }
            Stage::Polygon(back) => {
                let base = self.before(k - back);
                if let Some(sol) = poly_solve(&self.frames[k - back..=k], &base, self.w)
                    && let Some(found) = self.joined(k, back, base, &sol)
                    && c.seen.insert(found.0.clone())
                {
                    c.queue.push_back(found);
                }
            }
            Stage::Flags(i) => {
                let flags = c.flag_list[i].clone();
                let got = if flags.is_empty() { c.first.clone() } else { frame_bounds(&fr, &st, &flags) };
                if let Ok(got) = got {
                    c.tried.push((flags.clone(), got.clone()));
                    if !flags.is_empty() {
                        let starts = [got.xs.clone(), tight(&fr, &st, &got)];
                        for x0 in &starts {
                            if c.seen.insert(x0.clone())
                                && let Some(t) = self.reproduces(k, &st, x0)
                            {
                                c.queue.push_back((x0.clone(), t, BTreeMap::new(), flags.clone()));
                            }
                        }
                        for x0 in starts {
                            let mut win = self.window(st, vec![frame_of(&fr, &got, x0)]);
                            if repair(&mut win, 80) {
                                self.offer(c, &st, win.f[0].x.clone(), &flags);
                            }
                        }
                    }
                }
            }
            Stage::Later => {
                for back in (1..=2).filter(|&back| k >= back) {
                    c.stages.extend((0..c.tried.len()).map(|fi| Stage::Window(back, fi)));
                }
                for fi in 0..c.tried.len() {
                    c.stages.extend([Stage::Anneal(fi, 1), Stage::Anneal(fi, 2)]);
                }
            }
            Stage::Window(back, fi) => {
                let (flags, got) = c.tried[fi].clone();
                let base = self.before(k - back);
                let mut wf = Vec::new();
                let mut s = base;
                for j in k - back..k {
                    let Ok(g) = frame_bounds(&self.frames[j], &s, &self.flagsets[j]) else { return };
                    wf.push(frame_of(&self.frames[j], &g, self.xs[j].clone()));
                    encode_frame(&mut s, &self.xs[j], self.frames[j][1], self.w);
                }
                wf.push(frame_of(&fr, &got, got.xs.clone()));
                // Repaired, else annealed: the first that holds.
                for annealed in [false, true] {
                    let mut win = self.window(base, wf.clone());
                    let solved = if annealed { anneal(&mut win, 8000, back as u64) } else { repair(&mut win, 80) };
                    if !solved {
                        continue;
                    }
                    let xs: Vec<Vec<i64>> = win.f.iter().map(|f| f.x.clone()).collect();
                    if let Some((last, t, changed, _)) = self.joined(k, back, base, &xs) {
                        c.queue.push_back((last, t, changed, flags));
                        break;
                    }
                }
            }
            Stage::Anneal(fi, seed) => {
                let (flags, got) = c.tried[fi].clone();
                let mut win = self.window(st, vec![frame_of(&fr, &got, got.xs.clone())]);
                if anneal(&mut win, 6000, seed) {
                    self.offer(c, &st, win.f[0].x.clone(), &flags);
                }
            }
        }
    }

    fn window(&self, st: State, f: Vec<WinFrame>) -> Win<'_> {
        Win { s1: st.s1, s2: st.s2, f, w: self.w, vs: Vec::new() }
    }

    fn take(&mut self, k: usize, found: Found) {
        let (x, t, changed, flags) = found;
        self.xs[k] = x;
        self.after[k] = t;
        self.flagsets[k] = flags;
        for (j, (xj, aj)) in changed {
            self.xs[j] = xj;
            self.after[j] = aj;
        }
    }
}

fn frame_of(fr: &Frame, b: &Bounds, x: Vec<i64>) -> WinFrame {
    WinFrame { hdr: fr[0], lo: b.lo.clone(), hi: b.hi.clone(), cen: b.cen.clone(), x }
}

/// (inputs per frame, unsolved frame indices), by depth-first search over each frame's candidates; with
/// `first_failure`, returning at the first unsolved frame.
#[expect(clippy::too_many_arguments, reason = "the search's settings, as the decoder runs it")]
fn solve_frames(
    frames: &[Frame],
    w: &[f64; 5],
    thorough: bool,
    depth: usize,
    budget: usize,
    hint: Option<&[Vec<i64>]>,
    first_failure: bool,
    cheap: bool,
) -> (Vec<Vec<i64>>, Vec<usize>) {
    let n = frames.len();
    let mut search = Search {
        frames,
        w,
        hint,
        xs: vec![vec![0; 28]; n],
        after: vec![State::default(); n],
        flagsets: vec![BTreeSet::new(); n],
    };
    let mut gens: Vec<Option<Candidates>> = (0..n).map(|_| None).collect();
    let mut failures = Vec::new();
    let (mut k, mut frontier, mut low, mut steps) = (0, 0, 0, 0);
    while k < n {
        if gens[k].is_none() {
            gens[k] = Some(search.candidates(k, thorough, cheap));
        }
        let mut c = gens[k].take().expect("made");
        let got = search.next(&mut c);
        gens[k] = Some(c);
        steps += 1;
        if let Some(found) = got {
            search.take(k, found);
            k += 1;
            if k > frontier {
                frontier = k;
                steps = 0;
            }
            continue;
        }
        gens[k] = None;
        if k > low && frontier - (k - 1) <= depth && steps < budget {
            k -= 1;
            continue;
        }
        // Give up on the frontier frame: first candidates up to it, the default there.
        let mut j = frontier.min(n - 1);
        for (jj, slot) in gens.iter_mut().enumerate().take(frontier.min(n)).skip(k) {
            let mut c = search.candidates(jj, thorough, cheap);
            let got = search.next(&mut c);
            *slot = Some(c);
            let Some(found) = got else {
                j = jj;
                break;
            };
            search.take(jj, found);
        }
        let before = search.before(j);
        let x = frame_bounds(&frames[j], &before, &BTreeSet::new()).map_or_else(|_| vec![0; 28], |g| g.xs);
        let mut t = before;
        encode_frame(&mut t, &x, frames[j][1], w);
        search.xs[j] = x;
        search.after[j] = t;
        search.flagsets[j] = BTreeSet::new();
        failures.push(j);
        if first_failure {
            break;
        }
        k = j + 1;
        frontier = frontier.max(k);
        low = k;
        steps = 0;
        for g in gens.iter_mut().take(low) {
            *g = None;
        }
    }
    (search.xs, failures)
}

/// The most restrictive mode whose filters include every filter the body's frames use.
#[must_use]
pub fn mode_of(body: &[u8]) -> &'static str {
    let used: BTreeSet<usize> = body_frames(body)
        .iter()
        .filter(|fr| fr[2..].iter().any(|&b| b != 0))
        .map(|fr| usize::from(fr[0] >> 4))
        .collect();
    ["4bit", "low", "high", "normal"]
        .into_iter()
        .find(|m| used.iter().all(|f| allowed(m).contains(f)))
        .unwrap_or("normal")
}

/// A body as the SPU plays it, once through, without its leading silent frame or its end block: each frame's codes
/// scaled by its shift and fed back through its filter, as the SPU decodes them, before it interpolates.
#[must_use]
pub fn play(body: &[u8]) -> Vec<i16> {
    const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];
    let (mut s1, mut s2) = (0_i32, 0_i32);
    let mut out = Vec::new();
    for frame in body_frames(body) {
        let shift = u32::from(frame[0] & 0xF).min(12);
        let (f0, f1) = FILTERS[usize::from(frame[0] >> 4).min(4)];
        for i in 0..28 {
            let code = (nib(&frame, i) as i32) << 12 >> shift;
            let sample = (code + ((s1 * f0 + s2 * f1 + 32) >> 6)).clamp(i32::from(i16::MIN), i32::from(i16::MAX));
            (s2, s1) = (s1, sample);
            out.push(sample as i16);
        }
        if frame[1] & 1 != 0 {
            break;
        }
    }
    out
}

/// A body's loop start (a frame index), or none for a one-shot.
#[must_use]
pub fn loop_of(body: &[u8]) -> Option<usize> {
    if body.ends_with(&END_BLOCK) {
        return None;
    }
    body.get(16..).unwrap_or_default().as_chunks::<16>().0.iter().position(|fr| fr[1] == 6)
}

/// What a body decodes to: input `encode` turns into it exactly, the mode it needs and its loop start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub pcm: Vec<i64>,
    pub mode: &'static str,
    pub loop_start: Option<usize>,
}

/// Input that `encode` turns into `body` exactly, in normal mode wherever it can be.
///
/// # Errors
/// No input is found for some frames, or the input found does not re-encode to the body.
pub fn decode(body: &[u8]) -> Result<Decoded, String> {
    let mut mode = mode_of(body);
    let frames = body_frames(body);
    let loop_start = loop_of(body);
    let w = weights(mode).ok_or_else(|| format!("no mode {mode}"))?;
    let (mut xs, failures) = match solve_frames(&frames, &w, false, 3, 400, None, false, false) {
        (xs, failures) if failures.is_empty() => (xs, failures),
        _ => solve_frames(&frames, &w, true, 3, 400, None, false, false),
    };
    if !failures.is_empty() {
        return Err(format!("no input reproduces frames {:?} (mode {mode})", &failures[..failures.len().min(8)]));
    }
    let normal = weights("normal").unwrap_or_default();
    if mode != "normal" {
        let (again, missed) = solve_frames(&frames, &normal, false, 0, 400, Some(&xs), true, true);
        if missed.is_empty() {
            xs = again;
            mode = "normal";
        }
    }
    let pcm: Vec<i64> = xs.into_iter().flatten().collect();
    if encode(&pcm, &weights(mode).unwrap_or(normal), loop_start) != body {
        return Err("the input found does not re-encode to the body".into());
    }
    Ok(Decoded { pcm, mode, loop_start })
}

#[cfg(test)]
mod tests {
    #[test]
    fn what_is_encoded_decodes_to_input_encoding_alike() {
        // A tone and a click, one-shot and looping.
        let tone: Vec<i64> = (0..28 * 12).map(|i| ((f64::from(i) * 0.3).sin() * 12000.0) as i64).collect();
        let click: Vec<i64> = (0..28 * 4).map(|i| if i % 13 == 0 { 30000 } else { -200 }).collect();
        for (pcm, loop_start) in [(tone, None), (click, Some(1))] {
            let body = super::encode(&pcm, &super::weights("normal").unwrap(), loop_start);
            let decoded = super::decode(&body).unwrap();
            assert_eq!(decoded.loop_start, loop_start);
            assert_eq!(super::encode(&decoded.pcm, &super::weights(decoded.mode).unwrap(), loop_start), body);
        }
    }
}
