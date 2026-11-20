//! System monitor — lightweight CPU & RAM usage sampling.
//!
//! Samples are taken once a second on a thread of their own: asking the
//! system — its processors, its memory, the GPU's I/O Registry entries and
//! every one of their clients — waits on it, and the show's thread, which
//! asked every second, must not. The latest values are exposed via simple
//! getters for the snapshot pipeline.

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::System;

/// How often to re-sample CPU/RAM (default: 1 second).
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// What one look at the system found.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Sample {
    /// CPU usage as a percentage (0–100), averaged across all cores.
    cpu_usage: f32,
    /// Total physical RAM in bytes.
    ram_total: u64,
    /// Used physical RAM in bytes.
    ram_used: u64,
    /// The GPU's busy share (0–100) as the OS reports it, where it does
    /// (macOS: see [`gpu_busy_now`]).
    gpu_busy: Option<f32>,
    /// Nida's own share of the GPU (0–100), as Activity Monitor reads it:
    /// its GPU time gone by since the last look, over the time gone by.
    nida_gpu: Option<f32>,
    /// Nida's own memory, in bytes (its physical footprint, as Activity
    /// Monitor's Memory column).
    nida_ram: Option<u64>,
}

/// The sampling thread's own: the system as last refreshed, and Nida's GPU
/// time at the last look.
struct Sampler {
    sys: System,
    /// Nida's GPU time so far (nanoseconds) by client counter, and when it
    /// was read.
    nida_gpu_time: Option<(GpuTimes, Instant)>,
}

impl Sampler {
    fn new() -> Self {
        Self {
            sys: System::new(),
            nida_gpu_time: None,
        }
    }

    /// Look at the system now.
    fn sample(&mut self) -> Sample {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let now = Instant::now();
        let mut nida_gpu = None;
        if let Some(times) = own_gpu_time() {
            if let Some((before, at)) = &self.nida_gpu_time {
                let spent = gpu_time_spent(before, &times) as f64;
                let gone = now.duration_since(*at).as_nanos().max(1) as f64;
                nida_gpu = Some(((spent / gone) * 100.0).clamp(0.0, 100.0) as f32);
            }
            self.nida_gpu_time = Some((times, now));
        }
        Sample {
            cpu_usage: SystemMonitor::avg_cpu(&self.sys),
            ram_total: self.sys.total_memory(),
            ram_used: self.sys.used_memory(),
            gpu_busy: gpu_busy_now(),
            nida_gpu,
            nida_ram: own_footprint(),
        }
    }
}

/// Lightweight system resource monitor.
///
/// Call `update()` every frame — it takes the thread's latest sample, if
/// it can without waiting, and never asks the system itself.
pub struct SystemMonitor {
    /// The latest sample taken in.
    latest: Sample,
    /// Where the thread puts each sample.
    shared: Arc<Mutex<Sample>>,
    /// Dropped with the monitor: the thread then ends.
    _stop: mpsc::Sender<()>,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Sample::default()));
        let (stop, stopped) = mpsc::channel::<()>();
        let into = Arc::clone(&shared);
        let started = std::thread::Builder::new()
            .name("system-monitor".into())
            .spawn(move || {
                let mut sampler = Sampler::new();
                loop {
                    let sample = sampler.sample();
                    if let Ok(mut shared) = into.lock() {
                        *shared = sample;
                    }
                    match stopped.recv_timeout(SAMPLE_INTERVAL) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
            });
        if let Err(e) = started {
            log::warn!("CPU, memory and GPU use are not watched: their thread did not start ({e})");
        }
        Self {
            latest: Sample::default(),
            shared,
            _stop: stop,
        }
    }

    /// Call every frame: takes the latest sample, never waiting for it.
    pub fn update(&mut self) {
        if let Ok(shared) = self.shared.try_lock() {
            self.latest = *shared;
        }
    }

    /// CPU usage % (0–100), averaged across all cores.
    pub fn cpu_usage(&self) -> f32 {
        self.latest.cpu_usage
    }

    /// The GPU's busy share (0–100) as the OS reports it; `None` where it
    /// does not.
    pub fn gpu_busy(&self) -> Option<f32> {
        self.latest.gpu_busy
    }

    /// Nida's own share of the GPU (0–100), as Activity Monitor reads it;
    /// `None` where the OS does not say.
    pub fn nida_gpu(&self) -> Option<f32> {
        self.latest.nida_gpu
    }

    /// Nida's own memory in bytes (its footprint); `None` where unknown.
    pub fn nida_ram(&self) -> Option<u64> {
        self.latest.nida_ram
    }

    /// Total physical RAM in bytes.
    pub fn ram_total(&self) -> u64 {
        self.latest.ram_total
    }

    /// Used physical RAM in bytes.
    pub fn ram_used(&self) -> u64 {
        self.latest.ram_used
    }

    /// RAM usage as a percentage (0–100).
    pub fn ram_usage_pct(&self) -> f32 {
        if self.latest.ram_total > 0 {
            (self.latest.ram_used as f64 / self.latest.ram_total as f64 * 100.0) as f32
        } else {
            0.0
        }
    }

    fn avg_cpu(sys: &System) -> f32 {
        let cpus = sys.cpus();
        if cpus.is_empty() {
            return 0.0;
        }
        cpus.iter().map(sysinfo::Cpu::cpu_usage).sum::<f32>() / cpus.len() as f32
    }
}

