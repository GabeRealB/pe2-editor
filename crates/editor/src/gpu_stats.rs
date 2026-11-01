//! What the GPU is doing. `sysinfo` reads what each platform has, and two things here fill in what it does not reach: under
//! WSL the GPU is no device it finds, where NVIDIA's tool still answers, and on Linux a driver that gives no total may still
//! keep what this process's own connection to the GPU has used (`fdinfo`). What cannot be told is left out: a source
//! without a total just has none.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use sysinfo::Gpus;

const AMD: u32 = 0x1002;
const INTEL: u32 = 0x8086;
const NVIDIA: u32 = 0x10DE;
const MIB: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub percent: Option<u32>,
    pub memory_mib: Option<u32>,
    pub memory_total_mib: Option<u32>,
    /// Where it comes from.
    pub source: &'static str,
    /// Whether it is this process's use alone, the GPU's being what nothing here can read.
    pub own: bool,
}

/// Reads the GPU, keeping what a rate needs between readings.
pub struct Gpu {
    adapters: Option<Gpus>,
    fdinfo: FdInfo,
}

impl Gpu {
    pub fn new() -> Gpu {
        Gpu { adapters: Gpus::new().ok(), fdinfo: FdInfo::default() }
    }

    /// `vendor`: the PCI vendor of the adapter drawing, which of a machine's GPUs is the one to read; zero if unknown.
    pub fn read(&mut self, vendor: u32) -> Option<Reading> {
        if let Some(adapters) = &mut self.adapters {
            adapters.refresh(true);
            let drawing = adapters.iter().filter(|adapter| adapter.vendor().is_none_or(|name| of_vendor(vendor, name)));
            if let Some(reading) = at_work(drawing.map(reading_of)) {
                return Some(reading);
            }
        }
        if !cfg!(target_os = "macos")
            && !matches!(vendor, AMD | INTEL)
            && let Some(reading) = nvidia_smi()
        {
            return Some(reading);
        }
        if cfg!(target_os = "linux") { self.fdinfo.read() } else { None }
    }
}

/// Whether an adapter of this name is of a PCI vendor: `sysinfo` knows vendors by name. An unknown one could be any.
fn of_vendor(vendor: u32, name: &str) -> bool {
    let name = name.to_lowercase();
    match vendor {
        AMD => name.contains("amd") || name.contains("advanced micro"),
        INTEL => name.contains("intel"),
        NVIDIA => name.contains("nvidia"),
        _ => true,
    }
}

fn reading_of(adapter: &sysinfo::Gpu) -> Reading {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to a percent")]
    let percent = adapter.usage().map(|percent| percent.round().clamp(0.0, 100.0) as u32);
    let mib = |bytes: Option<u64>| bytes.and_then(|bytes| u32::try_from(bytes / MIB).ok());
    Reading {
        percent,
        memory_mib: mib(adapter.used_memory()),
        memory_total_mib: mib(adapter.total_memory()),
        source: "sysinfo",
        own: false,
    }
}

/// Of what the adapters say, the busiest: the one at work. One saying nothing is no reading.
fn at_work(readings: impl Iterator<Item = Reading>) -> Option<Reading> {
    readings.filter(|reading| reading.percent.is_some() || reading.memory_mib.is_some()).max_by_key(|reading| reading.percent)
}

