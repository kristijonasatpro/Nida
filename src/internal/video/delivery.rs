//! Actual delivery, separate from the requested decoder preference.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

pub use crate::engine::value::video_delivery::DeliveryDiagnostics;

#[doc(hidden)]
#[derive(Default)]
pub struct DeliveryCounters {
    pub(crate) gpu_frames: AtomicU64,
    pub(crate) gpu_copies: AtomicU64,
    pub(crate) downloaded_bytes: AtomicU64,
    pub(crate) uploaded_bytes: AtomicU64,
    pub(crate) dropped_frames: AtomicU64,
    pub(crate) backpressure_events: AtomicU64,
    pub(crate) occupied_slots: AtomicU64,
    pub(crate) peak_slots: AtomicU64,
    pub(crate) copy_wait_ns: AtomicU64,
    pub(crate) fallback_reason: Mutex<Option<String>>,
}
impl DeliveryCounters {
    pub fn snapshot(&self) -> DeliveryDiagnostics {
        DeliveryDiagnostics {
            gpu_frames: self.gpu_frames.load(Ordering::Relaxed),
            gpu_copies: self.gpu_copies.load(Ordering::Relaxed),
            downloaded_bytes: self.downloaded_bytes.load(Ordering::Relaxed),
            uploaded_bytes: self.uploaded_bytes.load(Ordering::Relaxed),
            dropped_frames: self.dropped_frames.load(Ordering::Relaxed),
            backpressure_events: self.backpressure_events.load(Ordering::Relaxed),
            occupied_slots: self.occupied_slots.load(Ordering::Relaxed),
            peak_slots: self.peak_slots.load(Ordering::Relaxed),
            copy_wait_ns: self.copy_wait_ns.load(Ordering::Relaxed),
            fallback_reason: self.fallback_reason.lock().ok().and_then(|r| r.clone()),
        }
    }
    pub(crate) fn fallback(&self, reason: impl Into<String>) {
        if let Ok(mut slot) = self.fallback_reason.lock() {
            *slot = Some(reason.into());
        }
    }
}
