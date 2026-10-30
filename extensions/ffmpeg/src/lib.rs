//! pe2.ffmpeg, an unsandboxed extension: video files through ffmpeg, offered as the `video-container` service. What uses
//! it gives a stream of frames and sound and gets a file, not knowing what is behind it. ffmpeg is the one it ships
//! where it ships one, else as the system finds it.
//!
//! A file is a container holding a video and an audio encoding: an encoding is written
//! `<container>+<video>[:option=value,...]+<audio>[:option=value,...]`, or `lossless`, the file whose frames and sound
//! come back byte for byte (H.264 at quantizer 0, and FLAC, in an MP4). Frames go in as 4:2:0 and full range, and stay
//! so: a lossless encoder keeps them. ffmpeg takes one pipe, so it runs twice: the frames written, then the sound with
//! the video copied in.
//!
//! What is there is asked of ffmpeg (`capabilities`, `container`, `encoder`), as a build has what it was built with.
//! ffmpeg says what it has, not what goes with what, and listing every pair would take a run each: `check` tries the one
//! chosen, on a frame of its real size, as an encoder may refuse a size.

use pe2_ext::media::{AudioData, PixelFormat, SampleFormat};
use pe2_ext::process::{Process, spawn};
use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{Data, File, Registry, Result, Signature, Table, Typed, Value, anyhow, bail};

mod ask;
mod decode;

pe2_ext::extension!("pe2.ffmpeg", "0.1.0", register, provides = ["video-container@1.0.0"], unsandboxed = true);

/// A video file.
pub struct Video;

impl Registered for Video {
    const NAME: &str = "pe2.ffmpeg@video";
    const LABEL: &str = "Video file";

    fn signatures() -> Vec<Signature> {
        ["*.mp4", "*.m4v", "*.mov", "*.mkv", "*.webm", "*.avi"].into_iter().map(|name| Signature::Name(name.into())).collect()
    }
}

impl Files for Video {}

/// The encoding `lossless` names.
const LOSSLESS: &str = "mp4+libx264:qp=0,preset=medium+flac";

/// An encoder and the options it is given.
struct Encoder {
    name: String,
    options: Vec<(String, String)>,
}

impl Encoder {
    /// An encoder as an encoding writes it: `name[:option=value,...]`.
    fn parse(text: &str) -> Result<Encoder> {
        let (name, options) = text.split_once(':').unwrap_or((text, ""));
        let plain = |word: &str| !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "_-".contains(c)) && !word.starts_with('-');
        if !plain(name) {
            bail!("`{name}` is no encoder");
        }
        let options = options.split(',').filter(|o| !o.is_empty()).map(|option| {
            let (key, value) = option.split_once('=').ok_or_else(|| anyhow!("`{option}` is no option=value"))?;
            if !plain(key) {
                bail!("`{key}` is no option");
            }
            Ok((key.to_string(), value.to_string()))
        });
        Ok(Encoder { name: name.to_string(), options: options.collect::<Result<_>>()? })
    }

    /// Its part of a command line, for a stream (`v`, `a`).
    fn args(&self, stream: &str) -> Vec<String> {
        let mut args = vec![format!("-c:{stream}"), self.name.clone()];
        args.extend(self.options.iter().flat_map(|(key, value)| [format!("-{key}"), value.clone()]));
        args
    }
}

/// What a file is encoded as.
struct Encoding {
    container: String,
    video: Encoder,
    audio: Encoder,
}

