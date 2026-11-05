//! The player: an asset whose type has a stream output (`stream<T>`) played in the preview. The stream, opened on a
//! thread of its own, is read by two readers of the player's: one for the picture, at the item shown, and one for the
//! sound, half a second ahead, feeding each item's samples to the device. An item is shown by what it holds: a
//! picture, sound, or both (a list of them): a picture as RGBA through a texture, a frame in YCbCr 4:2:0 through the
//! GPU's shader (`yuv`); sound as 16-bit samples.
//!
//! Where it is follows the sound while it sounds - the samples the device has played, mapped back to the items they
//! came with - else the clock at the stream's rate, which waits while the item it is at is not made yet: no item is
//! skipped for being late.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use eframe::egui::{self, RichText};
use pe2_ext::media::{PixelFormat, SampleFormat, VideoFrame};
use project::ext::Extensions;
use project::graph::Value;
use project::graph::stream::{Poll, Reader, Stream};
use project::index::Index;

use crate::settings::{Preview, Upscale};
use crate::{icons, theme};

/// How far ahead of the picture the sound is fed, in items.
const FEED: u64 = 8;

/// A picture an item holds, in a layout the player shows.
enum Picture<'a> {
    Rgba { width: usize, height: usize, rgba: &'a [u8] },
    Yuv(&'a VideoFrame),
}

/// Sound an item holds: its rate, its channels, and its 16-bit samples, interleaved - as numbers, or as their bytes.
struct Sound<'a> {
    rate: u32,
    channels: usize,
    samples: Samples<'a>,
}

enum Samples<'a> {
    S16(&'a [i16]),
    Le(&'a [u8]),
}

impl Sound<'_> {
    fn len(&self) -> usize {
        match self.samples {
            Samples::S16(s) => s.len(),
            Samples::Le(b) => b.len() / 2,
        }
    }

    fn extend(&self, into: &mut VecDeque<i16>) {
        match self.samples {
            Samples::S16(s) => into.extend(s),
            Samples::Le(b) => into.extend(b.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b))),
        }
    }
}

/// What an item holds to play, of what the player shows: its picture and its sound, either or both.
fn parts(item: &Value) -> (Option<Picture<'_>>, Option<Sound<'_>>) {
    match item {
        Value::Image(image) => {
            (Some(Picture::Rgba { width: image.width, height: image.height, rgba: &image.rgba }), None)
        }
        Value::Video(frame) => match frame.format {
            PixelFormat::Rgba8888 => {
                let (width, height) = (frame.width as usize, frame.height as usize);
                (Some(Picture::Rgba { width, height, rgba: frame.buffer.bytes() }), None)
            }
            PixelFormat::Yuv420p => (Some(Picture::Yuv(frame)), None),
            PixelFormat::Bgra8888 | PixelFormat::Nv12 => (None, None),
        },
        Value::Pcm(pcm) => (
            None,
            Some(Sound {
                rate: pcm.rate,
                channels: usize::from(pcm.channels.max(1)),
                samples: Samples::S16(&pcm.samples),
            }),
        ),
        Value::Audio(sound) if sound.format == SampleFormat::S16Interleaved => {
            let channels = (sound.channels as usize).max(1);
            (None, Some(Sound { rate: sound.sample_rate, channels, samples: Samples::Le(sound.buffer.bytes()) }))
        }
        Value::List(items) => {
            let found: Vec<_> = items.iter().map(parts).collect();
            let mut picture = None;
            let mut sound = None;
            for (p, s) in found {
                picture = picture.or(p);
                sound = sound.or(s);
            }
            (picture, sound)
        }
        _ => (None, None),
    }
}

/// The picture shown: through the texture, or a YCbCr frame by its key.
enum Showing {
    Texture,
    Yuv(VideoFrame, u64),
}

/// A stream being opened, or open.
enum Opening {
    Waiting(mpsc::Receiver<Result<Arc<Stream>, String>>),
    Open { picture: Reader, sound: Reader },
    Failed(String),
}

