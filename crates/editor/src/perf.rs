//! What the editor costs, as it runs: sampled once a second off the window's thread, for the overlay and for agents
//! (`editor_performance`). Sampling shows nothing by itself: the overlay asks for a frame a second only while it is on.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use serde::Serialize;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

const SECOND: Duration = Duration::from_secs(1);
/// A source can be a program to start, not a counter to read.
const GPU_EVERY: Duration = Duration::from_secs(3);
/// How long after an agent's last question the GPU stays sampled.
const ASKED_FOR: Duration = Duration::from_secs(30);
/// The overlay's type: a corner's worth, at any zoom it is read in.
const TEXT: f32 = 9.0;

#[derive(Clone, Default, Serialize)]
pub struct Sample {
    /// Frames shown in the last second.
    pub frames_per_second: u32,
    /// What a frame took on the window's thread, its copy to the screen included, the wait for the display not.
    pub frame_ms: f32,
    /// Of one core, this process: 100 is one core's whole time.
    pub cpu_percent: Option<f32>,
    /// Of all of them, the machine.
    pub system_cpu_percent: Option<f32>,
    pub cores: usize,
    pub memory_mib: Option<f32>,
    pub gpu_percent: Option<u32>,
    pub gpu_memory_mib: Option<u32>,
    pub gpu_memory_total_mib: Option<u32>,
    /// What these three came from.
    pub gpu_source: Option<&'static str>,
    /// Whether they are this process's use alone rather than the GPU's.
    pub gpu_is_own_use: bool,
    /// The adapter drawing and how: `Microsoft Direct3D12 (...) (Vulkan)`.
    pub renderer: Option<String>,
    pub window: Option<[u32; 2]>,
}

struct Shared {
    frames: AtomicU64,
    busy_micros: AtomicU64,
    /// Whether the GPU is worth asking about: the overlay on.
    shown: AtomicBool,
    /// The adapter drawing's PCI vendor: which of a machine's GPUs to read.
    vendor: AtomicU32,
    asked: Mutex<Option<Instant>>,
    sample: Mutex<Sample>,
}

fn shared() -> &'static Arc<Shared> {
    static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
    SHARED.get_or_init(|| {
        let shared = Arc::new(Shared {
            frames: AtomicU64::new(0),
            busy_micros: AtomicU64::new(0),
            shown: AtomicBool::new(false),
            vendor: AtomicU32::new(0),
            asked: Mutex::new(None),
            sample: Mutex::new(Sample::default()),
        });
        let sampling = shared.clone();
        let started = std::thread::Builder::new().name("perf".to_string()).spawn(move || sample_forever(&sampling));
        if let Err(e) = started {
            eprintln!("the performance sampler: {e}");
        }
        shared
    })
}

/// A frame reached the window; `seconds`: what the last one took.
pub fn frame(seconds: Option<f32>, pixels: [u32; 2]) {
    let shared = shared();
    shared.frames.fetch_add(1, Ordering::Relaxed);
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "microseconds of a frame")]
    shared.busy_micros.fetch_add((seconds.unwrap_or(0.0) * 1e6) as u64, Ordering::Relaxed);
    shared.sample.lock().unwrap_or_else(PoisonError::into_inner).window = Some(pixels);
}

pub fn note_renderer(name: String, vendor: u32) {
    let shared = shared();
    shared.vendor.store(vendor, Ordering::Relaxed);
    shared.sample.lock().unwrap_or_else(PoisonError::into_inner).renderer = Some(name);
}

/// The last sample, for an agent, which keeps the GPU sampled for a while.
#[must_use]
pub fn read() -> serde_json::Value {
    let shared = shared();
    *shared.asked.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    let sample = shared.sample.lock().unwrap_or_else(PoisonError::into_inner).clone();
    serde_json::to_value(sample).unwrap_or_default()
}