/// The GPU's busy share now, as macOS reports it: the accelerator's
/// "Device Utilization %" in the I/O Registry — what Activity Monitor-style
/// tools read. Nida's own estimate needs GPU timestamps, which Apple GPUs do
/// not give inside passes, and fell back to CPU time: it read 5 % on a
/// laptop whose GPU was 80 % busy and holding every frame up.
#[cfg(target_os = "macos")]
fn gpu_busy_now() -> Option<f32> {
    iokit::device_utilization()
}

#[cfg(not(target_os = "macos"))]
fn gpu_busy_now() -> Option<f32> {
    None
}

/// GPU time so far in nanoseconds, by counter: (the client's registry
/// entry, the counter's place in its list).
type GpuTimes = std::collections::HashMap<(u64, isize), u64>;

/// Nida's GPU time so far, by counter: the accelerators' per-client
/// `AppUsage` for this process, as Activity Monitor and `ioreg` read it.
#[cfg(target_os = "macos")]
fn own_gpu_time() -> Option<GpuTimes> {
    iokit::own_gpu_time(std::process::id())
}

#[cfg(not(target_os = "macos"))]
fn own_gpu_time() -> Option<GpuTimes> {
    None
}

/// The GPU time spent between two readings, counter by counter, over the
/// counters in both: a client gone since takes nothing away from the
/// others (Codex: summed, `[100 s, 20 s]` then `[100.2 s]` read 0 %); one
/// new, back after a reading missed it, or started again from nothing
/// counts from the next — its time so far is not all this reading's (a
/// 20 s counter back after a miss read 100 %).
fn gpu_time_spent(before: &GpuTimes, now: &GpuTimes) -> u64 {
    now.iter()
        .filter_map(|(counter, &time)| {
            let was = *before.get(counter)?;
            (time >= was).then(|| time - was)
        })
        .sum()
}

/// Nida's physical footprint in bytes: what Activity Monitor's Memory
/// column shows for it.
#[cfg(target_os = "macos")]
fn own_footprint() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    #[allow(clippy::cast_possible_wrap)]
    let pid = std::process::id() as libc::c_int;
    // SAFETY: our own pid; the buffer is a rusage_info_v2, as the flavour asks.
    let done = unsafe {
        libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast())
    };
    // SAFETY: filled in on success.
    (done == 0).then(|| unsafe { info.assume_init() }.ri_phys_footprint)
}

#[cfg(not(target_os = "macos"))]
fn own_footprint() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
mod iokit {
    use std::ffi::{c_char, c_void};

