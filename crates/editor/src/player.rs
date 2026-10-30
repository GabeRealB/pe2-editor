//! The player: an asset of a kind that plays (`KindBuilder::playback`) shown in the preview frame by frame. Its frames
//! are the members of its kind's frame output, made off the window's thread - those just ahead while it plays - and
//! shown through one texture. Its sound, its kind's audio output, is made once on a thread of its own and played from
//! where it is, resampled to the device's rate; while it sounds, where it is follows the samples played, else the
//! clock.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use eframe::egui::{self, RichText};
use project::ext::Extensions;
use project::index::Index;

use crate::settings::Preview;
use crate::{icons, theme};

/// How many frames are asked for ahead of the one shown while it plays.
const AHEAD: u32 = 8;
/// How many frames are kept behind the one shown.
const BEHIND: u32 = 4;

/// Frames to make of an asset, in order: the newest request replaces those not begun.
struct Request {
    index: Arc<Index>,
    path: String,
    output: String,
    numbers: Vec<u32>,
}

/// A frame made, or why not; none where its request was replaced before it was begun.
type Made = (String, u32, Option<Result<Arc<egui::ColorImage>, String>>);

/// What is shown: its asset (as it was when its timing was read), its frames' output and timing, where it is.
struct Shown {
    asset: Arc<project::index::Asset>,
    output: String,
    timing: pe2_ext::Timing,
    /// The frame shown, a fraction past it while playing.
    at: f64,
    /// Playing: since when, from where.
    playing: Option<(Instant, f64)>,
    /// Held on the timeline while it played: it plays again from there when let go.
    held: bool,
    frames: BTreeMap<u32, Arc<egui::ColorImage>>,
    /// Asked for and not made yet.
    asked: Vec<u32>,
    failed: Option<String>,
    /// Its audio output, where its kind has one.
    audio: Option<String>,
    sound: Sound,
    /// Sounding: the device's stream, and where it is.
    sounding: Option<Sounding>,
}

/// An asset's sound.
enum Sound {
    Unasked,
    Making,
    Made(Arc<pe2_ext::Pcm>),
    /// None to play, or none could be.
    Silent(String),
}

/// Sound playing on the device: its stream, and where it is.
struct Sounding {
    _stream: cpal::Stream,
    clock: Arc<SoundClock>,
    rate: u32,
}

/// Where a sound is, in samples (frames of its channels): told by each buffer the device takes - the sample it begins
/// at, when it was taken, how many it holds - and between buffers the time since, within the buffer. A device takes a
/// buffer at a time (a tenth of a second, through `PulseAudio`), so the samples taken alone move in steps longer than
/// a frame of the picture.
struct SoundClock {
    begun: Instant,
    /// Each as `f64` bits: the buffer's first sample, its length; when it was taken, in nanoseconds since `begun`.
    at: AtomicU64,
    len: AtomicU64,
    taken: AtomicU64,
}

impl SoundClock {
    fn new(from: f64) -> SoundClock {
        SoundClock { begun: Instant::now(), at: AtomicU64::new(from.to_bits()), len: AtomicU64::new(0), taken: AtomicU64::new(0) }
    }

    fn took(&self, at: f64, len: f64) {
        #[expect(clippy::cast_possible_truncation, reason = "nanoseconds of a sound playing")]
        let now = self.begun.elapsed().as_nanos() as u64;
        self.at.store(at.to_bits(), Ordering::Relaxed);
        self.len.store(len.to_bits(), Ordering::Relaxed);
        self.taken.store(now, Ordering::Relaxed);
    }

    /// The sample playing now, at `rate` samples a second.
    fn now(&self, rate: f64) -> f64 {
        let at = f64::from_bits(self.at.load(Ordering::Relaxed));
        let len = f64::from_bits(self.len.load(Ordering::Relaxed));
        let since = self.begun.elapsed().saturating_sub(std::time::Duration::from_nanos(self.taken.load(Ordering::Relaxed)));
        at + (since.as_secs_f64() * rate).min(len)
    }
}

type MadeSound = (String, Result<Arc<pe2_ext::Pcm>, String>);