/// What is shown: its asset (as it was when opened), its stream, where it is.
struct Shown {
    asset: Arc<project::index::Asset>,
    opening: Opening,
    /// Items a second, how many there are where known, the number its first is called by.
    rate: f64,
    count: Option<u64>,
    first: u64,
    /// The item shown, a fraction past it while playing.
    at: f64,
    /// Playing by the clock: since when, from where.
    playing: Option<(Instant, f64)>,
    /// Held on the timeline while it played: it plays again from there when let go.
    held: bool,
    sounding: Option<Sounding>,
    /// No sound to play, and why: the stream has none, the device would not, it ended.
    silent: Option<String>,
    /// The item shown last, and how.
    at_item: Option<u64>,
    showing: Option<Showing>,
    failed: Option<String>,
}

impl Shown {
    fn last(&self) -> u64 {
        self.count.map_or(u64::MAX, |c| c.saturating_sub(1))
    }

    /// Play from where it is by the clock - the sound to follow, where there is one.
    fn by_clock(&mut self) {
        self.sounding = None;
        self.playing = Some((Instant::now(), self.at));
    }
}

/// Sound playing on the device: its stream, the queue it plays from and where it is, and the items its samples came
/// with.
struct Sounding {
    _device: cpal::Stream,
    queue: Arc<Mutex<Queue>>,
    clock: Arc<SoundClock>,
    /// The rate its samples come at, told by the first fed: shared with the device's callback.
    rate: u32,
    source_rate: Arc<AtomicU32>,
    /// The next item whose sound is fed.
    fed: u64,
    /// Each item fed and where its samples begin, in samples (frames of channels) since the sound began.
    spans: VecDeque<(u64, f64)>,
    /// The samples fed.
    total: f64,
    started: bool,
}

/// The samples to play, and how many channels they come in.
struct Queue {
    samples: VecDeque<i16>,
    channels: usize,
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
    fn new() -> SoundClock {
        SoundClock {
            begun: Instant::now(),
            at: AtomicU64::new(0f64.to_bits()),
            len: AtomicU64::new(0),
            taken: AtomicU64::new(0),
        }
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
        let since =
            self.begun.elapsed().saturating_sub(std::time::Duration::from_nanos(self.taken.load(Ordering::Relaxed)));
        at + (since.as_secs_f64() * rate).min(len)
    }
}

pub struct Player {
    extensions: Arc<Extensions>,
    /// Which player it is, to the renderer.
    id: u64,
    shown: Option<Shown>,
    texture: Option<egui::TextureHandle>,
    /// The key of the YCbCr frame shown last: each new one a new key.
    keys: u64,
    /// The amplitude sound plays at, as `f32` bits: shared with what sounds, so a change is heard at once.
    gain: Arc<AtomicU32>,
    /// Why the device would not play, where it would not.
    device: Arc<Mutex<Option<String>>>,
}

impl Drop for Player {
    fn drop(&mut self) {
        crate::yuv::forget(self.id);
    }
}

impl Player {
    pub fn new(extensions: Arc<Extensions>) -> Player {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Player {
            extensions,
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            shown: None,
            texture: None,
            keys: 0,
            gain: Arc::new(AtomicU32::new(1f32.to_bits())),
            device: Arc::default(),
        }
    }

    /// Stop where it is: its tab is not shown, and what is not seen is not heard.
    pub fn pause(&mut self) {
        let Some(shown) = &mut self.shown else { return };
        if shown.sounding.is_some() {
            shown.by_clock();
        }
        shown.playing = None;
        shown.held = false;
    }

    /// Whether an asset plays: its type has a stream output.
    pub fn plays(extensions: &Extensions, asset: &project::index::Asset) -> bool {
        extensions.stream_output(asset).is_some()
    }

