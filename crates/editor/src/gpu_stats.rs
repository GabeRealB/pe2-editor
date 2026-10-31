//! What the GPU is doing, from what each platform has: no one way reaches them all. Windows counts every vendor's engines
//! (PDH), Linux has a file per AMD card and, for any driver that keeps it, what this process's own connection to the GPU
//! has used (`fdinfo`), macOS has what `ioreg` prints, and NVIDIA's tool answers on Linux and under WSL, where nothing
//! else does. What cannot be told is left out: a source without a total just has none.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

const AMD: u32 = 0x1002;
const INTEL: u32 = 0x8086;
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
    fdinfo: FdInfo,
    #[cfg(windows)]
    pdh: Option<pdh::Query>,
}

impl Gpu {
    pub fn new() -> Gpu {
        Gpu {
            fdinfo: FdInfo::default(),
            #[cfg(windows)]
            pdh: pdh::Query::open(),
        }
    }

    /// `vendor`: the PCI vendor of the adapter drawing, which of a machine's GPUs is the one to read; zero if unknown.
    pub fn read(&mut self, vendor: u32) -> Option<Reading> {
        #[cfg(windows)]
        if let Some(reading) = self.pdh.as_mut().and_then(pdh::Query::read) {
            return Some(reading);
        }
        if cfg!(target_os = "macos") {
            return ioreg();
        }
        if !matches!(vendor, AMD | INTEL)
            && let Some(reading) = nvidia_smi()
        {
            return Some(reading);
        }
        if !cfg!(target_os = "linux") {
            return None;
        }
        if vendor != INTEL
            && let Some(reading) = amdgpu(Path::new("/sys/class/drm"))
        {
            return Some(reading);
        }
        self.fdinfo.read()
    }
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

/// The first AMD card under `drm`, `/sys/class/drm` but for tests.
fn amdgpu(drm: &Path) -> Option<Reading> {
    let mut cards: Vec<_> = std::fs::read_dir(drm).ok()?.flatten().map(|entry| entry.path()).collect();
    cards.sort();
    cards.iter().find_map(|card| {
        // `card0-HDMI-A-1` and the like are its connectors.
        let name = card.file_name()?.to_str()?;
        if !name.starts_with("card") || name.contains('-') {
            return None;
        }
        let device = card.join("device");
        let number = |file: &str| std::fs::read_to_string(device.join(file)).ok()?.trim().parse::<u64>().ok();
        let vendor = std::fs::read_to_string(device.join("vendor")).ok()?;
        if vendor.trim() != "0x1002" {
            return None;
        }
        let percent = u32::try_from(number("gpu_busy_percent")?).ok();
        let mib = |file| number(file).and_then(|bytes| u32::try_from(bytes / MIB).ok());
        Some(Reading { percent, memory_mib: mib("mem_info_vram_used"), memory_total_mib: mib("mem_info_vram_total"), source: "amdgpu", own: false })
    })
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

fn ioreg() -> Option<Reading> {
    let out = std::process::Command::new("ioreg").args(["-r", "-d", "1", "-w", "0", "-c", "IOAccelerator"]).output().ok()?;
    parse_ioreg(&String::from_utf8_lossy(&out.stdout))
}

fn parse_ioreg(text: &str) -> Option<Reading> {
    let number = |key: &str| -> Option<u64> {
        let (_, rest) = text.split_once(&format!("\"{key}\"="))?;
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        rest[..end].parse().ok()
    };
    let percent = number("Device Utilization %").or_else(|| number("GPU Activity(%)")).and_then(|n| u32::try_from(n).ok());
    let memory_mib = number("In use system memory").and_then(|bytes| u32::try_from(bytes / MIB).ok());
    (percent.is_some() || memory_mib.is_some()).then_some(Reading { percent, memory_mib, memory_total_mib: None, source: "ioreg", own: false })
}

/// One adapter's and one engine's part of a PDH instance name:
/// `pid_4242_luid_0x00000000_0x0000C3A5_phys_0_eng_0_engtype_3D`, or for memory `luid_0x00000000_0x0000C3A5_phys_0`.
#[cfg(any(windows, test))]
fn pdh_adapter(name: &str) -> Option<(&str, &str)> {
    let from_adapter = &name[name.find("luid_")?..];
    let adapter = from_adapter.split_once("_eng_").map_or(from_adapter, |(adapter, _)| adapter);
    Some((adapter, from_adapter))
}

/// From each process's use of each engine, what Task Manager shows: an engine's use is the sum of its processes', an
/// adapter's that of its busiest engine; and the adapter to read is the one at work. Its use, and which it is.
#[cfg(any(windows, test))]
fn pdh_busiest<'a>(engines: impl Iterator<Item = (&'a str, f64)>) -> Option<(String, u32)> {
    let mut by_engine: HashMap<(&str, &str), f64> = HashMap::new();
    for (name, percent) in engines {
        if let Some(key) = pdh_adapter(name) {
            *by_engine.entry(key).or_default() += percent;
        }
    }
    let ((adapter, _), percent) = by_engine.into_iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to a percent")]
    Some((adapter.to_string(), percent.round().clamp(0.0, 100.0) as u32))
}