fn nvidia_smi() -> Option<Reading> {
    let mut command = std::process::Command::new("nvidia-smi");
    command.args(["--query-gpu=utilization.gpu,memory.used,memory.total", "--format=csv,noheader,nounits"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    parse_nvidia_smi(&String::from_utf8(command.output().ok()?.stdout).ok()?)
}

fn parse_nvidia_smi(text: &str) -> Option<Reading> {
    let mut numbers = text.lines().next()?.split(',').map(|n| n.trim().parse::<u32>().ok());
    let (percent, used, total) = (numbers.next()??, numbers.next()??, numbers.next()??);
    Some(Reading { percent: Some(percent), memory_mib: Some(used), memory_total_mib: Some(total), source: "nvidia-smi", own: false })
}

/// One connection to a GPU, as `/proc/<pid>/fdinfo` tells of it (the kernel's `drm-usage-stats`).
#[derive(Debug, Default, PartialEq)]
struct Client {
    id: String,
    /// By engine: what it has been busy for, and out of what, where it counts cycles rather than nanoseconds.
    engines: HashMap<String, (u64, Option<u64>)>,
    /// In KiB, by region.
    memory: HashMap<String, u64>,
}

fn parse_fdinfo(text: &str) -> Option<Client> {
    let mut client = Client::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let mut words = value.split_whitespace();
        let (number, unit) = (words.next(), words.next());
        if key == "drm-client-id" {
            client.id = number?.to_string();
        }
        let number = number.and_then(|n| n.parse::<u64>().ok());
        // Cycles first: `drm-total-cycles-` would otherwise read as a memory region.
        if let (Some(engine), Some(total)) = (key.strip_prefix("drm-total-cycles-"), number) {
            client.engines.entry(engine.to_string()).or_default().1 = Some(total);
        } else if let (Some(engine), Some(busy)) = (key.strip_prefix("drm-cycles-"), number) {
            client.engines.entry(engine.to_string()).or_default().0 = busy;
        } else if let (Some(engine), Some(busy)) = (key.strip_prefix("drm-engine-"), number) {
            client.engines.entry(engine.to_string()).or_default().0 = busy;
        } else if let (Some(region), Some(size)) = (key.strip_prefix("drm-total-"), number) {
            let kib = match unit {
                Some("KiB") => size,
                Some("MiB") => size * 1024,
                _ => size / 1024,
            };
            client.memory.insert(region.to_string(), kib);
        }
    }
    (!client.id.is_empty()).then_some(client)
}

/// By client and engine, what each has been busy for, out of what.
type Counts = HashMap<(String, String), (u64, Option<u64>)>;

/// This process's connections, one reading to the next: a rate is what changed between two.
#[derive(Default)]
struct FdInfo {
    last: Option<(Instant, Counts)>,
}

impl FdInfo {
    fn read(&mut self) -> Option<Reading> {
        let mut clients: HashMap<String, Client> = HashMap::new();
        for entry in std::fs::read_dir("/proc/self/fdinfo").ok()?.flatten() {
            let is_gpu = std::fs::read_link(Path::new("/proc/self/fd").join(entry.file_name())).is_ok_and(|to| to.starts_with("/dev/dri"));
            // A connection opened twice is one client.
            if is_gpu && let Some(client) = std::fs::read_to_string(entry.path()).ok().as_deref().and_then(parse_fdinfo) {
                clients.insert(client.id.clone(), client);
            }
        }
        if clients.is_empty() {
            return None;
        }
        let now = Instant::now();
        let counts: HashMap<_, _> = clients
            .values()
            .flat_map(|client| client.engines.iter().map(|(engine, &count)| ((client.id.clone(), engine.clone()), count)))
            .collect();
        let percent = self.last.as_ref().map(|(then, before)| busiest(before, &counts, now.duration_since(*then).as_secs_f64()));
        self.last = Some((now, counts));
        let memory_mib = u32::try_from(video_memory_kib(clients.values()) / 1024).ok();
        Some(Reading { percent, memory_mib, memory_total_mib: None, source: "drm-fdinfo", own: true })
    }
}

/// The percent of its time the busiest engine had, summed over the clients on it.
fn busiest(before: &Counts, now: &Counts, seconds: f64) -> u32 {
    let mut by_engine: HashMap<&str, f64> = HashMap::new();
    for (key, &(busy, total)) in now {
        let Some(&(busy_before, total_before)) = before.get(key) else { continue };
        #[expect(clippy::cast_precision_loss, reason = "a share of a few seconds")]
        let share = match total.zip(total_before) {
            Some((total, total_before)) if total > total_before => busy.saturating_sub(busy_before) as f64 / (total - total_before) as f64,
            None if seconds > 0.0 => busy.saturating_sub(busy_before) as f64 / (seconds * 1e9),
            _ => 0.0,
        };
        *by_engine.entry(&key.1).or_default() += share;
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to a percent")]
    let percent = (by_engine.values().fold(0.0, |most: f64, &share| most.max(share)) * 100.0).round().clamp(0.0, 100.0) as u32;
    percent
}

/// What is the GPU's own memory where a driver tells it apart from the system's, which it may hold as well.
fn video_memory_kib<'a>(clients: impl Iterator<Item = &'a Client>) -> u64 {
    let clients: Vec<_> = clients.collect();
    let sum = |local: bool| -> u64 {
        let local_region = |region: &str| region.starts_with("vram") || region.starts_with("local");
        clients.iter().flat_map(|client| &client.memory).filter(|(region, _)| !local || local_region(region)).map(|(_, kib)| kib).sum()
    };
    match sum(true) {
        0 => sum(false),
        local => local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_smi_gives_all_three() {
        let reading = parse_nvidia_smi("32, 3437, 24576\n").unwrap();
        assert_eq!((reading.percent, reading.memory_mib, reading.memory_total_mib), (Some(32), Some(3437), Some(24576)));
        assert_eq!(parse_nvidia_smi("[N/A], 3437, 24576\n"), None);
        assert_eq!(parse_nvidia_smi(""), None);
    }

    #[test]
    fn an_adapter_is_told_by_its_vendors_name() {
        assert!(of_vendor(AMD, "Advanced Micro Devices, Inc. [AMD/ATI]"));
        assert!(of_vendor(NVIDIA, "NVIDIA Corporation"));
        assert!(of_vendor(INTEL, "Intel Corporation"));
        assert!(!of_vendor(INTEL, "NVIDIA Corporation"));
        assert!(of_vendor(0, "Apple"));
    }

    #[test]
    fn the_adapter_at_work_is_the_one_read() {
        let reading = |percent, memory_mib| Reading { percent, memory_mib, ..Reading::default() };
        let idle = reading(Some(2), Some(100));
        let busy = reading(Some(40), None);
        assert_eq!(at_work([idle, busy, reading(None, None)].into_iter()), Some(busy));
        assert_eq!(at_work([reading(None, Some(100))].into_iter()), Some(reading(None, Some(100))));
        assert_eq!(at_work([reading(None, None)].into_iter()), None);
    }

    #[test]
    fn fdinfo_in_nanoseconds_and_in_cycles() {
        let amd = "pos:\t0\nflags:\t02100002\ndrm-driver:\tamdgpu\ndrm-client-id:\t12\ndrm-engine-gfx:\t1000 ns\n\
                   drm-total-vram:\t2048 MiB\ndrm-total-gtt:\t512 KiB\n";
        let client = parse_fdinfo(amd).unwrap();
        assert_eq!(client.id, "12");
        assert_eq!(client.engines["gfx"], (1000, None));
        assert_eq!(client.memory["vram"], 2048 * 1024);
        let xe = "drm-driver:\txe\ndrm-client-id:\t3\ndrm-cycles-rcs:\t500\ndrm-total-cycles-rcs:\t2000\ndrm-total-system:\t64 KiB\n";
        let client = parse_fdinfo(xe).unwrap();
        assert_eq!(client.engines["rcs"], (500, Some(2000)));
        assert_eq!(client.memory.len(), 1, "a cycle count is not a memory region");
        assert_eq!(parse_fdinfo("pos:\t0\nflags:\t02100002\n"), None);
    }

    #[test]
    fn the_busiest_engine_is_the_rate() {
        let key = |client: &str, engine: &str| (client.to_string(), engine.to_string());
        let before = HashMap::from([(key("1", "gfx"), (0, None)), (key("2", "gfx"), (0, None)), (key("1", "video"), (0, None))]);
        let now = HashMap::from([
            (key("1", "gfx"), (250_000_000, None)),
            (key("2", "gfx"), (250_000_000, None)),
            (key("1", "video"), (100_000_000, None)),
        ]);
        assert_eq!(busiest(&before, &now, 1.0), 50);
        let before = HashMap::from([(key("1", "rcs"), (100, Some(1000)))]);
        let now = HashMap::from([(key("1", "rcs"), (400, Some(2000)))]);
        assert_eq!(busiest(&before, &now, 1.0), 30);
    }

    #[test]
    fn video_memory_is_preferred_over_the_system_kind() {
        let both = Client { memory: HashMap::from([("vram".to_string(), 4096), ("gtt".to_string(), 1024)]), ..Client::default() };
        assert_eq!(video_memory_kib([&both].into_iter()), 4096);
        let system = Client { memory: HashMap::from([("system".to_string(), 1024)]), ..Client::default() };
        assert_eq!(video_memory_kib([&system].into_iter()), 1024);
    }
}