fn sample_forever(shared: &Shared) {
    // Without either, the rest is still worth having.
    let mut system = System::new().ok();
    let own = sysinfo::get_current_pid().ok();
    let mut gpu = crate::gpu_stats::Gpu::new();
    let mut gpu_at: Option<Instant> = None;
    let mut last = Instant::now();
    loop {
        std::thread::sleep(SECOND);
        let elapsed = last.elapsed().as_secs_f32();
        last = Instant::now();
        let frames = shared.frames.swap(0, Ordering::Relaxed);
        let busy = shared.busy_micros.swap(0, Ordering::Relaxed);
        if let Some(system) = &mut system {
            system.refresh_cpu_usage();
            if let Some(own) = own {
                let kind = ProcessRefreshKind::nothing().with_cpu().with_memory();
                system.refresh_processes_specifics(ProcessesToUpdate::Some(&[own]), true, kind);
            }
        }
        let process = system.as_ref().zip(own).and_then(|(system, own)| system.process(own));
        let asked = shared.asked.lock().unwrap_or_else(PoisonError::into_inner).is_some_and(|at| at.elapsed() < ASKED_FOR);
        let wants_gpu = (asked || shared.shown.load(Ordering::Relaxed)) && gpu_at.is_none_or(|at| at.elapsed() >= GPU_EVERY);
        let reading = wants_gpu.then(|| gpu.read(shared.vendor.load(Ordering::Relaxed))).flatten();
        if wants_gpu {
            gpu_at = Some(Instant::now());
        }
        let mut sample = shared.sample.lock().unwrap_or_else(PoisonError::into_inner);
        #[expect(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a rate of a second")]
        {
            sample.frames_per_second = (frames as f32 / elapsed).round() as u32;
            sample.frame_ms = if frames == 0 { 0.0 } else { busy as f32 / frames as f32 / 1000.0 };
            sample.memory_mib = process.map(|process| process.memory() as f32 / (1024.0 * 1024.0));
        }
        sample.cpu_percent = process.map(sysinfo::Process::cpu_usage);
        sample.system_cpu_percent = system.as_ref().map(System::global_cpu_usage);
        sample.cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        if let Some(reading) = reading {
            sample.gpu_percent = reading.percent;
            sample.gpu_memory_mib = reading.memory_mib;
            sample.gpu_memory_total_mib = reading.memory_total_mib;
            sample.gpu_source = Some(reading.source);
            sample.gpu_is_own_use = reading.own;
        }
    }
}

/// The overlay, in a corner above the status bar: it takes nothing from what is under it.
pub fn overlay(ctx: &egui::Context) {
    let shared = shared();
    shared.shown.store(true, Ordering::Relaxed);
    ctx.request_repaint_after(SECOND);
    let sample = shared.sample.lock().unwrap_or_else(PoisonError::into_inner).clone();
    egui::Area::new(egui::Id::unique("performance overlay"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-10.0, -34.0))
        .interactable(false)
        .show(ctx, |ui| {
            let frame = egui::Frame::NONE
                .fill(egui::Color32::from_black_alpha(170))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(8, 5));
            frame.show(ui, |ui| {
                egui::Grid::new("performance rows").num_columns(2).spacing([10.0, 1.0]).show(ui, |ui| {
                    for (name, value) in rows(&sample) {
                        ui.label(RichText::new(name).size(TEXT).color(egui::Color32::from_gray(170)));
                        ui.label(RichText::new(value).size(TEXT).color(egui::Color32::from_gray(235)));
                        ui.end_row();
                    }
                });
            });
        });
}

pub fn overlay_hidden() {
    shared().shown.store(false, Ordering::Relaxed);
}

fn rows(sample: &Sample) -> Vec<(&'static str, String)> {
    let known = |value: Option<String>| value.unwrap_or_else(|| "n/a".to_string());
    let cpu = sample.cpu_percent.zip(sample.system_cpu_percent);
    let mut rows = vec![
        ("Frames", format!("{} a second, {:.1} ms", sample.frames_per_second, sample.frame_ms)),
        ("CPU", known(cpu.map(|(own, all)| format!("{own:.0}% of a core, all {all:.0}%")))),
        ("Memory", known(sample.memory_mib.map(|mib| format!("{mib:.0} MiB")))),
        ("GPU", known(gpu_row(sample))),
    ];
    if let Some(renderer) = &sample.renderer {
        rows.push(("Draws on", short_renderer(renderer)));
    }
    rows
}

/// As much as the source could tell: `32%, 3437 of 24576 MiB`, or `32%, 3437 MiB`, or `32%`.
fn gpu_row(sample: &Sample) -> Option<String> {
    let memory = match (sample.gpu_memory_mib, sample.gpu_memory_total_mib) {
        (Some(used), Some(total)) => Some(format!("{used} of {total} MiB")),
        (Some(used), None) => Some(format!("{used} MiB")),
        _ => None,
    };
    let parts: Vec<String> = sample.gpu_percent.map(|percent| format!("{percent}%")).into_iter().chain(memory).collect();
    (!parts.is_empty()).then(|| parts.join(", ") + if sample.gpu_is_own_use { ", the editor's" } else { "" })
}

/// `RTX 3090 (Vulkan)` of `Microsoft Direct3D12 (NVIDIA GeForce RTX 3090) (Vulkan)`: the overlay is a small thing.
fn short_renderer(renderer: &str) -> String {
    let Some((name, backend)) = renderer.rsplit_once(" (") else { return renderer.to_string() };
    let device = name.rsplit_once('(').map_or(name, |(_, device)| device.trim_end_matches(')'));
    let device = device.strip_prefix("NVIDIA GeForce ").unwrap_or(device);
    format!("{device} ({backend}")
}