pub struct Player {
    extensions: Arc<Extensions>,
    requests: mpsc::Sender<Request>,
    made: mpsc::Receiver<Made>,
    sounds: (mpsc::Sender<MadeSound>, mpsc::Receiver<MadeSound>),
    shown: Option<Shown>,
    texture: Option<(egui::TextureHandle, String, u32)>,
    ctx: Arc<OnceLock<egui::Context>>,
    /// The amplitude sound plays at, as `f32` bits: shared with what sounds, so a change is heard at once.
    gain: Arc<AtomicU32>,
    /// Why the device would not play, where it would not.
    device: Arc<Mutex<Option<String>>>,
}

impl Player {
    pub fn new(extensions: Arc<Extensions>) -> Player {
        let worker = extensions.clone();
        let (requests, asked) = mpsc::channel::<Request>();
        let (made_tx, made) = mpsc::channel();
        let ctx: Arc<OnceLock<egui::Context>> = Arc::default();
        let wake = ctx.clone();
        let _ = std::thread::Builder::new().name("player".into()).spawn(move || {
            let extensions = worker;
            let engine = extensions.engine();
            while let Ok(mut request) = asked.recv() {
                // Only the newest request is made.
                while let Ok(newer) = asked.try_recv() {
                    // Those it asked for are not made: asked for again where still wanted.
                    let replaced = std::mem::replace(&mut request, newer);
                    for number in replaced.numbers {
                        let _ = made_tx.send((replaced.path.clone(), number, None));
                    }
                }
                for number in request.numbers {
                    let made = request.index.assets.get(&request.path).map(|asset| {
                        extensions.output(&engine, &request.index, asset, &format!("{}/{number}", request.output), true)
                    });
                    let image = match made {
                        Some(Ok(given)) => match given.value.as_deref() {
                            Some(project::graph::Value::Image(image)) => Ok(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
                                [image.width, image.height],
                                &image.rgba,
                            ))),
                            _ => Err("it made no picture".to_string()),
                        },
                        Some(Err(e)) => Err(format!("{e:#}")),
                        None => Err("it is gone".to_string()),
                    };
                    if made_tx.send((request.path.clone(), number, Some(image))).is_err() {
                        return;
                    }
                    if let Some(ctx) = wake.get() {
                        ctx.request_repaint();
                    }
                }
            }
        });
        let sounds = mpsc::channel();
        Player { extensions, requests, made, sounds, shown: None, texture: None, ctx, gain: Arc::new(AtomicU32::new(1f32.to_bits())), device: Arc::default() }
    }

    /// Make an asset's sound, on a thread of its own: sent to `made`, the window woken by `wake`.
    fn make_sound(extensions: &Arc<Extensions>, made: &mpsc::Sender<MadeSound>, wake: &Arc<OnceLock<egui::Context>>, at: (&Arc<Index>, &str, &str)) {
        let (index, path, output) = at;
        let (extensions, index, path, output) = (extensions.clone(), index.clone(), path.to_string(), output.to_string());
        let (made, wake) = (made.clone(), wake.clone());
        let _ = std::thread::Builder::new().name("player sound".into()).spawn(move || {
            let engine = extensions.engine();
            let sound = match index.assets.get(&path).map(|a| extensions.output(&engine, &index, a, &output, true)) {
                Some(Ok(given)) => match given.value.as_deref() {
                    Some(project::graph::Value::Pcm(pcm)) if !pcm.samples.is_empty() => Ok(Arc::new(pcm.clone())),
                    Some(project::graph::Value::Pcm(_)) => Err("it has none".to_string()),
                    _ => Err("it made no sound".to_string()),
                },
                Some(Err(e)) => Err(format!("{e:#}")),
                None => Err("it is gone".to_string()),
            };
            let _ = made.send((path, sound));
            if let Some(ctx) = wake.get() {
                ctx.request_repaint();
            }
        });
    }

    /// Whether an asset plays.
    pub fn plays(extensions: &Extensions, asset: &project::index::Asset) -> bool {
        extensions.kind_of(asset).is_some_and(|k| k.playback.is_some())
    }

    /// Its sound made and sounding while it plays, unless muted; where it is, by the sound's samples or the clock.
    fn keep_time(&mut self, index: &Arc<Index>, path: &str, muted: bool) {
        let Some(shown) = &mut self.shown else { return };
        for (made_path, sound) in self.sounds.1.try_iter() {
            if made_path == shown.asset.path {
                shown.sound = sound.map_or_else(Sound::Silent, Sound::Made);
            }
        }
        // The device failing stops the sound: the clock goes on.
        if let Some(failed) = self.device.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            shown.sound = Sound::Silent(format!("the device: {failed}"));
            shown.sounding = None;
            shown.playing = shown.playing.map(|_| (Instant::now(), shown.at));
        }
        let (first, last) = (shown.timing.first, shown.timing.first + shown.timing.count.saturating_sub(1));
        // Sounding while it plays, unless muted: from where it is.
        if shown.playing.is_none() || muted {
            shown.sounding = None;
        } else if shown.sounding.is_none() {
            match (&shown.sound, &shown.audio) {
                (Sound::Unasked, Some(output)) => {
                    shown.sound = Sound::Making;
                    let output = output.clone();
                    Player::make_sound(&self.extensions, &self.sounds.0, &self.ctx, (index, path, &output));
                }
                (Sound::Made(pcm), _) => {
                    let start = (shown.at - f64::from(first)) / shown.timing.rate * f64::from(pcm.rate);
                    match sound(pcm.clone(), start.max(0.0), &self.gain, self.device.clone()) {
                        Ok(sounding) => {
                            shown.sounding = Some(sounding);
                            // The clock goes on from here, should the sound stop.
                            shown.playing = Some((Instant::now(), shown.at));
                        }
                        Err(e) => shown.sound = Sound::Silent(e),
                    }
                }
                _ => {}
            }
        }
        if let Some((since, from)) = shown.playing {
            shown.at = match &shown.sounding {
                // Never back: a buffer may begin a little before where the time since the last put it.
                Some(s) => {
                    let rate = f64::from(s.rate);
                    shown.at.max(f64::from(first) + s.clock.now(rate) / rate * shown.timing.rate)
                }
                None => from + since.elapsed().as_secs_f64() * shown.timing.rate,
            };
            if shown.at >= f64::from(last) {
                shown.at = f64::from(last);
                shown.playing = None;
                shown.sounding = None;
            }
        }
    }

    /// An asset played: its frame where it is, the controls under it.
    pub fn ui(&mut self, ui: &mut egui::Ui, (extensions, index): (&Extensions, &Arc<Index>), path: &str, view: &mut Preview) {
        let _ = self.ctx.set(ui.ctx().clone());
        let volume = view.volume.clamp(0.0, 1.0);
        self.gain.store((volume * volume).to_bits(), Ordering::Relaxed);
        let Some(asset) = index.assets.get(path) else { return };
        // Its timing read again where it is another asset, or changed.
        if self.shown.as_ref().is_none_or(|s| !Arc::ptr_eq(&s.asset, asset)) {
            let at = self.shown.as_ref().filter(|s| s.asset.path == *path).map(|s| s.at);
            self.shown = extensions.playback(asset).map(|(playback, timing)| Shown {
                asset: asset.clone(),
                output: playback.frames.clone(),
                timing,
                at: at.unwrap_or(f64::from(timing.first)),
                playing: None,
                held: false,
                frames: BTreeMap::new(),
                asked: Vec::new(),
                failed: None,
                audio: playback.audio.clone(),
                sound: Sound::Unasked,
                sounding: None,
            });
        }
        let Some(shown) = &mut self.shown else {
            ui.label(RichText::new("It does not say how it plays").color(theme::weak()));
            return;
        };
        for (made_path, number, image) in self.made.try_iter() {
            if made_path != shown.asset.path {
                continue;
            }
            shown.asked.retain(|&n| n != number);
            match image {
                Some(Ok(image)) => {
                    shown.frames.insert(number, image);
                }
                Some(Err(e)) => shown.failed = Some(format!("frame {number}: {e}")),
                None => {}
            }
        }
        self.keep_time(index, path, view.muted);
        let Some(shown) = &mut self.shown else { return };
        let (first, last) = (shown.timing.first, shown.timing.first + shown.timing.count.saturating_sub(1));
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a frame number within its count")]
        let number = (shown.at.floor() as u32).clamp(first, last);
        // Ask for the frame and, playing, those after it; keep those near it.
        let wanted: Vec<u32> = if shown.playing.is_some() { (number..=last.min(number + AHEAD)).collect() } else { vec![number] };
        let missing: Vec<u32> = wanted.into_iter().filter(|n| !shown.frames.contains_key(n) && !shown.asked.contains(n)).collect();
        if !missing.is_empty() {
            shown.asked.extend(&missing);
            let output = shown.output.clone();
            let _ = self.requests.send(Request { index: index.clone(), path: path.to_string(), output, numbers: missing });
        }
        shown.frames.retain(|&n, _| n + BEHIND >= number && n <= number + AHEAD);
        // The frame shown: this one where it is made, else the last one shown.
        if let Some(image) = shown.frames.get(&number)
            && self.texture.as_ref().is_none_or(|(_, p, n)| p != path || *n != number)
        {
            let options = egui::TextureOptions::LINEAR;
            if let Some((handle, p, n)) = &mut self.texture {
                handle.set((**image).clone(), options);
                (*p, *n) = (path.to_string(), number);
            } else {
                let handle = ui.ctx().load_texture("player", (**image).clone(), options);
                self.texture = Some((handle, path.to_string(), number));
            }
        }
        if let Some((handle, _, _)) = self.texture.as_ref().filter(|(_, p, _)| p == path) {
            let size = handle.size_vec2();
            let scale = (ui.available_width() / size.x).max(0.1);
            ui.add(egui::Image::new((handle.id(), size * scale)));
        } else {
            ui.add_space(8.0);
            ui.label(RichText::new(shown.failed.as_deref().unwrap_or("Making its frame…")).color(theme::weak()));
        }
        ui.add_space(6.0);
        controls(ui, shown, number, view);
        if shown.playing.is_some() {
            ui.ctx().request_repaint();
        }
    }
}

