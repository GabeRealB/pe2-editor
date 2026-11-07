//! A clip: what moves a skeleton's joints, one track per joint, as the game stores it - an `AnimationSet` with its pose
//! banks, records and track starts - and as it is authored, each track a list of keys.
//!
//! A key is the pose a joint blends into over its `frames`: Euler angles in 4096 units per turn, and on a track stored
//! with translation (encoding 1, six halfwords) its translation; the others store the rotation alone in 8-unit steps
//! (encoding 4, packed 11/10/11 bits). `cue` is the record's cue bits (1-3), `loop` the key a finished track jumps
//! back to (none: the track ends).
//!
//! Everything else is derived, as every clip on the disc has it:
//! - a loop record `(key's record, 129, 0x80)` before the end, an end `(0, 128, 0xC0)`;
//! - records in track order, track starts from them, a zero halfword after an odd number of starts;
//! - the pose banks, encoding 1 before 4, poses in the order keys first use them, keys with equal poses sharing one
//!   entry. The original exporter merged poses before quantizing them, so equal stored poses are sometimes separate
//!   entries: a key's `variant` keeps them apart, and keys share an entry where value and variant agree. Decoding
//!   numbers the entries of one value in their order of storage, so an edited key always has a valid variant.

use std::collections::HashMap;

use pe2_ext::{Result, anyhow, bail};

/// The `AnimationSet` record: two pointers and eight pose banks, the clip's last bytes.
pub const SET_SIZE: usize = 0x28;
/// Pose encodings: six halfwords, or a packed rotation.
pub const TRANSLATION: u8 = 1;
pub const ROTATION: u8 = 4;
/// Encoding 4 stores angles in steps of this many units.
pub const ANGLE_STEP: i32 = 8;
/// One turn, in the game's angle units.
pub const TURN: f64 = 4096.0;
const LOOP: (u8, u8) = (129, 0x80);
const END: (u16, u8, u8) = (0, 128, 0xC0);
const CUE_SHIFT: u8 = 4;
const CONTROL: u8 = 0x80;
const STOP: u8 = 0x40;
const BITS: [u32; 3] = [11, 10, 11];

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Key {
    pub rotation: [i32; 3],
    /// On a track stored with translation.
    pub translation: Option<[i32; 3]>,
    pub frames: u8,
    pub cue: u8,
    pub variant: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub keys: Vec<Key>,
    /// The key a finished track jumps back to; none, it ends.
    pub looped: Option<usize>,
}

impl Track {
    /// Whether it is stored with translation: its first key says, and every key has to agree.
    #[must_use]
    pub fn translated(&self) -> bool {
        self.keys.first().is_some_and(|key| key.translation.is_some())
    }

