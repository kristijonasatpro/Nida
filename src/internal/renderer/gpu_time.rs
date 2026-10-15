//! GPU time by part of the app.
//!
//! Apple GPUs give no timestamps inside passes, and Nida's own estimate fell
//! back to CPU time. Metal does say when each command buffer started and
//! ended on the GPU; Nida's wgpu-hal patch keeps that under the buffer's label
//! (`Queue::take_gpu_times`, vendor/wgpu-hal/NIDA-PATCH.md). wgpu records each
//! render pass in a command buffer of its own, named after the pass, so summed
//! over a second by label it says where the GPU time goes — the show's
//! passes, the control window, its previews, the outputs — and, buffers that
//! overlap counted once, how much of the second Nida kept the GPU busy.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::engine::{GpuPartTime, GpuTimeSnapshot};

/// One command buffer's run on the GPU: its label, and when it started and
/// ended, host seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Ran {
    pub label: String,
    pub start: f64,
    pub end: f64,
}

/// The time a second's GPU figures are summed over.
const WINDOW: Duration = Duration::from_secs(1);

/// What the GPU ran for Nida, summed a second at a time.
#[derive(Debug, Default)]
pub struct GpuTime {
    since: Option<Instant>,
    /// The second's runs: start, end, and the label's place in `labels`.
    runs: Vec<(f64, f64, usize)>,
    labels: Vec<String>,
    label_at: HashMap<String, usize>,
    last: Option<GpuTimeSnapshot>,
}

impl GpuTime {
    /// Take the runs reported since the last call, at `now`.
    pub fn add(&mut self, now: Instant, ran: impl IntoIterator<Item = Ran>) {
        let since = *self.since.get_or_insert(now);
        for run in ran {
            // Not started (zero) or not ended: Metal had no time for it.
            if run.start > 0.0 && run.end > run.start {
                let at = if let Some(&at) = self.label_at.get(&run.label) {
                    at
                } else {
                    self.labels.push(run.label.clone());
                    self.label_at.insert(run.label, self.labels.len() - 1);
                    self.labels.len() - 1
                };
                self.runs.push((run.start, run.end, at));
            }
        }
        let seconds = now.saturating_duration_since(since);
        if seconds >= WINDOW {
            self.last = Some(summarise(&self.runs, &self.labels, seconds.as_secs_f64()));
            self.runs.clear();
            self.labels.clear();
            self.label_at.clear();
            self.since = Some(now);
        }
    }

    /// The last whole second's figures; `None` before the first.
    pub fn snapshot(&self) -> Option<GpuTimeSnapshot> {
        self.last.clone()
    }
}

/// A second's `runs` (start, end, label index into `labels`) over `seconds`
/// as figures per second.
///
/// Command buffers on the GPU overlap — measured on a laptop, their spans
/// added up to five times the time any was running, and a 1×1 clear "took"
/// a millisecond beside big passes. So each moment the GPU was busy is shared
/// equally among the runs going on then: the parts add up to the time the GPU
/// was busy with Nida's work, and a run is charged for the time it had, not
/// for the time it waited beside others.
fn summarise(runs: &[(f64, f64, usize)], labels: &[String], seconds: f64) -> GpuTimeSnapshot {
    let mut events: Vec<(f64, bool, usize)> = Vec::with_capacity(runs.len() * 2);
    for (i, &(start, end, _)) in runs.iter().enumerate() {
        events.push((start, true, i));
        events.push((end, false, i));
    }
    // Ends before starts at the same moment: runs that only touch do not
    // overlap.
    events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut share = vec![0.0_f64; labels.len()];
    let mut busy = 0.0;
    let mut active: Vec<usize> = Vec::new();
    let mut last = events.first().map_or(0.0, |e| e.0);
    for &(at, starts, run) in &events {
        if !active.is_empty() && at > last {
            let span = at - last;
            busy += span;
            #[allow(clippy::cast_precision_loss)]
            let each = span / active.len() as f64;
            for &a in &active {
                share[runs[a].2] += each;
            }
        }
        last = at;
        if starts {
            active.push(run);
        } else if let Some(place) = active.iter().position(|&a| a == run) {
            active.swap_remove(place);
        }
    }
    let mut counts = vec![0_u32; labels.len()];
    for &(_, _, label) in runs {
        counts[label] += 1;
    }

    let per_second = |secs: f64| (secs / seconds * 1000.0) as f32;
    let rate = |count: u32| (f64::from(count) / seconds) as f32;
    let mut parts: Vec<GpuPartTime> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| GpuPartTime {
            name: if label.is_empty() {
                "(no label)".to_owned()
            } else {
                label.clone()
            },
            ms_per_s: per_second(share[i]),
            runs_per_s: rate(counts[i]),
        })
        .collect();
    parts.sort_by(|a, b| b.ms_per_s.total_cmp(&a.ms_per_s));

    let mut by_group: HashMap<&'static str, (f64, u32)> = HashMap::new();
    for (i, label) in labels.iter().enumerate() {
        let group = by_group.entry(group_of(label)).or_default();
        group.0 += share[i];
        group.1 += counts[i];
    }
    let mut groups: Vec<GpuPartTime> = by_group
        .into_iter()
        .map(|(group, (secs, count))| GpuPartTime {
            name: group.to_owned(),
            ms_per_s: per_second(secs),
            runs_per_s: rate(count),
        })
        .collect();
    groups.sort_by(|a, b| b.ms_per_s.total_cmp(&a.ms_per_s));

    GpuTimeSnapshot {
        busy_pct: (busy / seconds * 100.0).min(100.0) as f32,
        groups,
        parts,
    }
}