/// Play or pause, where it is (a frame), and the time.
fn controls(ui: &mut egui::Ui, shown: &mut Shown, number: u32, view: &mut Preview) {
    let (first, last) = (shown.timing.first, shown.timing.first + shown.timing.count.saturating_sub(1));
    let rate = shown.timing.rate.max(1.0);
    ui.horizontal(|ui| {
        let playing = shown.playing.is_some() || shown.held;
        let (glyph, says) = if playing { (icons::PAUSE, "Pause") } else { (icons::PLAY, "Play") };
        if ui.button(RichText::new(glyph).size(16.0)).on_hover_text(says).clicked() {
            shown.playing = if playing {
                shown.held = false;
                None
            } else {
                // From the start again where it ended.
                if number >= last {
                    shown.at = f64::from(first);
                }
                Some((Instant::now(), shown.at))
            };
        }
        // The timeline takes what the time and the sound's controls leave.
        let sound = if shown.audio.is_some() { 120.0 } else { 0.0 };
        let mut chosen = number;
        ui.spacing_mut().slider_width = (ui.available_width() - 110.0 - sound).max(80.0);
        let slider = ui.add(egui::Slider::new(&mut chosen, first..=last).show_value(false));
        // Held, it stops where it is held - picture and sound - and plays on from there when let go, if it played.
        if slider.is_pointer_button_down_on() || slider.dragged() {
            if shown.playing.take().is_some() {
                shown.held = true;
                shown.sounding = None;
            }
        } else if std::mem::take(&mut shown.held) {
            shown.playing = Some((Instant::now(), shown.at));
        }
        if slider.changed() {
            shown.at = f64::from(chosen);
            if shown.playing.is_some() {
                // Sounding again from there.
                shown.playing = Some((Instant::now(), shown.at));
                shown.sounding = None;
            }
        }
        let time = |frame: u32| {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "seconds of a movie")]
            let seconds = (f64::from(frame - first) / rate) as u64;
            format!("{}:{:02}", seconds / 60, seconds % 60)
        };
        ui.label(RichText::new(format!("{} / {}", time(number), time(last))).monospace());
        if shown.audio.is_some() {
            ui.add_space(8.0);
            let (glyph, says) = if view.muted { (icons::SPEAKER_X, "Sound off") } else { (icons::SPEAKER_HIGH, "Sound on") };
            if ui.button(RichText::new(glyph).size(16.0)).on_hover_text(says).clicked() {
                view.muted = !view.muted;
            }
            ui.spacing_mut().slider_width = 70.0;
            ui.add_enabled_ui(!view.muted, |ui| {
                let percent = (view.volume * 100.0).round();
                ui.add(egui::Slider::new(&mut view.volume, 0.0..=1.0).step_by(0.05).show_value(false)).on_hover_text(format!("Volume {percent}%"));
            });
        }
    });
    ui.label(
        RichText::new(format!("Frame {number} of {first}–{last}  ·  {:.3} a second", shown.timing.rate))
            .small()
            .color(theme::weak()),
    );
    if let Some(failed) = &shown.failed {
        ui.label(RichText::new(failed).small().color(theme::weak()));
    }
    let sound = match &shown.sound {
        _ if shown.audio.is_none() => None,
        Sound::Unasked => Some("Sound: made when it plays".to_string()),
        Sound::Making => Some("Sound: being made…".to_string()),
        Sound::Made(pcm) => Some(format!(
            "Sound: {} Hz, {}",
            pcm.rate,
            if pcm.channels == 2 { "stereo" } else { "mono" }
        )),
        Sound::Silent(why) => Some(format!("No sound: {why}")),
    };
    if let Some(sound) = sound {
        ui.label(RichText::new(sound).small().color(theme::weak()));
    }
}