    /// Open an asset's stream on a thread of its own.
    fn open(&self, ctx: &egui::Context, index: &Arc<Index>, asset: &Arc<project::index::Asset>) -> Shown {
        let (sent, opening) = mpsc::channel();
        let (extensions, index, path, ctx) = (self.extensions.clone(), index.clone(), asset.path.clone(), ctx.clone());
        let _ = std::thread::Builder::new().name("player open".into()).spawn(move || {
            let engine = extensions.engine();
            let opened = index.assets.get(&path).ok_or_else(|| "it is gone".to_string()).and_then(|asset| {
                let output = extensions.stream_output(asset).ok_or_else(|| "it has no stream".to_string())?;
                extensions.stream(&engine, &index, asset, &output).map_err(|e| format!("{e:#}"))
            });
            if let Ok(stream) = &opened {
                let wake = ctx.clone();
                stream.on_made(Arc::new(move || wake.request_repaint()));
            }
            let _ = sent.send(opened);
            ctx.request_repaint();
        });
        Shown {
            asset: asset.clone(),
            opening: Opening::Waiting(opening),
            rate: 15.0,
            count: None,
            first: 0,
            at: 0.0,
            playing: None,
            held: false,
            sounding: None,
            silent: None,
            at_item: None,
            showing: None,
            failed: None,
        }
    }

    /// An asset played: its picture where it is, the controls under it.
    pub fn ui(&mut self, ui: &mut egui::Ui, (_, index): (&Extensions, &Arc<Index>), path: &str, view: &mut Preview) {
        // As the pictures are: the frame as it is, unless the setting says otherwise. Filling the pane is `fitting`'s.
        let smooth = matches!(view.upscale, Upscale::Bilinear | Upscale::Sharp);
        let sampling = if smooth { egui::TextureOptions::LINEAR } else { egui::TextureOptions::NEAREST };
        let volume = view.volume.clamp(0.0, 1.0);
        self.gain.store((volume * volume).to_bits(), Ordering::Relaxed);
        let Some(asset) = index.assets.get(path) else { return };
        // Opened again where it is another asset, or changed: where it was kept.
        if self.shown.as_ref().is_none_or(|s| !Arc::ptr_eq(&s.asset, asset)) {
            let at = self.shown.as_ref().filter(|s| s.asset.path == *path).map(|s| s.at);
            let mut shown = self.open(ui.ctx(), index, asset);
            shown.at = at.unwrap_or(0.0);
            self.shown = Some(shown);
            self.texture = None;
        }
        let Some(shown) = &mut self.shown else { return };
        if let Opening::Waiting(opening) = &shown.opening
            && let Ok(opened) = opening.try_recv()
        {
            shown.opening = match opened {
                Ok(stream) => {
                    let header = stream.header();
                    shown.rate =
                        header.get("rate").and_then(toml::Value::as_float).filter(|r| *r > 0.0).unwrap_or(15.0);
                    shown.count =
                        header.get("count").and_then(toml::Value::as_integer).and_then(|c| u64::try_from(c).ok());
                    shown.first = header
                        .get("first")
                        .and_then(toml::Value::as_integer)
                        .and_then(|c| u64::try_from(c).ok())
                        .unwrap_or(0);
                    Opening::Open { picture: stream.reader(), sound: stream.reader() }
                }
                Err(e) => Opening::Failed(e),
            };
        }
        match &shown.opening {
            Opening::Waiting(_) => {
                ui.label(RichText::new("Opening…").color(theme::weak()));
                return;
            }
            Opening::Failed(why) => {
                ui.label(RichText::new(format!("It could not be opened: {why}")).color(theme::weak()));
                return;
            }
            Opening::Open { .. } => {}
        }
        let device = self.device.lock().unwrap_or_else(PoisonError::into_inner).take();
        keep_time(shown, view.muted, device, &self.gain, &self.device);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "an item's place")]
        let number = (shown.at.max(0.0).floor() as u64).min(shown.last());
        let Opening::Open { picture, sound } = &shown.opening else { return };
        // Not feeding, the sound's reader keeps with the picture's: an old place would keep the queue filling there.
        if shown.sounding.is_none() {
            let _ = sound.get(number);
        }
        match picture.get(number) {
            Poll::Ready(item) if shown.at_item != Some(number) => {
                match parts(&item).0 {
                    Some(Picture::Rgba { width, height, rgba }) => {
                        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], rgba);
                        match &mut self.texture {
                            Some(texture) => texture.set(image, sampling),
                            None => self.texture = Some(ui.ctx().load_texture("player", image, sampling)),
                        }
                        shown.showing = Some(Showing::Texture);
                    }
                    Some(Picture::Yuv(frame)) => {
                        self.keys += 1;
                        shown.showing = Some(Showing::Yuv(frame.clone(), self.keys));
                    }
                    None => {}
                }
                shown.at_item = Some(number);
            }
            Poll::Ready(_) => {}
            // Not made yet: by the clock, it waits here.
            Poll::Pending => {
                if shown.sounding.is_none()
                    && let Some((_, _)) = shown.playing
                {
                    shown.playing = Some((Instant::now(), shown.at));
                }
            }
            Poll::End => {
                shown.count = Some(number);
                shown.playing = None;
                shown.sounding = None;
            }
            Poll::Failed(e) => shown.failed = Some(e),
        }
        show(ui, shown, self.texture.as_ref(), self.id, smooth);
        ui.add_space(6.0);
        controls(ui, shown, number, view);
        // When the next item is due and not before: a movie changes far less often than a window can.
        if shown.playing.is_some() || shown.sounding.is_some() {
            let items = (shown.at.floor() + 1.0 - shown.at).clamp(0.01, 1.0);
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(items / shown.rate.max(1.0)));
        }
    }
}