#[cfg(windows)]
mod pdh {
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE,
        PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
        PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    };

    use super::{MIB, Reading};

    pub struct Query {
        handle: PDH_HQUERY,
        engines: PDH_HCOUNTER,
        memory: PDH_HCOUNTER,
    }

    impl Drop for Query {
        fn drop(&mut self) {
            // SAFETY: the query is ours, opened in `open`, and closed once.
            unsafe { PdhCloseQuery(self.handle) };
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn name_of(item: &PDH_FMT_COUNTERVALUE_ITEM_W) -> String {
        let name = item.szName;
        // SAFETY: PDH's names are terminated, and live in the buffer the item came in.
        unsafe {
            let mut length = 0;
            while *name.add(length) != 0 {
                length += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(name, length))
        }
    }

    impl Query {
        pub fn open() -> Option<Query> {
            let mut handle = null_mut();
            let engine_path = wide("\\GPU Engine(*)\\Utilization Percentage");
            let memory_path = wide("\\GPU Adapter Memory(*)\\Dedicated Usage");
            // SAFETY: the out-pointers are live for the calls, and the paths are terminated. Failing, `opened` closes.
            unsafe {
                if PdhOpenQueryW(null(), 0, &raw mut handle) != 0 {
                    return None;
                }
                let mut opened = Query { handle, engines: null_mut(), memory: null_mut() };
                if PdhAddEnglishCounterW(handle, engine_path.as_ptr(), 0, &raw mut opened.engines) != 0
                    || PdhAddEnglishCounterW(handle, memory_path.as_ptr(), 0, &raw mut opened.memory) != 0
                {
                    return None;
                }
                // A rate takes two collections: this is the first.
                PdhCollectQueryData(handle);
                Some(opened)
            }
        }

        /// Every instance of a counter, by name, those without a value now left out.
        fn values<T>(counter: PDH_HCOUNTER, format: PDH_FMT, value: impl Fn(&PDH_FMT_COUNTERVALUE_ITEM_W) -> T) -> Vec<(String, T)> {
            let (mut size, mut count) = (0u32, 0u32);
            // SAFETY: asks the size only, which a null buffer means.
            let asked = unsafe { PdhGetFormattedCounterArrayW(counter, format, &raw mut size, &raw mut count, null_mut()) };
            if asked != PDH_MORE_DATA {
                return Vec::new();
            }
            // In u64s for the items' alignment; their names are kept in the same buffer, after them.
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            let items = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            // SAFETY: the buffer holds the `size` bytes just asked for.
            if unsafe { PdhGetFormattedCounterArrayW(counter, format, &raw mut size, &raw mut count, items) } != 0 {
                return Vec::new();
            }
            // SAFETY: PDH wrote `count` items at the buffer's start.
            let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
            items
                .iter()
                .filter(|item| matches!(item.FmtValue.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA))
                .map(|item| (name_of(item), value(item)))
                .collect()
        }

        pub fn read(&mut self) -> Option<Reading> {
            // SAFETY: the query is open for as long as `self`.
            if unsafe { PdhCollectQueryData(self.handle) } != 0 {
                return None;
            }
            // SAFETY: each format here is what fills the union's field read.
            let engines = Self::values(self.engines, PDH_FMT_DOUBLE, |item| unsafe { item.FmtValue.Anonymous.doubleValue });
            let memory = Self::values(self.memory, PDH_FMT_LARGE, |item| unsafe { item.FmtValue.Anonymous.largeValue });
            let (adapter, percent) = super::pdh_busiest(engines.iter().map(|(name, percent)| (name.as_str(), *percent)))?;
            let used = memory.iter().find(|(name, _)| name.contains(&adapter)).map(|(_, bytes)| *bytes);
            let memory_mib = used.and_then(|bytes| u32::try_from(bytes.cast_unsigned() / MIB).ok());
            Some(Reading { percent: Some(percent), memory_mib, memory_total_mib: None, source: "pdh", own: false })
        }
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
    fn amdgpu_reads_the_amd_card_and_not_its_connectors() {
        let root = std::env::temp_dir().join(format!("pe2-gpu-stats-{}", std::process::id()));
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("card0/device/vendor", "0x8086\n");
        write("card0/device/gpu_busy_percent", "99\n");
        write("card1-HDMI-A-1/device/vendor", "0x1002\n");
        write("card1/device/vendor", "0x1002\n");
        write("card1/device/gpu_busy_percent", "41\n");
        write("card1/device/mem_info_vram_used", &format!("{}\n", 2048 * MIB));
        write("card1/device/mem_info_vram_total", &format!("{}\n", 8192 * MIB));
        let reading = amdgpu(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!((reading.percent, reading.memory_mib, reading.memory_total_mib), (Some(41), Some(2048), Some(8192)));
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

    #[test]
    fn ioreg_statistics() {
        let text = r#"+-o AGXAcceleratorG14X  <class AGXAcceleratorG14X>
    {
      "PerformanceStatistics" = {"Tiler Utilization %"=3,"In use system memory"=2147483648,"Device Utilization %"=12}
    }"#;
        let reading = parse_ioreg(text).unwrap();
        assert_eq!((reading.percent, reading.memory_mib), (Some(12), Some(2048)));
        assert_eq!(parse_ioreg("nothing here"), None);
    }

    #[test]
    fn pdh_adds_processes_and_takes_the_busiest_engine() {
        let dgpu = "luid_0x00000000_0x0000C3A5_phys_0";
        let igpu = "luid_0x00000000_0x0000AAAA_phys_0";
        let engines = [
            (format!("pid_1_{dgpu}_eng_0_engtype_3D"), 30.0),
            (format!("pid_2_{dgpu}_eng_0_engtype_3D"), 25.0),
            (format!("pid_1_{dgpu}_eng_1_engtype_Copy"), 40.0),
            (format!("pid_1_{igpu}_eng_0_engtype_3D"), 10.0),
        ];
        let (adapter, percent) = pdh_busiest(engines.iter().map(|(name, percent)| (name.as_str(), *percent))).unwrap();
        assert_eq!((adapter.as_str(), percent), (dgpu, 55));
    }
}