/// Play a sound on the default device from a sample of it (a frame of its channels, fractional), resampled to the
/// device's rate: what plays, and where it is.
fn sound(pcm: Arc<pe2_ext::Pcm>, from: f64, gain: &Arc<AtomicU32>, failed: Arc<Mutex<Option<String>>>) -> Result<Sounding, String> {
    let device = output_device().ok_or("no sound device")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let clock = Arc::new(SoundClock::new(from));
    let rate = pcm.rate;
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => stream::<f32>(&device, &config.into(), (pcm, gain.clone()), (from, clock.clone()), failed),
        cpal::SampleFormat::I16 => stream::<i16>(&device, &config.into(), (pcm, gain.clone()), (from, clock.clone()), failed),
        cpal::SampleFormat::U16 => stream::<u16>(&device, &config.into(), (pcm, gain.clone()), (from, clock.clone()), failed),
        other => return Err(format!("the device takes {other} samples")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Sounding { _stream: stream, clock, rate })
}

/// The device sound goes to: `PulseAudio`'s, where ALSA has one - where a desktop's default goes, and the only way out
/// where ALSA has no card of its own (WSL) - else the default.
fn output_device() -> Option<cpal::Device> {
    let host = cpal::default_host();
    let pulse = host.output_devices().ok().and_then(|mut devices| devices.find(|d| d.name().is_ok_and(|n| n == "pulse")));
    pulse.or_else(|| host.default_output_device())
}

