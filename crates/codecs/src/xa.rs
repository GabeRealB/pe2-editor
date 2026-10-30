//! CD-ROM XA ADPCM: the `PlayStation`'s streamed audio, PE2's movie soundtracks. An audio sector's 2304 bytes are 18
//! sound groups of 128 bytes; a group is 16 header bytes - a filter and a shift for each of 8 units, the units' headers
//! read from bytes 4 to 11 - and 112 bytes of 4-bit samples, unit `i`'s `j`th in byte `16 + i / 2 + j * 4`'s low or
//! high nibble. Each sample is its nibble shifted up by `12 - shift`, plus the filter's prediction from the channel's
//! two previous samples (`(s1 * f0 + s2 * f1 + 32) >> 6`), clamped to 16 bits. Stereo puts the even units on the left,
//! the odd on the right; mono plays them in order. The state runs on from sector to sector of a stream. (After
//! `tools/peassets/xa_codec.py`, which follows `FFmpeg`'s `xa_decode`.)

/// The filters' coefficients, in 1/64.
const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

/// An audio sector's sound data: 18 groups of 128 bytes.
pub const SOUND_DATA: usize = 2304;
const GROUP: usize = 128;

pub use formats::str::Coding;

/// A stream's decoding state: each channel's two previous samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decoder {
    previous: [[i32; 2]; 2],
}

impl Decoder {
    /// One audio sector's samples, interleaved by channel (left, right, left, ... for stereo): 4032 a sector in
    /// stereo, as 2016 a channel, or 4032 in mono. None where it is not 4-bit audio or holds less than its data.
    #[must_use]
    pub fn sector(&mut self, coding: Coding, data: &[u8]) -> Option<Vec<i16>> {
        if !coding.four_bit || data.len() < SOUND_DATA {
            return None;
        }
        let mut out = Vec::with_capacity(SOUND_DATA / GROUP * 224);
        for group in data[..SOUND_DATA].as_chunks::<GROUP>().0 {
            self.group(group, coding.stereo, &mut out);
        }
        Some(out)
    }

    fn group(&mut self, group: &[u8], stereo: bool, out: &mut Vec<i16>) {
        for i in 0..4 {
            let left = Self::unit(group, i, false, &mut self.previous[0]);
            // Stereo's odd units have the right channel's state; mono's go on from the even ones'.
            let right = Self::unit(group, i, true, &mut self.previous[usize::from(stereo)]);
            if stereo {
                out.extend(left.iter().zip(&right).flat_map(|(&l, &r)| [l, r]));
            } else {
                out.extend(left);
                out.extend(right);
            }
        }
    }

    /// One unit's 28 samples: the low nibbles of its bytes (the even unit) or the high (the odd).
    fn unit(group: &[u8], i: usize, high: bool, state: &mut [i32; 2]) -> [i16; 28] {
        let header = group[4 + i * 2 + usize::from(high)];
        let mut shift = 12 - i32::from(header & 0x0F);
        let mut filter = usize::from(header >> 4);
        if filter >= FILTERS.len() || shift < 0 {
            filter = 0;
            shift = shift.max(0);
        }
        let (f0, f1) = FILTERS[filter];
        let [mut s1, mut s2] = *state;
        let mut out = [0i16; 28];
        for (j, sample) in out.iter_mut().enumerate() {
            let byte = group[16 + i + j * 4];
            let nibble = if high { byte >> 4 } else { byte & 0x0F };
            let t = i32::from(nibble) - if nibble >= 8 { 16 } else { 0 };
            let s = (t * (1 << shift) + ((s1 * f0 + s2 * f1 + 32) >> 6)).clamp(-32768, 32767);
            s2 = s1;
            s1 = s;
            *sample = i16::try_from(s).unwrap_or_default();
        }
        *state = [s1, s2];
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{Coding, Decoder, SOUND_DATA};

    #[test]
    fn codings_and_silence() {
        assert_eq!(Coding::of(0x01), Coding { stereo: true, rate: 37800, four_bit: true });
        assert_eq!(Coding::of(0x04), Coding { stereo: false, rate: 18900, four_bit: true });
        // Zero nibbles under filter 0: silence, 4032 samples a sector.
        let pcm = Decoder::default().sector(Coding::of(0x01), &[0; SOUND_DATA]).unwrap();
        assert_eq!((pcm.len(), pcm.iter().all(|&s| s == 0)), (4032, true));
    }

    #[test]
    fn a_nibble_shifted_and_predicted() {
        // Group 0, unit 0: filter 1 (60/64), shift 12 - 8 = 4; its first sample's nibble 1 -> 16, then 0 -> 15.
        let mut data = vec![0u8; SOUND_DATA];
        data[4] = 0x18;
        data[16] = 0x01;
        let pcm = Decoder::default().sector(Coding::of(0x01), &data).unwrap();
        assert_eq!(&pcm[..4], &[16, 0, 15, 0]);
    }
}