/// The picture shown: through the texture, or a YCbCr frame through the GPU's shader; or why there is none yet.
/// How much to scale a picture to fit what is left, its controls still under it.
fn fitting(ui: &egui::Ui, size: egui::Vec2) -> f32 {
    const CONTROLS: f32 = 76.0;
    (ui.available_width() / size.x).min((ui.available_height() - CONTROLS) / size.y).max(0.1)
}

fn show(ui: &mut egui::Ui, shown: &Shown, texture: Option<&egui::TextureHandle>, player: u64, smooth: bool) {
    ui.vertical_centered(|ui| show_centered(ui, shown, texture, player, smooth));
}

fn show_centered(ui: &mut egui::Ui, shown: &Shown, texture: Option<&egui::TextureHandle>, player: u64, smooth: bool) {
    match (&shown.showing, texture) {
        (Some(Showing::Texture), Some(texture)) => {
            let size = texture.size_vec2();
            ui.add(egui::Image::new((texture.id(), size * fitting(ui, size))));
        }
        (Some(Showing::Yuv(frame, key)), _) => {
            #[expect(clippy::cast_precision_loss, reason = "a frame's size")]
            let size = egui::vec2(frame.width as f32, frame.height as f32);
            let (rect, _) = ui.allocate_exact_size(size * fitting(ui, size), egui::Sense::hover());
            let draw = crate::yuv::Draw { frame: frame.clone(), key: *key, player, smooth };
            ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(rect, draw));
        }
        _ => {
            ui.add_space(8.0);
            ui.label(RichText::new(shown.failed.as_deref().unwrap_or("Making its first item…")).color(theme::weak()));
        }
    }
}

/// Sounding started from where it is: the item it is on whole, the clock left to the sound, or why it is silent.
fn start_sounding(shown: &mut Shown, gain: &Arc<AtomicU32>, failed: &Arc<Mutex<Option<String>>>) {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "an item's place")]
    let from = shown.at.max(0.0).floor() as u64;
    match sound(gain, failed.clone()) {
        Ok((device, queue, clock, source_rate)) => {
            let spans = VecDeque::new();
            shown.sounding = Some(Sounding {
                _device: device,
                queue,
                clock,
                rate: 0,
                source_rate,
                fed: from,
                spans,
                total: 0.0,
                started: false,
            });
            shown.at = shown.at.floor();
            shown.playing = None;
        }
        Err(e) => shown.silent = Some(e),
    }
}

