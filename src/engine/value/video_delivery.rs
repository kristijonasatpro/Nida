//! Primitive video delivery diagnostics shared by engine snapshots and players.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct DeliveryDiagnostics {
    pub gpu_frames: u64,
    pub gpu_copies: u64,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub dropped_frames: u64,
    pub backpressure_events: u64,
    pub occupied_slots: u64,
    pub peak_slots: u64,
    /// Worker copy time including fence waits, not GPU timestamps.
    pub copy_wait_ns: u64,
    pub fallback_reason: Option<String>,
}
