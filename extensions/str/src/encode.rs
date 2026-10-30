//! A movie made from a stream (`encode`), with the STR it replaces. What came in is held to what that decodes to: frame by
//! frame, and the sound as one run, as streams need not split it alike. A stream that changed nothing makes the original,
//! its sectors exact and nothing copied. What did change has to be encoded, which this cannot yet do: it says what.

use pe2_ext::media::{AudioData, VideoFrame};
use pe2_ext::{Call, Data, Generator, Result, StreamInput, Typed, bail};

use crate::frames::Frames;

/// An item's picture and sound.
fn parts(item: Data) -> (Option<VideoFrame>, Option<AudioData>) {
    let mut found = (None, None);
    let items = if let Data::List(items) = item { items } else { vec![item] };
    for part in items {
        match part {
            Data::Video(frame) => found.0 = Some(frame),
            Data::Audio(heard) => found.1 = Some(heard),
            _ => {}
        }
    }
    found
}

/// Whether two pictures are the same.
fn same(ours: &VideoFrame, theirs: &VideoFrame) -> bool {
    (ours.format, ours.width, ours.height) == (theirs.format, theirs.width, theirs.height) && ours.buffer.read() == theirs.buffer.read()
}

/// What of a stream differs from a movie.
#[derive(Default)]
struct Differences {
    /// The frames that differ or are not in both, by their places, and how many there are.
    frames: Vec<u64>,
    /// The first byte of the sound that differs.
    sound: Option<u64>,
    /// The sound of each not yet held to the other's, and how much has been.
    ours: Vec<u8>,
    theirs: Vec<u8>,
    held: u64,
}

impl Differences {
    /// Sound of both, held to each other as far as both go.
    fn hear(&mut self, ours: Option<&AudioData>, theirs: Option<&AudioData>) {
        self.ours.extend(ours.map_or_default(|a| a.buffer.read()));
        self.theirs.extend(theirs.map_or_default(|a| a.buffer.read()));
        let both = self.ours.len().min(self.theirs.len());
        if self.sound.is_none() {
            let differs = self.ours[..both].iter().zip(&self.theirs[..both]).position(|(a, b)| a != b);
            self.sound = differs.map(|at| self.held + at as u64);
        }
        self.ours.drain(..both);
        self.theirs.drain(..both);
        self.held += both as u64;
    }

    /// What is left of the sound of either, at the end of both.
    fn end(&mut self) {
        if self.sound.is_none() && self.ours.len() != self.theirs.len() {
            self.sound = Some(self.held);
        }
    }

    fn none(&self) -> bool {
        self.frames.is_empty() && self.sound.is_none()
    }
}

/// A movie of a stream, with the one it replaces.
///
/// # Errors
/// Either could not be read, or the stream changed what cannot yet be encoded.
pub fn encode(call: &Call<'_>, stream: &StreamInput, original: &Typed) -> Result<Typed> {
    let mut reading = Frames::start(call, original.clone())?;
    let mut differences = Differences::default();
    let mut at = 0;
    loop {
        let ours = reading.next(call, 1)?.pop();
        let theirs = stream.next()?;
        if ours.is_none() && theirs.is_none() {
            break;
        }
        let (our_picture, our_sound) = ours.map_or((None, None), parts);
        let (their_picture, their_sound) = theirs.map_or((None, None), parts);
        if !our_picture.as_ref().zip(their_picture.as_ref()).is_some_and(|(a, b)| same(a, b)) {
            differences.frames.push(at);
        }
        differences.hear(our_sound.as_ref(), their_sound.as_ref());
        at += 1;
    }
    differences.end();
    if differences.none() {
        return Ok(original.clone());
    }
    let frames = differences.frames.len();
    let first = differences.frames.first().map_or_else(|| "none".to_string(), ToString::to_string);
    let sound = differences.sound.map_or_else(|| "the same".to_string(), |byte| format!("different from byte {byte}"));
    bail!("{frames} of {at} frames differ, the first {first}, and the sound is {sound}: encoding BS and XA is not built")
}

#[cfg(test)]
mod tests {
    use super::Differences;
    use pe2_ext::media::{AudioData, MediaBuffer, SampleFormat};

    fn sound(bytes: &[u8]) -> AudioData {
        let buffer = MediaBuffer::new(bytes.to_vec());
        AudioData { buffer, format: SampleFormat::S16Interleaved, sample_rate: 37800, channels: 2, timestamp: 0 }
    }

    #[test]
    fn sound_is_held_as_one_run_however_it_is_split() {
        let mut same = Differences::default();
        same.hear(Some(&sound(&[1, 2, 3])), Some(&sound(&[1])));
        same.hear(Some(&sound(&[4])), Some(&sound(&[2, 3, 4])));
        same.end();
        assert!(same.none());
        // Found where it is, in the whole run.
        let mut different = Differences::default();
        different.hear(Some(&sound(&[1, 2])), Some(&sound(&[1, 2])));
        different.hear(Some(&sound(&[3, 4])), Some(&sound(&[3, 9])));
        assert_eq!(different.sound, Some(3));
        // Longer than the other.
        let mut longer = Differences::default();
        longer.hear(Some(&sound(&[1, 2])), Some(&sound(&[1])));
        longer.end();
        assert_eq!(longer.sound, Some(1));
    }
}