/// Where it is: by the sound while it sounds - fed half a second ahead - else by the clock; sounding started while it
/// plays, unless muted or silent.
fn keep_time(
    shown: &mut Shown,
    muted: bool,
    device: Option<String>,
    gain: &Arc<AtomicU32>,
    failed: &Arc<Mutex<Option<String>>>,
) {
    if let Some(device) = device {
        shown.silent = Some(format!("the device: {device}"));
        if shown.sounding.is_some() {
            shown.by_clock();
        }
    }
    if muted && shown.sounding.is_some() {
        shown.by_clock();
    }
    let playing = shown.playing.is_some() || shown.sounding.is_some();
    if !playing {
        shown.sounding = None;
        return;
    }
    if shown.sounding.is_none() && !muted && shown.silent.is_none() {
        start_sounding(shown, gain, failed);
    }
    let Some(sounding) = &mut shown.sounding else {
        if let Some((since, from)) = shown.playing {
            shown.at = from + since.elapsed().as_secs_f64() * shown.rate;
        }
        stop_at_end(shown);
        return;
    };
    // Fed ahead of the picture.
    let Opening::Open { sound, .. } = &shown.opening else { return };
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "an item's place")]
    let ahead = shown.at.max(0.0) as u64 + FEED;
    let mut ended = false;
    while sounding.fed <= ahead {
        match sound.get(sounding.fed) {
            Poll::Ready(item) => {
                let heard = parts(&item).1;
                let mut queue = sounding.queue.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(heard) = heard.filter(|h| h.rate > 0) {
                    if sounding.rate == 0 {
                        sounding.rate = heard.rate;
                        sounding.source_rate.store(heard.rate, Ordering::Relaxed);
                        queue.channels = heard.channels;
                    }
                    sounding.spans.push_back((sounding.fed, sounding.total));
                    heard.extend(&mut queue.samples);
                    #[expect(clippy::cast_precision_loss, reason = "samples of a sound")]
                    let frames = (heard.len() / queue.channels) as f64;
                    sounding.total += frames;
                } else {
                    sounding.spans.push_back((sounding.fed, sounding.total));
                }
                sounding.fed += 1;
            }
            Poll::End => {
                ended = true;
                break;
            }
            Poll::Pending | Poll::Failed(_) => break,
        }
    }
    // It sounds once what is fed carries sound; none in the first items fed - made, not waited for - it has none.
    if !sounding.started {
        if sounding.total > 0.0 {
            sounding.started = true;
        } else if sounding.spans.len() as u64 > FEED {
            shown.silent = Some("it has none".to_string());
            shown.by_clock();
        }
        return;
    }
    // Where it is: the item whose sound is playing - its sound being what led up to it.
    // Items fed without sound begin where the next does: the place stays on the last that sounded.
    let played = sounding.clock.now(f64::from(sounding.rate)).min(sounding.total);
    while sounding.spans.len() > 1 && sounding.spans[1].1 < played {
        sounding.spans.pop_front();
    }
    if let Some(&(item, start)) = sounding.spans.front() {
        let end = sounding.spans.get(1).map_or(sounding.total, |s| s.1);
        let into = if end > start { ((played - start) / (end - start)).clamp(0.0, 1.0) } else { 0.0 };
        #[expect(clippy::cast_precision_loss, reason = "an item's place")]
        let at = (item as f64 - 1.0 + into).max(0.0);
        // Never back: a buffer may begin a little before where the time since the last put it.
        shown.at = shown.at.max(at);
    }
    // The sound has ended before the items: the clock goes on.
    let empty = sounding.queue.lock().unwrap_or_else(PoisonError::into_inner).samples.is_empty();
    #[expect(clippy::cast_precision_loss, reason = "an item's place")]
    let fed_past = sounding.fed as f64 > shown.at + 1.0;
    if empty && (ended || played >= sounding.total) && fed_past {
        shown.silent = Some("its sound has ended".to_string());
        shown.by_clock();
    }
    stop_at_end(shown);
}

