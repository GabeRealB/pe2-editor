//! A sample played as the SPU decodes it, raw - at the rate its WAV is written at, once through, without the
//! instrument, script or bank that play it in the game: a stream of sound only, a sixtieth of a second an item, as the
//! player takes it.

use pe2_ext::media::{AudioData, MediaBuffer, SampleFormat};
use pe2_ext::{Call, Generator, Result, Table, Value};

/// Items a second.
const RATE: u32 = 60;

pub struct Played {
    pcm: Vec<i16>,
    rate: u32,
    at: usize,
}

impl Played {
    #[must_use]
    pub fn new(body: &[u8], rate: u32) -> Played {
        Played { pcm: formats::vag::play(body), rate: rate.max(RATE), at: 0 }
    }

    fn step(&self) -> usize {
        (self.rate / RATE) as usize
    }
}

impl Generator for Played {
    fn header(&self) -> Table {
        let mut header = Table::new();
        header.insert("rate".into(), Value::Float(f64::from(RATE)));
        header.insert("count".into(), Value::Integer(i64::try_from(self.pcm.len().div_ceil(self.step())).unwrap_or(0)));
        header.insert("holds".into(), Value::String("a sixtieth of a second of the sample".into()));
        header
    }

    fn next(&mut self, _: &Call<'_>, count: usize) -> Result<Vec<pe2_ext::Data>> {
        let mut items = Vec::new();
        while items.len() < count && self.at < self.pcm.len() {
            let end = (self.at + self.step()).min(self.pcm.len());
            let bytes = self.pcm[self.at..end].iter().flat_map(|s| s.to_le_bytes()).collect();
            let timestamp = u64::try_from(self.at).unwrap_or(0) * 1_000_000 / u64::from(self.rate);
            items.push(pe2_ext::Data::Audio(AudioData {
                buffer: MediaBuffer::new(bytes),
                format: SampleFormat::S16Interleaved,
                sample_rate: self.rate,
                channels: 1,
                timestamp,
            }));
            self.at = end;
        }
        Ok(items)
    }

    fn seek(&mut self, _: &Call<'_>, position: u64) -> Result<bool> {
        self.at = usize::try_from(position).unwrap_or(usize::MAX).saturating_mul(self.step()).min(self.pcm.len());
        Ok(true)
    }
}