impl Encoding {
    fn parse(text: &str) -> Result<Encoding> {
        let text = if text == "lossless" { LOSSLESS } else { text };
        let mut parts = text.split('+');
        let (Some(container), Some(video), Some(audio), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            bail!("`{text}` is no <container>+<video>+<audio>");
        };
        let plain = !container.is_empty() && container.chars().all(|c| c.is_ascii_alphanumeric() || "_-".contains(c)) && !container.starts_with('-');
        if !plain {
            bail!("`{container}` is no container");
        }
        Ok(Encoding { container: container.to_string(), video: Encoder::parse(video)?, audio: Encoder::parse(audio)? })
    }
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Video>();
    ask::register(r);
    // What is in a video file, as a stream.
    let _ = r.op("decode").label("Frames and sound").input::<Video>("source").output::<ty::Any>().streams(|call| {
        let source = call.take::<Video>(0)?;
        Ok(Box::new(decode::Reading::start(call, &source)?))
    });
    let _ = r
        .op("encode")
        .label("Video file")
        .input::<ty::Stream<ty::Any>>("frames")
        .output::<ty::File<Video>>()
        .parameter::<String>("encoding")
        .run(|call| {
            let encoding = Encoding::parse(&call.parameter::<String>()?)?;
            let stream = call.take::<ty::Stream<ty::Any>>(0)?;
            let rate = stream.header().get("rate").and_then(Value::as_float).unwrap_or(15.0);
            let source = format!("source.{}", ask::extension(&encoding.container)?);
            let video = call.temp()?;
            let mut encoder: Option<Process> = None;
            let mut sound: Vec<AudioData> = Vec::new();
            while let Some(item) = stream.next()? {
                let parts = if let Data::List(parts) = item { parts } else { vec![item] };
                for part in parts {
                    match part {
                        Data::Video(frame) => {
                            if frame.format != PixelFormat::Yuv420p {
                                bail!("it takes yuv420p frames, not {}", frame.format.name());
                            }
                            if encoder.is_none() {
                                encoder = Some(encode(&encoding, frame.width, frame.height, rate, &video)?);
                            }
                            if let Some(process) = &mut encoder
                                && process.write_buffer(&frame.buffer).is_err()
                            {
                                return Err(encoder.take().map_or_else(|| anyhow!("no encoder"), stopped));
                            }
                        }
                        Data::Audio(heard) => sound.push(heard),
                        _ => {}
                    }
                }
            }
            encoder.ok_or_else(|| anyhow!("the stream has no frames"))?.wait()?;
            let Some(first) = sound.first() else { return Ok(Typed::new(Video::NAME, vec![call.keep(&video, &source)?])) };
            if first.format != SampleFormat::S16Interleaved {
                bail!("it takes s16-interleaved sound, not {}", first.format.name());
            }
            let file = call.temp()?;
            let mut args = strings(&[
                "-hide_banner", "-loglevel", "error", "-y", "-i", &video, "-f", "s16le", "-ar", &first.sample_rate.to_string(),
                "-ac", &first.channels.to_string(), "-i", "pipe:0", "-map", "0:v", "-map", "1:a", "-c:v", "copy",
            ]);
            args.extend(encoding.audio.args("a"));
            args.extend(strings(&["-strict", "-2", "-f", &encoding.container, &file]));
            let mut mux = spawn("ffmpeg", &args)?;
            if sound.iter().any(|heard| mux.write_buffer(&heard.buffer).is_err()) {
                return Err(stopped(mux));
            }
            mux.wait()?;
            let _ = std::fs::remove_file(&video);
            let made: File = call.keep(&file, &source)?;
            Ok(Typed::new(Video::NAME, vec![made]))
        });
    Ok(())
}

/// Why a program stopped reading: what it said, not the broken pipe.
fn stopped(process: Process) -> pe2_ext::Error {
    process.wait().err().unwrap_or_else(|| anyhow!("ffmpeg stopped reading"))
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(ToString::to_string).collect()
}

/// ffmpeg, encoding frames written to it to a file.
fn encode(encoding: &Encoding, width: u32, height: u32, rate: f64, file: &str) -> Result<Process> {
    let mut args = strings(&[
        "-hide_banner", "-loglevel", "error", "-y", "-color_range", "pc", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s",
        &format!("{width}x{height}"), "-r", &rate.to_string(), "-i", "pipe:0",
    ]);
    args.extend(encoding.video.args("v"));
    // Full range going in and coming out: an encoder taking no `yuvj420p` is not given limited range by a conversion.
    args.extend(strings(&["-color_range", "pc", "-f", &encoding.container, file]));
    spawn("ffmpeg", &args)
}

/// A table of one entry.
fn table(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Table(entries.into_iter().map(|(key, value)| (key.to_string(), value)).collect::<Table>())
}