/// Stop at its last item.
fn stop_at_end(shown: &mut Shown) {
    #[expect(clippy::cast_precision_loss, reason = "an item's place")]
    let last = shown.last() as f64;
    if shown.at >= last {
        shown.at = last;
        shown.playing = None;
        shown.sounding = None;
    }
}

/// Play or pause, where it is (an item), and the time; the sound's controls.
fn controls(ui: &mut egui::Ui, shown: &mut Shown, number: u64, view: &mut Preview) {
    let last = shown.last();
    ui.horizontal(|ui| {
        let playing = shown.playing.is_some() || shown.sounding.is_some() || shown.held;
        let (glyph, says) = if playing { (icons::PAUSE, "Pause") } else { (icons::PLAY, "Play") };
        if ui.button(RichText::new(glyph).size(16.0)).on_hover_text(says).clicked() {
            if playing {
                shown.held = false;
                shown.playing = None;
                shown.sounding = None;
            } else {
                // From the start again where it ended.
                if number >= last {
                    shown.at = 0.0;
                }
                shown.silent = shown.silent.take().filter(|s| s.starts_with("it has none"));
                shown.by_clock();
            }
        }
        // From the pane's edge inward, so that these are always whole.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().slider_width = 70.0;
            ui.add_enabled_ui(!view.muted, |ui| {
                let percent = (view.volume * 100.0).round();
                ui.add(egui::Slider::new(&mut view.volume, 0.0..=1.0).step_by(0.05).show_value(false))
                    .on_hover_text(format!("Volume {percent}%"));
            });
            let (glyph, says) =
                if view.muted { (icons::SPEAKER_X, "Sound off") } else { (icons::SPEAKER_HIGH, "Sound on") };
            if ui.button(RichText::new(glyph).size(16.0)).on_hover_text(says).clicked() {
                view.muted = !view.muted;
            }
            ui.add_space(8.0);
            let rate = shown.rate;
            let time = |item: u64| {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss,
                    reason = "seconds of a stream"
                )]
                let seconds = (item as f64 / rate) as u64;
                format!("{}:{:02}", seconds / 60, seconds % 60)
            };
            let total = if shown.count.is_some() { time(last) } else { "…".to_string() };
            ui.label(RichText::new(format!("{} / {total}", time(number))).monospace());
            // The timeline takes what they leave.
            let mut chosen = number;
            ui.spacing_mut().slider_width = (ui.available_width() - ui.spacing().item_spacing.x).max(80.0);
            let slider = ui.add(egui::Slider::new(&mut chosen, 0..=last.min(u64::from(u32::MAX))).show_value(false));
            // Held, it stops where it is held - picture and sound - and plays on from there when let go, if it played.
            if slider.is_pointer_button_down_on() || slider.dragged() {
                if shown.playing.take().is_some() | shown.sounding.take().is_some() {
                    shown.held = true;
                }
            } else if std::mem::take(&mut shown.held) {
                shown.by_clock();
            }
            if slider.changed() {
                #[expect(clippy::cast_precision_loss, reason = "an item's place")]
                let chosen = chosen as f64;
                shown.at = chosen;
                shown.silent = shown.silent.take().filter(|s| s.starts_with("it has none"));
                if shown.playing.is_some() || shown.sounding.is_some() {
                    shown.by_clock();
                }
            }
        });
    });
    let of = shown.count.map_or_else(|| "…".to_string(), |c| format!("{}", shown.first + c.saturating_sub(1)));
    ui.label(
        RichText::new(format!("Item {} of {}–{of}  ·  {:.3} a second", shown.first + number, shown.first, shown.rate))
            .small()
            .color(theme::weak()),
    );
    if let Some(failed) = &shown.failed {
        ui.label(RichText::new(failed).small().color(theme::weak()));
    }
    let sound = match (&shown.sounding, &shown.silent) {
        (Some(s), _) if s.started => Some(format!("Sound: {} Hz", s.rate)),
        (_, Some(why)) => Some(format!("No sound: {why}")),
        _ => None,
    };
    if let Some(sound) = sound {
        ui.label(RichText::new(sound).small().color(theme::weak()));
    }
}

