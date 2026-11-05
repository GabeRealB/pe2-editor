//! A video file read as a stream (`decode`): each item a frame, with the sound up to it - what `encode` takes, 4:2:0 and
//! full range. What the file holds is asked of ffprobe. Its frames and its sound come from two runs of ffmpeg, a pipe
//! each, so neither waits on the other being read.

use pe2_ext::media::{AudioData, MediaBuffer, PixelFormat, SampleFormat, VideoFrame};
use pe2_ext::process::{Process, spawn};
use pe2_ext::{Call, Data, Generator, Result, Table, Typed, Value, anyhow};

use crate::strings;

/// What ffprobe says of a stream of a file (`v:0`, `a:0`), as its `key=value` lines.
fn probed(path: &str, stream: &str, entries: &str) -> Result<Vec<(String, String)>> {
    let mut process = spawn(
        "ffprobe",
        &strings(&[
            "-v",
            "error",
            "-count_packets",
            "-select_streams",
            stream,
            "-show_entries",
            &format!("stream={entries}"),
            "-of",
            "default=nw=1",
            path,
        ]),
    )?;
    let text = read(&mut process, usize::MAX)?;
    process.wait()?;
    let text = String::from_utf8_lossy(&text);
    Ok(text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect())
}

/// Up to `len` bytes of a program's output: fewer only at its end.
fn read(process: &mut Process, len: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while bytes.len() < len {
        let piece = process.read(u32::try_from(len - bytes.len()).unwrap_or(1 << 20).min(1 << 20))?;
        if piece.is_empty() {
            break;
        }
        bytes.extend(piece);
    }
    Ok(bytes)
}

fn number<T: std::str::FromStr>(entries: &[(String, String)], key: &str) -> Result<T> {
    let value = entries.iter().find(|(k, _)| k == key).ok_or_else(|| anyhow!("ffprobe says no {key}"))?;
    value.1.parse().map_err(|_| anyhow!("ffprobe says {key} is {}", value.1))
}

/// A rate as ffprobe writes it: `30000/1001`.
fn rate(text: &str) -> Option<f64> {
    let (top, bottom) = text.split_once('/')?;
    let (top, bottom): (f64, f64) = (top.parse().ok()?, bottom.parse().ok()?);
    (bottom != 0.0).then(|| top / bottom)
}

/// A file's sound, as it is read.
struct Sound {
    process: Process,
    rate: u32,
    channels: u32,
    /// The samples (frames of its channels) given so far.
    given: u64,
}

/// A video file, as a stream is read.
pub struct Reading {
    header: Table,
    video: Process,
    sound: Option<Sound>,
    width: u32,
    height: u32,
    count: u64,
    rate: f64,
    /// The next frame.
    at: u64,
}

impl Reading {
    /// A file's frames from its first.
    ///
    /// # Errors
    /// It is on no path a program can read, or ffprobe or ffmpeg will not read it.
    pub fn start(call: &Call<'_>, file: &Typed) -> Result<Reading> {
        let path = call.path_of(file.file()?).ok_or_else(|| anyhow!("the file is on no path"))?;
        let video = probed(&path, "v:0", "width,height,avg_frame_rate,nb_read_packets")?;
        let (width, height, count) =
            (number(&video, "width")?, number(&video, "height")?, number(&video, "nb_read_packets")?);
        let frames = video.iter().find(|(k, _)| k == "avg_frame_rate").and_then(|(_, r)| rate(r));
        let frames = frames.ok_or_else(|| anyhow!("ffprobe says no frame rate"))?;
        let sound = probed(&path, "a:0", "sample_rate,channels")?;
        let run = |args: &[&str]| {
            let mut all = strings(&["-hide_banner", "-loglevel", "error", "-nostdin", "-i", &path]);
            all.extend(strings(args));
            spawn("ffmpeg", &all)
        };
        // One frame per frame stored, none dropped or repeated to fit a rate; full range, whatever the file says it is.
        let reading = run(&[
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-vf",
            "scale=out_range=pc",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "rawvideo",
            "pipe:1",
        ])?;
        let heard = if sound.is_empty() {
            None
        } else {
            let (rate, channels) = (number(&sound, "sample_rate")?, number(&sound, "channels")?);
            Some(Sound { process: run(&["-map", "0:a:0", "-f", "s16le", "pipe:1"])?, rate, channels, given: 0 })
        };
        let mut header = Table::new();
        header.insert("count".into(), Value::Integer(count));
        header.insert("rate".into(), Value::Float(frames));
        header.insert("first".into(), Value::Integer(0));
        header.insert("holds".into(), Value::String("a frame's picture, and the sound up to it".into()));
        header.insert("video".into(), Value::String(PixelFormat::Yuv420p.name().into()));
        header.insert("audio".into(), Value::String(SampleFormat::S16Interleaved.name().into()));
        Ok(Reading {
            header,
            video: reading,
            sound: heard,
            width,
            height,
            count: count.try_into()?,
            rate: frames,
            at: 0,
        })
    }

    /// The sound up to the end of the frame just read; to its end, at the last.
    fn heard(&mut self) -> Result<Option<AudioData>> {
        let (count, at, rate) = (self.count, self.at, self.rate);
        let Some(sound) = &mut self.sound else { return Ok(None) };
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "samples of a movie"
        )]
        let upto = ((at + 1) as f64 * f64::from(sound.rate) / rate) as u64;
        let bytes = u64::from(sound.channels) * 2;
        let len = if at + 1 == count { usize::MAX } else { usize::try_from(upto.saturating_sub(sound.given) * bytes)? };
        let samples = read(&mut sound.process, len)?;
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "microseconds of a movie"
        )]
        let timestamp = (sound.given as f64 / f64::from(sound.rate) * 1e6) as u64;
        sound.given += samples.len() as u64 / bytes;
        Ok(Some(AudioData {
            buffer: MediaBuffer::new(samples),
            format: SampleFormat::S16Interleaved,
            sample_rate: sound.rate,
            channels: sound.channels,
            timestamp,
        }))
    }
}

impl Generator for Reading {
    fn header(&self) -> Table {
        self.header.clone()
    }

    fn next(&mut self, _: &Call<'_>, count: usize) -> Result<Vec<Data>> {
        let size = usize::try_from(PixelFormat::Yuv420p.size(self.width, self.height))?;
        let mut items = Vec::new();
        while items.len() < count && self.at < self.count {
            let bytes = read(&mut self.video, size)?;
            if bytes.len() < size {
                self.at = self.count;
                break;
            }
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "microseconds of a movie"
            )]
            let timestamp = (self.at as f64 / self.rate * 1e6) as u64;
            let (width, height) = (self.width, self.height);
            let picture =
                VideoFrame { buffer: MediaBuffer::new(bytes), format: PixelFormat::Yuv420p, width, height, timestamp };
            let mut item = vec![Data::Video(picture)];
            item.extend(self.heard()?.map(Data::Audio));
            items.push(Data::List(item));
            self.at += 1;
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::rate;

    #[test]
    fn a_rate_is_read_as_ffprobe_writes_it() {
        assert_eq!(rate("15/1"), Some(15.0));
        assert_eq!(rate("0/0"), None);
        assert_eq!(rate("15"), None);
    }
}
