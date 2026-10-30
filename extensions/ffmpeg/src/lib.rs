//! pe2.ffmpeg, an unsandboxed extension: video files through ffmpeg, offered as the `video-container` service. What uses
//! it gives a stream of frames and sound and gets a file, not knowing what is behind it. ffmpeg is the one it ships
//! where it ships one, else as the system finds it.
//!
//! A file is lossless: H.264 at quantizer 0, 4:2:0 tagged full range, and FLAC, so frames and sound come back byte for
//! byte. ffmpeg takes one pipe, so it runs twice: the frames written, then the sound with the video copied in.

use pe2_ext::media::{AudioData, PixelFormat, SampleFormat};
use pe2_ext::process::{Process, spawn};
use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{Data, File, Registry, Result, Signature, Typed, anyhow, bail};

pe2_ext::extension!("pe2.ffmpeg", "0.1.0", register, provides = ["video-container@1.0.0"], unsandboxed = true);

/// An MP4 file.
pub struct Mp4;

impl Registered for Mp4 {
    const NAME: &str = "pe2.ffmpeg@mp4";
    const LABEL: &str = "MP4 video";
    const MEDIA_TYPE: Option<&str> = Some("video/mp4");

    fn signatures() -> Vec<Signature> {
        vec![Signature::Bytes { offset: 4, bytes: b"ftyp".to_vec() }]
    }
}

impl Files for Mp4 {}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Mp4>();
    let _ = r.op("mp4_write").label("MP4, written").input::<ty::Stream<ty::Any>>("frames").output::<ty::File<Mp4>>().run(|call| {
        let stream = call.take::<ty::Stream<ty::Any>>(0)?;
        let rate = stream.header().get("rate").and_then(pe2_ext::Value::as_float).unwrap_or(15.0);
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
                            encoder = Some(encode(frame.width, frame.height, rate, &video)?);
                        }
                        if let Some(encoder) = &mut encoder {
                            encoder.write_buffer(&frame.buffer)?;
                        }
                    }
                    Data::Audio(heard) => sound.push(heard),
                    _ => {}
                }
            }
        }
        encoder.ok_or_else(|| anyhow!("the stream has no frames"))?.wait()?;
        let Some(first) = sound.first() else { return Ok(Typed::new(Mp4::NAME, vec![call.keep(&video, SOURCE)?])) };
        if first.format != SampleFormat::S16Interleaved {
            bail!("it takes s16-interleaved sound, not {}", first.format.name());
        }
        let file = call.temp()?;
        let mut mux = spawn("ffmpeg", &strings(&[
            "-hide_banner", "-loglevel", "error", "-y", "-i", &video, "-f", "s16le", "-ar", &first.sample_rate.to_string(),
            "-ac", &first.channels.to_string(), "-i", "pipe:0", "-map", "0:v", "-map", "1:a", "-c:v", "copy", "-c:a", "flac",
            "-strict", "-2", "-f", "mp4", &file,
        ]))?;
        for heard in &sound {
            mux.write_buffer(&heard.buffer)?;
        }
        mux.wait()?;
        let _ = std::fs::remove_file(&video);
        let made: File = call.keep(&file, SOURCE)?;
        Ok(Typed::new(Mp4::NAME, vec![made]))
    });
    Ok(())
}

/// What a video is named.
const SOURCE: &str = "source.mp4";

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(ToString::to_string).collect()
}

/// ffmpeg, encoding frames written to it to a file.
fn encode(width: u32, height: u32, rate: f64, file: &str) -> Result<Process> {
    spawn("ffmpeg", &strings(&[
        "-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuvj420p", "-s", &format!("{width}x{height}"),
        "-r", &rate.to_string(), "-i", "pipe:0", "-c:v", "libx264", "-qp", "0", "-preset", "medium", "-f", "mp4", file,
    ]))
}