    type CFTypeRef = *const c_void;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_CF_NUMBER_SINT64_TYPE: isize = 4;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingServices(main_port: u32, matching: *mut c_void, iter: *mut u32) -> i32;
        fn IOIteratorNext(iter: u32) -> u32;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: CFTypeRef,
            allocator: CFTypeRef,
            options: u32,
        ) -> CFTypeRef;
        fn IOObjectRelease(object: u32) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, encoding: u32) -> CFTypeRef;
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> bool;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFDictionaryGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFRelease(cf: CFTypeRef);
    }

    /// The highest "Device Utilization %" among the machine's accelerators.
    pub(super) fn device_utilization() -> Option<f32> {
        // SAFETY: documented IOKit and CoreFoundation calls; every object
        // created or copied here is released once, and none is used after.
        unsafe {
            let matching = IOServiceMatching(c"IOAccelerator".as_ptr());
            if matching.is_null() {
                return None;
            }
            let mut iter = 0;
            // Takes the matching dictionary's reference.
            if IOServiceGetMatchingServices(0, matching, &raw mut iter) != 0 {
                return None;
            }
            let stats_key = cf_string(c"PerformanceStatistics");
            let busy_key = cf_string(c"Device Utilization %");
            let mut busiest: Option<f32> = None;
            loop {
                let service = IOIteratorNext(iter);
                if service == 0 {
                    break;
                }
                let stats = IORegistryEntryCreateCFProperty(service, stats_key, std::ptr::null(), 0);
                if !stats.is_null() {
                    if CFGetTypeID(stats) == CFDictionaryGetTypeID() {
                        let busy = CFDictionaryGetValue(stats, busy_key);
                        let mut value: i64 = 0;
                        if !busy.is_null()
                            && CFGetTypeID(busy) == CFNumberGetTypeID()
                            && CFNumberGetValue(busy, K_CF_NUMBER_SINT64_TYPE, (&raw mut value).cast())
                        {
                            #[allow(clippy::cast_precision_loss)]
                            let pct = (value as f32).clamp(0.0, 100.0);
                            busiest = Some(busiest.map_or(pct, |b| b.max(pct)));
                        }
                    }
                    CFRelease(stats);
                }
                IOObjectRelease(service);
            }
            IOObjectRelease(iter);
            CFRelease(stats_key);
            CFRelease(busy_key);
            busiest
        }
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IORegistryEntryGetChildIterator(entry: u32, plane: *const c_char, iter: *mut u32) -> i32;
        fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringGetCString(s: CFTypeRef, buffer: *mut c_char, size: isize, encoding: u32) -> bool;
        fn CFStringGetTypeID() -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, at: isize) -> CFTypeRef;
    }

    /// The GPU time so far of process `pid`, in nanoseconds, by counter:
    /// the `accumulatedGPUTime` of every `AppUsage` entry of the
    /// accelerators' user clients its `IOUserClientCreator` says are its
    /// ("pid N, …"), keyed by the client's registry entry and the entry's
    /// place.
    pub(super) fn own_gpu_time(pid: u32) -> Option<super::GpuTimes> {
        let mine = format!("pid {pid},");
        // SAFETY: documented IOKit and CoreFoundation calls; every object
        // created or copied here is released once, and none is used after.
        unsafe {
            let matching = IOServiceMatching(c"IOAccelerator".as_ptr());
            if matching.is_null() {
                return None;
            }
            let mut accelerators = 0;
            if IOServiceGetMatchingServices(0, matching, &raw mut accelerators) != 0 {
                return None;
            }
            let creator_key = cf_string(c"IOUserClientCreator");
            let usage_key = cf_string(c"AppUsage");
            let time_key = cf_string(c"accumulatedGPUTime");
            let mut total: Option<super::GpuTimes> = None;
            loop {
                let accelerator = IOIteratorNext(accelerators);
                if accelerator == 0 {
                    break;
                }
                let mut clients = 0;
                if IORegistryEntryGetChildIterator(accelerator, c"IOService".as_ptr(), &raw mut clients) == 0 {
                    loop {
                        let client = IOIteratorNext(clients);
                        if client == 0 {
                            break;
                        }
                        let creator = IORegistryEntryCreateCFProperty(client, creator_key, std::ptr::null(), 0);
                        let ours = !creator.is_null()
                            && CFGetTypeID(creator) == CFStringGetTypeID()
                            && {
                                let mut buffer = [0 as c_char; 256];
                                CFStringGetCString(creator, buffer.as_mut_ptr(), 256, K_CF_STRING_ENCODING_UTF8)
                                    && std::ffi::CStr::from_ptr(buffer.as_ptr())
                                        .to_string_lossy()
                                        .starts_with(&mine)
                            };
                        if !creator.is_null() {
                            CFRelease(creator);
                        }
                        let mut entry_id = 0_u64;
                        if ours && IORegistryEntryGetRegistryEntryID(client, &raw mut entry_id) == 0 {
                            let usage = IORegistryEntryCreateCFProperty(client, usage_key, std::ptr::null(), 0);
                            if !usage.is_null() {
                                if CFGetTypeID(usage) == CFArrayGetTypeID() {
                                    for at in 0..CFArrayGetCount(usage) {
                                        let entry = CFArrayGetValueAtIndex(usage, at);
                                        if entry.is_null() || CFGetTypeID(entry) != CFDictionaryGetTypeID() {
                                            continue;
                                        }
                                        let time = CFDictionaryGetValue(entry, time_key);
                                        let mut value: i64 = 0;
                                        if !time.is_null()
                                            && CFGetTypeID(time) == CFNumberGetTypeID()
                                            && CFNumberGetValue(time, K_CF_NUMBER_SINT64_TYPE, (&raw mut value).cast())
                                        {
                                            total
                                                .get_or_insert_with(Default::default)
                                                .insert((entry_id, at), u64::try_from(value).unwrap_or(0));
                                        }
                                    }
                                }
                                CFRelease(usage);
                            }
                        }
                        IOObjectRelease(client);
                    }
                    IOObjectRelease(clients);
                }
                IOObjectRelease(accelerator);
            }
            IOObjectRelease(accelerators);
            CFRelease(creator_key);
            CFRelease(usage_key);
            CFRelease(time_key);
            total
        }
    }

    unsafe fn cf_string(s: &std::ffi::CStr) -> CFTypeRef {
        // SAFETY: a NUL-terminated UTF-8 string; the caller releases it.
        unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), K_CF_STRING_ENCODING_UTF8) }
    }
}