    /// Its length in frames: every key's, the last held after it.
    #[must_use]
    pub fn length(&self) -> u32 {
        self.keys.iter().map(|key| u32::from(key.frames)).sum()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Clip {
    pub tracks: Vec<Track>,
}

impl Clip {
    /// Its length in frames: its longest track's.
    #[must_use]
    pub fn length(&self) -> u32 {
        self.tracks.iter().map(Track::length).max().unwrap_or(0)
    }
}

/// A record: a pose's word offset in its bank (or a control's target), frames, flags.
pub type Record = (u16, u8, u8);

/// What a clip is stored as, before it is placed: the arrays its `AnimationSet` points at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Arrays {
    /// Encoding 1: translation then rotation, six halfwords each.
    pub translations: Vec<[i16; 6]>,
    /// Encoding 4: each angle in 8-unit steps.
    pub rotations: Vec<[i32; 3]>,
    pub records: Vec<Record>,
    /// One per track, then a zero after an odd number of them.
    pub starts: Vec<u16>,
}

fn signed(value: u32, bits: u32) -> i32 {
    let mask = (1u32 << bits) - 1;
    let value = value & mask;
    #[expect(clippy::cast_possible_wrap, reason = "within the field's bits")]
    if value & (1 << (bits - 1)) == 0 { value as i32 } else { value as i32 - (1 << bits) }
}

fn unpack(word: u32) -> [i32; 3] {
    [signed(word, 11) * ANGLE_STEP, signed(word >> 11, 10) * ANGLE_STEP, signed(word >> 21, 11) * ANGLE_STEP]
}

/// A rotation in encoding 4's steps.
///
/// # Errors
/// An angle is not a whole number of steps, or does not fit its bits.
pub fn steps(rotation: [i32; 3]) -> Result<[i32; 3]> {
    let mut out = [0; 3];
    for ((angle, bits), step) in rotation.into_iter().zip(BITS).zip(&mut out) {
        let half = 1i32 << (bits - 1);
        if angle % ANGLE_STEP != 0 || !(-half..half).contains(&angle.div_euclid(ANGLE_STEP)) {
            bail!("rotation {rotation:?} does not pack into encoding 4 (8-unit steps, 11/10/11 bits)");
        }
        *step = angle.div_euclid(ANGLE_STEP);
    }
    Ok(out)
}

/// Steps packed into a word, as the game reads them.
#[must_use]
pub fn pack(steps: [i32; 3]) -> u32 {
    #[expect(clippy::cast_sign_loss, reason = "the bits of each field")]
    let field = |value: i32, bits: u32| (value as u32) & ((1 << bits) - 1);
    field(steps[0], 11) | field(steps[1], 10) << 11 | field(steps[2], 11) << 21
}

fn halfword(value: i32, what: &str) -> Result<i16> {
    i16::try_from(value).map_err(|_| anyhow!("{what} {value} is past a halfword"))
}

/// The arrays a clip is stored as.
///
/// # Errors
/// A track mixes keys with and without translation, a value does not fit how it is stored, or a loop jumps past its
/// track.
pub fn arrays(clip: &Clip) -> Result<Arrays> {
    let mut out = Arrays::default();
    let mut entries: HashMap<(u8, [i32; 6], u32), u16> = HashMap::new();
    let mut offsets = Vec::new();
    for (at, track) in clip.tracks.iter().enumerate() {
        let translated = track.translated();
        for key in &track.keys {
            if key.translation.is_some() != translated {
                bail!("track {at}: every key or none carries a translation");
            }
            let (kind, value) = match key.translation {
                Some(t) => (TRANSLATION, [t[0], t[1], t[2], key.rotation[0], key.rotation[1], key.rotation[2]]),
                None => (ROTATION, [0, 0, 0, key.rotation[0], key.rotation[1], key.rotation[2]]),
            };
            let entry = if let Some(entry) = entries.get(&(kind, value, key.variant)) {
                *entry
            } else {
                let entry = if kind == TRANSLATION {
                    let halfwords = value.iter().map(|v| halfword(*v, "a pose's value")).collect::<Result<Vec<_>>>()?;
                    out.translations.push(halfwords.try_into().map_err(|_| anyhow!("six halfwords"))?);
                    u16::try_from((out.translations.len() - 1) * 3)?
                } else {
                    out.rotations.push(steps(key.rotation)?);
                    u16::try_from(out.rotations.len() - 1)?
                };
                entries.insert((kind, value, key.variant), entry);
                entry
            };
            offsets.push((entry, kind));
        }
    }
    let mut keys = offsets.into_iter();
    for (at, track) in clip.tracks.iter().enumerate() {
        let start = u16::try_from(out.records.len())?;
        out.starts.push(start);
        for key in &track.keys {
            let (offset, kind) = keys.next().ok_or_else(|| anyhow!("a key without its pose"))?;
            if key.cue > 3 {
                bail!("track {at}: cue {} is past the two cue bits", key.cue);
            }
            out.records.push((offset, key.frames, kind | key.cue << CUE_SHIFT));
        }
        if let Some(looped) = track.looped {
            if looped >= track.keys.len() {
                bail!("track {at}: it loops to key {looped} of {}", track.keys.len());
            }
            out.records.push((start + u16::try_from(looped)?, LOOP.0, LOOP.1));
        }
        out.records.push(END);
    }
    if out.starts.len() % 2 == 1 {
        out.starts.push(0);
    }
    Ok(out)
}

fn word(bytes: &[u8], at: usize) -> Result<u32> {
    let slice = bytes.get(at..at + 4).ok_or_else(|| anyhow!("a word past its end, at {at:#X}"))?;
    Ok(u32::from_le_bytes(slice.try_into()?))
}

fn half(bytes: &[u8], at: usize) -> Result<u16> {
    let slice = bytes.get(at..at + 2).ok_or_else(|| anyhow!("a halfword past its end, at {at:#X}"))?;
    Ok(u16::from_le_bytes(slice.try_into()?))
}

/// The arrays laid out, the first byte at `address`: banks, records, starts, then the `AnimationSet`.
///
/// # Errors
/// An offset does not fit a word.
pub fn lay_out(arrays: &Arrays, address: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut banks = [0u32; 8];
    if !arrays.translations.is_empty() {
        banks[usize::from(TRANSLATION)] = address + u32::try_from(out.len())?;
        for pose in &arrays.translations {
            for value in pose {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    if !arrays.rotations.is_empty() {
        banks[usize::from(ROTATION)] = address + u32::try_from(out.len())?;
        for rotation in &arrays.rotations {
            out.extend_from_slice(&pack(*rotation).to_le_bytes());
        }
    }
    let records = address + u32::try_from(out.len())?;
    for (offset, frames, flags) in &arrays.records {
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend([*frames, *flags]);
    }
    let starts = address + u32::try_from(out.len())?;
    for start in &arrays.starts {
        out.extend_from_slice(&start.to_le_bytes());
    }
    for pointer in [records, starts].into_iter().chain(banks) {
        out.extend_from_slice(&pointer.to_le_bytes());
    }
    Ok(out)
}

/// A clip's bytes, the first at `address`.
///
/// # Errors
/// What `arrays` refuses.
pub fn encode(clip: &Clip, address: u32) -> Result<Vec<u8>> {
    lay_out(&arrays(clip)?, address)
}

/// Where a clip's arrays are, in its bytes: records, track starts, and each pose bank by encoding.
struct Layout {
    records: Vec<Record>,
    starts: Vec<usize>,
    banks: HashMap<u8, usize>,
}

fn layout(bytes: &[u8], address: u32) -> Result<Layout> {
    let set = bytes.len().checked_sub(SET_SIZE).ok_or_else(|| anyhow!("{} bytes hold no AnimationSet", bytes.len()))?;
    let local = |pointer: u32| -> Result<usize> {
        let at = pointer.checked_sub(address).ok_or_else(|| anyhow!("a pointer {pointer:#X} before the clip"))?;
        let at = usize::try_from(at)?;
        if at > set {
            bail!("a pointer {pointer:#X} past the clip");
        }
        Ok(at)
    };
    let records_at = local(word(bytes, set)?)?;
    let starts_at = local(word(bytes, set + 4)?)?;
    let mut banks: HashMap<u8, usize> = HashMap::new();
    for kind in 0..8u8 {
        let pointer = word(bytes, set + 8 + 4 * usize::from(kind))?;
        if pointer != 0 {
            banks.insert(kind, local(pointer)?);
        }
    }
    if starts_at < records_at {
        bail!("its track starts come before its records");
    }
    let records: Vec<Record> = (records_at..starts_at)
        .step_by(4)
        .map(|at| Ok((half(bytes, at)?, bytes[at + 2], bytes[at + 3])))
        .collect::<Result<_>>()?;
    let mut starts: Vec<usize> =
        (starts_at..set).step_by(2).map(|at| half(bytes, at).map(usize::from)).collect::<Result<_>>()?;
    if starts.len() > 1 && starts.last() == Some(&0) {
        starts.pop();
    }
    Ok(Layout { records, starts, banks })
}

/// A pose of a bank: translation then rotation, in the game's units.
fn pose(bytes: &[u8], banks: &HashMap<u8, usize>, kind: u8, offset: u16) -> Result<[i32; 6]> {
    let base = banks.get(&kind).ok_or_else(|| anyhow!("no bank of encoding {kind}"))? + usize::from(offset) * 4;
    if kind == TRANSLATION {
        let mut value = [0; 6];
        for (at, slot) in value.iter_mut().enumerate() {
            #[expect(clippy::cast_possible_wrap, reason = "a signed halfword")]
            let read = half(bytes, base + at * 2)? as i16;
            *slot = i32::from(read);
        }
        Ok(value)
    } else {
        let r = unpack(word(bytes, base)?);
        Ok([0, 0, 0, r[0], r[1], r[2]])
    }
}

/// Entries of equal value, numbered in their order of storage: each key's variant, by its bank and offset.
fn variants_of(bytes: &[u8], layout: &Layout) -> Result<HashMap<(u8, u16), u32>> {
    let Layout { records, banks, .. } = layout;
    let mut variants: HashMap<(u8, u16), u32> = HashMap::new();
    let mut kinds: Vec<u8> = banks.keys().copied().collect();
    kinds.sort_unstable();
    for kind in kinds {
        let step = if kind == TRANSLATION { 3 } else { 1 };
        let mut used: Vec<u16> = records
            .iter()
            .filter(|(_, _, flags)| flags & CONTROL == 0 && flags & 0xF == kind)
            .map(|(offset, _, _)| *offset)
            .collect();
        used.sort_unstable();
        used.dedup();
        let mut numbered: HashMap<[i32; 6], u32> = HashMap::new();
        for offset in used {
            if offset % step != 0 {
                bail!("a pose at word {offset} is not on a pose boundary");
            }
            let value = pose(bytes, banks, kind, offset)?;
            let next = numbered.entry(value).or_insert(0);
            variants.insert((kind, offset), *next);
            *next += 1;
        }
    }
    Ok(variants)
}

/// The clip whose bytes - first pose bank through its `AnimationSet` - are `bytes`, the first at `address`.
///
/// # Errors
/// They are not one: a pointer outside them, a record neither a key nor a control, a loop or an end out of place.
pub fn decode(bytes: &[u8], address: u32) -> Result<Clip> {
    let layout = layout(bytes, address)?;
    let variants = variants_of(bytes, &layout)?;
    let Layout { records, starts, banks } = &layout;
    let mut bounds = starts.clone();
    bounds.push(records.len());
    let mut tracks = Vec::with_capacity(starts.len());
    for at in 0..starts.len() {
        let (first, last) = (bounds[at], bounds[at + 1]);
        let mut track = Track::default();
        for (index, &(offset, frames, flags)) in records.iter().enumerate().take(last).skip(first) {
            if (offset, frames, flags) == END {
                if index != last - 1 {
                    bail!("track {at}: an end before its last record");
                }
                continue;
            }
            if (frames, flags) == LOOP {
                let target = usize::from(offset);
                if index + 2 != last || !(first..index).contains(&target) {
                    bail!("track {at}: a loop is the record before the end, and jumps back into its track");
                }
                track.looped = Some(target - first);
                continue;
            }
            let kind = flags & 0xF;
            if flags & (CONTROL | STOP) != 0 || !banks.contains_key(&kind) {
                bail!("track {at}: record {:?} is neither a key nor a control", (offset, frames, flags));
            }
            let value = pose(bytes, banks, kind, offset)?;
            track.keys.push(Key {
                rotation: [value[3], value[4], value[5]],
                translation: (kind == TRANSLATION).then_some([value[0], value[1], value[2]]),
                frames,
                cue: flags >> CUE_SHIFT & 3,
                variant: variants[&(kind, offset)],
            });
        }
        tracks.push(track);
    }
    Ok(Clip { tracks })
}

/// Where a track is at `frame`: it reaches key k after the frames of keys 0..=k and holds its last; angles and
/// translations blend linearly between keys, as the game blends them. Rotation in the game's units, and translation
/// where the track has one.
#[must_use]
pub fn sample(track: &Track, frame: f64) -> ([f64; 3], Option<[f64; 3]>) {
    let Some(first) = track.keys.first() else { return ([0.0; 3], None) };
    let float = |v: [i32; 3]| v.map(f64::from);
    let blend = |a: [f64; 3], b: [f64; 3], f: f64| [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * f);
    let (mut rotation, mut translation) = (float(first.rotation), first.translation.map(float));
    let mut start = 0.0;
    for key in &track.keys {
        let frames = f64::from(key.frames);
        if frame <= start + frames {
            let f = if frames > 0.0 { (frame - start) / frames } else { 1.0 };
            let moved = translation.zip(key.translation.map(float)).map(|(a, b)| blend(a, b, f));
            return (blend(rotation, float(key.rotation), f), moved.or(translation));
        }
        start += frames;
        rotation = float(key.rotation);
        translation = key.translation.map(float);
    }
    (rotation, translation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(rotation: [i32; 3], frames: u8) -> Key {
        Key { rotation, frames, ..Key::default() }
    }

    fn clip() -> Clip {
        Clip {
            tracks: vec![
                Track {
                    keys: vec![
                        Key { translation: Some([0, -928, 0]), ..key([0, 0, 0], 1) },
                        Key { translation: Some([5, -900, 2]), cue: 1, ..key([16, -8, 4096 - 8], 12) },
                    ],
                    looped: Some(0),
                },
                Track { keys: vec![key([8, 0, 0], 4), key([8, 0, 0], 4), key([-16, 24, 0], 6)], looped: None },
                Track { keys: vec![key([8, 0, 0], 4), Key { variant: 1, ..key([8, 0, 0], 2) }], looped: None },
            ],
        }
    }

    #[test]
    fn a_clip_comes_back_as_it_went() {
        let clip = clip();
        let bytes = encode(&clip, 0x8011_D8D8).unwrap();
        assert_eq!(decode(&bytes, 0x8011_D8D8).unwrap(), clip);
        assert_eq!(&bytes[bytes.len() - SET_SIZE + 8..bytes.len() - SET_SIZE + 12], &[0, 0, 0, 0]);
    }

    #[test]
    fn equal_poses_share_an_entry_unless_their_variants_differ() {
        let arrays = arrays(&clip()).unwrap();
        assert_eq!(arrays.translations.len(), 2);
        assert_eq!(arrays.rotations, vec![[1, 0, 0], [-2, 3, 0], [1, 0, 0]]);
        assert_eq!(arrays.starts, vec![0, 4, 8, 0]);
        assert_eq!(arrays.records[2], (0, 129, 0x80));
        assert_eq!(arrays.records[3], (0, 128, 0xC0));
    }

    #[test]
    fn a_rotation_off_its_steps_is_refused() {
        let clip = Clip { tracks: vec![Track { keys: vec![key([4, 0, 0], 1)], looped: None }] };
        assert!(arrays(&clip).is_err());
        assert!(steps([0, 512 * 8, 0]).is_err());
        assert_eq!(steps([-1024 * 8, 511 * 8, 1023 * 8]).unwrap(), [-1024, 511, 1023]);
    }

    #[test]
    fn a_track_blends_into_each_key_and_holds_the_last() {
        let track = Track { keys: vec![key([0, 0, 0], 0), key([80, 0, 0], 4), key([0, 0, 0], 4)], looped: None };
        assert_eq!(sample(&track, 2.0).0, [40.0, 0.0, 0.0]);
        assert_eq!(sample(&track, 6.0).0, [40.0, 0.0, 0.0]);
        assert_eq!(sample(&track, 20.0).0, [0.0, 0.0, 0.0]);
    }
}