/// Sound on the default device, played from a queue fed as items are read, resampled to the device's rate: the
/// device's stream, the queue, where it is, and the rate the queue's samples come at (none until the first are fed).
type Opened = (cpal::Stream, Arc<Mutex<Queue>>, Arc<SoundClock>, Arc<AtomicU32>);

fn sound(gain: &Arc<AtomicU32>, failed: Arc<Mutex<Option<String>>>) -> Result<Opened, String> {
    let device = output_device().ok_or("no sound device")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let queue = Arc::new(Mutex::new(Queue { samples: VecDeque::new(), channels: 2 }));
    let clock = Arc::new(SoundClock::new());
    // The rate the queue's samples come at: told by the first fed, which the callback reads each buffer.
    let rate = Arc::new(AtomicU32::new(0));
    let given = (queue.clone(), gain.clone(), clock.clone(), rate.clone());
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => stream::<f32>(&device, &config.into(), given, failed),
        cpal::SampleFormat::I16 => stream::<i16>(&device, &config.into(), given, failed),
        cpal::SampleFormat::U16 => stream::<u16>(&device, &config.into(), given, failed),
        other => return Err(format!("the device takes {other} samples")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, queue, clock, rate))
}

/// The device sound goes to: `PulseAudio`'s, where ALSA has one - where a desktop's default goes, and the only way out
/// where ALSA has no card of its own (WSL) - else the default.
fn output_device() -> Option<cpal::Device> {
    let host = cpal::default_host();
    let pulse =
        host.output_devices().ok().and_then(|mut devices| devices.find(|d| d.name().is_ok_and(|n| n == "pulse")));
    pulse.or_else(|| host.default_output_device())
}

type Given = (Arc<Mutex<Queue>>, Arc<AtomicU32>, Arc<SoundClock>, Arc<AtomicU32>);

fn stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    (queue, gain, clock, rate): Given,
    failed: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels);
    let device_rate = f64::from(config.sample_rate.0);
    // The samples taken from the queue, a fraction into the next, and the one held.
    let mut taken = 0.0f64;
    let mut phase = 0.0f64;
    let mut held = [0.0f32; 2];
    let output = move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
        let mut queue = queue.lock().unwrap_or_else(PoisonError::into_inner);
        // Nothing taken until the queue's rate is told.
        let step = f64::from(rate.load(Ordering::Relaxed)) / device_rate;
        #[expect(clippy::cast_precision_loss, reason = "a buffer's samples")]
        clock.took(taken, (out.len() / channels) as f64 * step);
        let gain = f32::from_bits(gain.load(Ordering::Relaxed));
        let source = queue.channels.max(1);
        for frame in out.chunks_mut(channels) {
            phase += step;
            while phase >= 1.0 && queue.samples.len() >= source {
                let samples: Vec<i16> = queue.samples.drain(..source).collect();
                held = [f32::from(samples[0]) / 32768.0, f32::from(samples[source.min(2) - 1]) / 32768.0];
                phase -= 1.0;
                taken += 1.0;
            }
            if phase >= 1.0 {
                // Starved: silence, and the place holds.
                held = [0.0; 2];
                phase = 0.0;
            }
            for (c, out) in frame.iter_mut().enumerate() {
                *out = T::from_sample(if c < 2 { held[c] * gain } else { 0.0 });
            }
        }
    };
    let error = move |e: cpal::StreamError| {
        *failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
    };
    device.build_output_stream(config, output, error, None).map_err(|e| e.to_string())
}
