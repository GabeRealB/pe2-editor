//! A sample as a WAV file: 16-bit mono PCM at the rate the game mostly plays it at (its declaration's `rate`), its loop
//! in a standard `smpl` chunk - one forward loop, from the sample it returns to through the last, played forever. On
//! reading, any other chunk is passed over, and a file that is not 16-bit mono PCM refused.

use anyhow::{Context, Result, bail};

/// A sound's samples, and the sample its loop returns to, where it loops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wav {
    pub pcm: Vec<i16>,
    pub loop_start: Option<usize>,
    pub rate: u32,
}

/// A WAV file's bytes.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a sample's sizes are far under 4 GiB; a period of a positive rate"
)]
pub fn write(wav: &Wav) -> Vec<u8> {
    let data: Vec<u8> = wav.pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut chunks = Vec::new();
    chunks.extend(b"fmt ");
    chunks.extend(16_u32.to_le_bytes());
    chunks.extend(1_u16.to_le_bytes());
    chunks.extend(1_u16.to_le_bytes());
    chunks.extend(wav.rate.to_le_bytes());
    chunks.extend((wav.rate * 2).to_le_bytes());
    chunks.extend(2_u16.to_le_bytes());
    chunks.extend(16_u16.to_le_bytes());
    chunks.extend(b"data");
    chunks.extend((data.len() as u32).to_le_bytes());
    chunks.extend(&data);
    if let Some(start) = wav.loop_start {
        let period = (1e9 / f64::from(wav.rate)).round() as u32;
        let words = [0, 0, period, 60, 0, 0, 0, 1, 0, 0, 0, start as u32, wav.pcm.len().saturating_sub(1) as u32, 0, 0];
        chunks.extend(b"smpl");
        chunks.extend((words.len() as u32 * 4).to_le_bytes());
        for w in words {
            chunks.extend(w.to_le_bytes());
        }
    }
    let mut out = b"RIFF".to_vec();
    out.extend((4 + chunks.len() as u32).to_le_bytes());
    out.extend(b"WAVE");
    out.extend(chunks);
    out
}

fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(b.get(at..at + 4).context("the file ends early")?.try_into()?))
}

/// A WAV file's sound.
///
/// # Errors
/// It is no WAV file, or not 16-bit mono PCM.
pub fn read(bytes: &[u8]) -> Result<Wav> {
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        bail!("not a WAV file");
    }
    let (mut at, mut format, mut pcm, mut loop_start) = (12, None, None, None);
    while at + 8 <= bytes.len() {
        let tag = &bytes[at..at + 4];
        let size = u32_at(bytes, at + 4)? as usize;
        let body = bytes.get(at + 8..at + 8 + size).context("a chunk runs past the file's end")?;
        match tag {
            b"fmt " => format = Some(body.to_vec()),
            b"data" => pcm = Some(body.as_chunks::<2>().0.iter().map(|&s| i16::from_le_bytes(s)).collect::<Vec<_>>()),
            b"smpl" if u32_at(body, 28)? > 0 => loop_start = Some(u32_at(body, 36 + 8)? as usize),
            _ => {}
        }
        at += 8 + size + (size & 1);
    }
    let format = format.context("no fmt chunk")?;
    let pcm = pcm.context("no data chunk")?;
    let field = |at: usize| u16::from_le_bytes([format[at], format[at + 1]]);
    let (tag, channels, bits) = (field(0), field(2), field(14));
    if tag != 1 || channels != 1 || bits != 16 {
        bail!("not 16-bit mono PCM (format {tag}, {channels} channels, {bits} bits)");
    }
    Ok(Wav { pcm, loop_start, rate: u32_at(&format, 4)? })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_wav_reads_back_as_written() {
        for loop_start in [None, Some(28)] {
            let wav = super::Wav { pcm: (0..56).map(|i| i * 100 - 2000).collect(), loop_start, rate: 22050 };
            assert_eq!(super::read(&super::write(&wav)).unwrap(), wav);
        }
    }
}