fn stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    (pcm, gain): (Arc<pe2_ext::Pcm>, Arc<AtomicU32>),
    (from, clock): (f64, Arc<SoundClock>),
    failed: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels);
    let source = usize::from(pcm.channels.max(1));
    let frames = pcm.samples.len() / source;
    let step = f64::from(pcm.rate) / f64::from(config.sample_rate.0);
    let sample = move |frame: usize, channel: usize| {
        let s = pcm.samples.get(frame * source + channel.min(source - 1)).copied().unwrap_or(0);
        f32::from(s) / 32768.0 * f32::from_bits(gain.load(Ordering::Relaxed))
    };
    let mut position = from;
    let output = move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
        #[expect(clippy::cast_precision_loss, reason = "a buffer's samples")]
        clock.took(position, (out.len() / channels) as f64 * step);
        for frame in out.chunks_mut(channels) {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a sample's place in the sound")]
            let i = position as usize;
            for (c, out) in frame.iter_mut().enumerate() {
                *out = T::from_sample(if i < frames && c < 2 { sample(i, c) } else { 0.0 });
            }
            position += step;
        }
    };
    let error = move |e: cpal::StreamError| {
        *failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e.to_string());
    };
    device.build_output_stream(config, output, error, None).map_err(|e| e.to_string())
}