/// The part of the app a command buffer's label belongs to: a pass's label,
/// or its encoder's for work outside passes.
fn group_of(label: &str) -> &'static str {
    match label {
        "Control Window Pass" | "Screen Encoder" => "control window",
        "Preview Encode Pass" | "Preview Encode" => "previews",
        _ if label.starts_with("Output '") => "outputs",
        _ if label.is_empty() || label.starts_with("(wgpu internal)") => "wgpu (uploads, clears)",
        _ => "show",
    }
}

/// The runs the GPU has finished since the last call, by label (macOS, Nida's
/// wgpu-hal patch); empty elsewhere.
pub fn take_runs(queue: &wgpu::Queue) -> Vec<Ran> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: reads a record the submit path keeps; the queue itself is
        // not touched.
        if let Some(hal) = unsafe { queue.as_hal::<wgpu::hal::api::Metal>() } {
            return hal
                .take_gpu_times()
                .into_iter()
                .map(|t| Ran {
                    label: t.label,
                    start: t.start,
                    end: t.end,
                })
                .collect();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = queue;
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ran(label: &str, start: f64, end: f64) -> Ran {
        Ran {
            label: label.to_owned(),
            start,
            end,
        }
    }

    /// A second's runs as milliseconds per second, by label and by part of
    /// the app, most first; overlapping runs count once towards how busy the
    /// GPU was.
    #[test]
    fn a_seconds_runs_add_up_by_label_and_part_and_overlaps_count_once() {
        let start = Instant::now();
        let mut gpu = GpuTime::default();
        gpu.add(
            start,
            [
                ran("Control Window Pass", 100.000, 100.004),
                ran("Preview Encode Pass", 100.003, 100.005),
                ran("Output 'LED' Swap Blit", 100.010, 100.011),
                ran("Mixer Composite Pass", 100.020, 100.026),
                ran("(wgpu internal) PendingWrites", 100.030, 100.031),
                ran("Control Window Pass", 100.100, 100.104),
                ran("never ran", 0.0, 0.0),
            ],
        );
        assert!(gpu.snapshot().is_none(), "not a second yet");
        gpu.add(start + Duration::from_secs(1), []);
        let s = gpu.snapshot().expect("a second's figures");
        let ms = |list: &[GpuPartTime], name: &str| {
            list.iter()
                .find(|p| p.name == name)
                .map(|p| (p.ms_per_s * 1000.0).round() / 1000.0)
        };
        // The control window's first run and the preview overlap for a
        // millisecond, shared between them.
        assert_eq!(ms(&s.parts, "Control Window Pass"), Some(7.5));
        assert_eq!(s.parts[0].name, "Control Window Pass", "most first");
        assert_eq!(ms(&s.groups, "control window"), Some(7.5));
        assert_eq!(ms(&s.groups, "previews"), Some(1.5));
        assert_eq!(ms(&s.groups, "outputs"), Some(1.0));
        assert_eq!(ms(&s.groups, "show"), Some(6.0));
        assert_eq!(ms(&s.groups, "wgpu (uploads, clears)"), Some(1.0));
        assert!(ms(&s.parts, "never ran").is_none());
        let runs = |name: &str| {
            s.parts
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.runs_per_s)
        };
        assert_eq!(runs("Control Window Pass"), Some(2.0));
        // 4 + 1 (the preview past the screen's end) + 1 + 6 + 1 + 4 ms, and
        // the parts add up to it.
        assert!((s.busy_pct - 1.7).abs() < 0.001, "{}", s.busy_pct);
        let total: f32 = s.parts.iter().map(|p| p.ms_per_s).sum();
        assert!((total - 17.0).abs() < 0.001, "{total}");
    }

    /// Each second stands alone: the next starts from nothing.
    #[test]
    fn each_second_starts_afresh() {
        let start = Instant::now();
        let mut gpu = GpuTime::default();
        gpu.add(start, [ran("Control Window Pass", 1.0, 1.5)]);
        gpu.add(start + Duration::from_secs(1), []);
        gpu.add(start + Duration::from_secs(2), []);
        let s = gpu.snapshot().expect("figures");
        assert!(s.parts.is_empty() && s.busy_pct == 0.0, "{s:?}");
    }

    /// On a Mac, a render pass's run on the GPU is reported under its label
    /// (Nida's wgpu-hal patch; wgpu gives each pass a command buffer).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_command_buffers_gpu_time_is_reported_under_its_label() {
        let Ok(gpu) = crate::renderer::context::GpuContext::new_headless() else {
            return;
        };
        let target = gpu.create_compositing_texture(256, 256);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Timed Encoder"),
            });
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Timed Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        gpu.submit([encoder.finish()]);
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        // The completion handler runs on a Metal thread of its own.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut runs = Vec::new();
        while Instant::now() < deadline {
            runs.extend(take_runs(&gpu.queue));
            if runs.iter().any(|r| r.label == "Timed Pass") {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let timed = runs
            .iter()
            .find(|r| r.label == "Timed Pass")
            .unwrap_or_else(|| panic!("no run for the pass: {runs:?}"));
        assert!(timed.end > timed.start && timed.start > 0.0, "{timed:?}");
    }
}
