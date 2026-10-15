//! System monitor — lightweight CPU & RAM usage sampling.
//!
//! Samples are taken at a configurable interval (default 1s) to avoid
//! the cost of querying sysinfo every frame. The latest values are
//! exposed via simple getters for the snapshot pipeline.

use std::time::{Duration, Instant};
use sysinfo::System;

/// How often to re-sample CPU/RAM (default: 1 second).
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Lightweight system resource monitor.
///
/// Call `update()` every frame — it only actually re-samples when
/// `SAMPLE_INTERVAL` has elapsed since the last sample.
pub struct SystemMonitor {
    sys: System,
    last_sample: Instant,
    /// CPU usage as a percentage (0–100), averaged across all cores.
    cpu_usage: f32,
    /// Total physical RAM in bytes.
    ram_total: u64,
    /// Used physical RAM in bytes.
    ram_used: u64,
    /// The GPU's busy share (0–100) as the OS reports it, where it does
    /// (macOS: see [`gpu_busy_now`]).
    gpu_busy: Option<f32>,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    pub fn new() -> Self {
        let mut sys = System::new();
        // Initial refresh to populate baseline
        sys.refresh_cpu_usage();
        sys.refresh_memory();

        let cpu_usage = Self::avg_cpu(&sys);
        let ram_total = sys.total_memory();
        let ram_used = sys.used_memory();

        Self {
            sys,
            last_sample: Instant::now(),
            cpu_usage,
            ram_total,
            ram_used,
            gpu_busy: gpu_busy_now(),
        }
    }

    /// Call every frame. Only re-samples when the interval has elapsed.
    pub fn update(&mut self) {
        if self.last_sample.elapsed() >= SAMPLE_INTERVAL {
            self.sys.refresh_cpu_usage();
            self.sys.refresh_memory();
            self.cpu_usage = Self::avg_cpu(&self.sys);
            self.ram_total = self.sys.total_memory();
            self.ram_used = self.sys.used_memory();
            self.gpu_busy = gpu_busy_now();
            self.last_sample = Instant::now();
        }
    }

    /// CPU usage % (0–100), averaged across all cores.
    pub fn cpu_usage(&self) -> f32 {
        self.cpu_usage
    }

    /// The GPU's busy share (0–100) as the OS reports it; `None` where it
    /// does not.
    pub fn gpu_busy(&self) -> Option<f32> {
        self.gpu_busy
    }

    /// Total physical RAM in bytes.
    pub fn ram_total(&self) -> u64 {
        self.ram_total
    }

    /// Used physical RAM in bytes.
    pub fn ram_used(&self) -> u64 {
        self.ram_used
    }

    /// RAM usage as a percentage (0–100).
    pub fn ram_usage_pct(&self) -> f32 {
        if self.ram_total > 0 {
            (self.ram_used as f64 / self.ram_total as f64 * 100.0) as f32
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

    unsafe fn cf_string(s: &std::ffi::CStr) -> CFTypeRef {
        // SAFETY: a NUL-terminated UTF-8 string; the caller releases it.
        unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), K_CF_STRING_ENCODING_UTF8) }
    }
}

#[cfg(test)]
mod tests {
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
        let mon = SystemMonitor::new();
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

    #[test]
    fn update_does_not_panic() {
        let mut mon = SystemMonitor::new();
        // Multiple updates should work fine
        for _ in 0..3 {
            mon.update();
        }
    }
}