#[cfg(test)]
mod tests {
    /// GPU time spent counter by counter, over those in both readings: a
    /// client gone since does not cancel another's work; one new, back after
    /// a miss, or started again counts from the next reading.
    #[test]
    fn gpu_time_is_counted_client_by_client() {
        let s = 1_000_000_000_u64;
        let before: super::GpuTimes = [((1, 0), 20 * s), ((2, 0), 100 * s)].into();
        let now: super::GpuTimes = [((2, 0), 100 * s + s / 5)].into();
        assert_eq!(super::gpu_time_spent(&before, &now), s / 5, "one gone");
        let now: super::GpuTimes =
            [((1, 0), 20 * s + s / 10), ((2, 0), 100 * s), ((3, 0), 30 * s)].into();
        assert_eq!(super::gpu_time_spent(&before, &now), s / 10, "a new one: from the next");
        let now: super::GpuTimes = [((1, 0), s / 4), ((2, 0), 100 * s)].into();
        assert_eq!(super::gpu_time_spent(&before, &now), 0, "started again: from the next");
        // A counter missing from one reading, back in the next: not its
        // whole 20 s.
        let partial: super::GpuTimes = [((2, 0), 100 * s + s / 5)].into();
        let back: super::GpuTimes = [((1, 0), 20 * s + s / 10), ((2, 0), 100 * s + s / 2)].into();
        assert_eq!(
            super::gpu_time_spent(&partial, &back),
            s / 2 - s / 5,
            "back after a miss: from the next"
        );
    }

    /// On a Mac, a process that has used the GPU finds its own GPU time and
    /// its footprint.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_process_finds_its_own_gpu_time_and_memory() {
        let gpu = crate::renderer::GpuContext::new_headless().expect("a GPU");
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 1024, height: 1024, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        for _ in 0..20 {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::RED),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        gpu.submit([encoder.finish()]);
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        let times = super::own_gpu_time().expect("its GPU clients found");
        assert!(!times.is_empty());
        let footprint = super::own_footprint().expect("its footprint");
        assert!(footprint > 1024 * 1024, "{footprint}");
    }

    /// On a Mac the GPU's busy share comes from macOS, a percentage.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_macs_gpu_busy_share_is_read_from_the_os() {
        let busy = super::gpu_busy_now().expect("an Apple GPU reports its utilization");
        assert!((0.0..=100.0).contains(&busy), "{busy}");
    }

    use super::*;

    #[test]
    fn system_monitor_initial_values() {
        let mut mon = SystemMonitor::new();
        // The first sample comes from the monitor's thread.
        let until = Instant::now() + Duration::from_secs(10);
        while mon.ram_total() == 0 && Instant::now() < until {
            mon.update();
            std::thread::sleep(Duration::from_millis(10));
        }
        // CPU usage should be between 0 and 100
        assert!(mon.cpu_usage() >= 0.0);
        assert!(mon.cpu_usage() <= 100.0);
        // RAM total should be positive on any real machine
        assert!(mon.ram_total() > 0);
        // Used <= Total
        assert!(mon.ram_used() <= mon.ram_total());
        // Percentage should be in range
        assert!(mon.ram_usage_pct() >= 0.0);
        assert!(mon.ram_usage_pct() <= 100.0);
    }

    /// The frame's update takes what the thread found if it can, and goes
    /// on at once if it cannot: it never waits on the system, nor on the
    /// thread.
    #[test]
    fn update_never_waits_on_the_sampling_thread() {
        let mut mon = SystemMonitor::new();
        let shared = Arc::clone(&mon.shared);
        let held = shared.lock().unwrap();
        let updating = std::thread::spawn(move || {
            mon.update();
            mon
        });
        let until = Instant::now() + Duration::from_secs(2);
        while !updating.is_finished() {
            assert!(Instant::now() < until, "update waited for the sample");
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(held);
        updating.join().unwrap();
    }

    #[test]
    fn update_does_not_panic() {
        let mut mon = SystemMonitor::new();
        // Multiple updates should work fine
        for _ in 0..3 {
            mon.update();
        }
    }
}
