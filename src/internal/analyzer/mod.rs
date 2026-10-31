//! Analyzer plugin system — frame analysis for modulation and shader preprocessing.
//!
//! See `/spec/plugin-architecture.md` for the full design.

pub(crate) mod brightness;
mod capture;
#[cfg(feature = "face-detection")]
pub(crate) mod face_detect;
pub(crate) mod traits;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};

use traits::{Analyzer, AnalyzerInput, AnalyzerSchema, AnalyzerSnapshot};

// ── Registry ────────────────────────────────────────────────────────────────

type AnalyzerFactory = Box<dyn Fn() -> Box<dyn Analyzer> + Send + Sync>;

/// Execution category of a preprocessor type.
///
/// Shaders declare all categories identically in their ISF `PREPROCESSORS` block;
/// the category tells the engine how to run the thing and whether a shader that
/// declares it can load without it. See `/spec/effect-preprocessing.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreprocessorCategory {
    /// Worker thread consuming a CPU readback of the deck's own frame, publishing
    /// `AnalyzerSnapshot`s. Optional — degrades to default outputs.
    CpuAnalyzer,
    /// GPU passes reading textures owned by an external device manager. The device
    /// is acquired at load time; if it is unavailable the shader does not load.
    GpuDeviceBacked,
    // A third category — GPU passes reading the deck's own frame (`GpuFrameDerived`,
    // e.g. edge detect) — is designed in /spec/effect-preprocessing.md but not yet
    // implemented; it needs a frame-input path no current preprocessor uses.
}

impl PreprocessorCategory {
    /// Whether this category runs as GPU passes rather than an analyzer thread.
    /// GPU categories have no factory and must never be handed to `DeckAnalyzers`.
    pub(crate) fn is_gpu(self) -> bool {
        !matches!(self, Self::CpuAnalyzer)
    }

    /// Whether a shader declaring this type must fail to load when it is unavailable.
    pub(crate) fn is_required(self) -> bool {
        matches!(self, Self::GpuDeviceBacked)
    }
}

/// Registry of available preprocessor types. Built at app startup via builder pattern.
pub(crate) struct AnalyzerRegistry {
    factories: HashMap<String, AnalyzerFactory>,
    schemas: HashMap<String, AnalyzerSchema>,
    categories: HashMap<String, PreprocessorCategory>,
}

impl AnalyzerRegistry {
    pub(crate) fn new() -> Self {
        Self {
            factories: HashMap::new(),
            schemas: HashMap::new(),
            categories: HashMap::new(),
        }
    }

    /// Register a CPU-async analyzer type with a factory function.
    pub(crate) fn register<F>(mut self, analyzer_type: &str, factory: F) -> Self
    where
        F: Fn() -> Box<dyn Analyzer> + Send + Sync + 'static,
    {
        let instance = factory();
        let schema = instance.output_schema();
        self.schemas.insert(analyzer_type.to_owned(), schema);
        self.factories
            .insert(analyzer_type.to_owned(), Box::new(factory));
        self.categories
            .insert(analyzer_type.to_owned(), PreprocessorCategory::CpuAnalyzer);
        self
    }

    /// Register a GPU-inline preprocessor type. These have no factory and no worker
    /// thread — the deck render path owns their passes — so the schema is supplied
    /// directly rather than read off an instance.
    pub(crate) fn register_gpu(
        mut self,
        preprocessor_type: &str,
        category: PreprocessorCategory,
        schema: AnalyzerSchema,
    ) -> Self {
        debug_assert!(
            category.is_gpu(),
            "register_gpu called with a CPU category for '{preprocessor_type}'"
        );
        self.schemas.insert(preprocessor_type.to_owned(), schema);
        self.categories
            .insert(preprocessor_type.to_owned(), category);
        self
    }

    /// Create a new instance of the given analyzer type. Returns `None` for GPU
    /// categories, which have no factory by construction.
    pub(crate) fn create(&self, analyzer_type: &str) -> Option<Box<dyn Analyzer>> {
        self.factories.get(analyzer_type).map(|f| f())
    }

    /// List all registered preprocessor type names, GPU and CPU alike.
    pub(crate) fn available_types(&self) -> Vec<&str> {
        self.categories.keys().map(String::as_str).collect()
    }

    /// Get the output schema for a registered preprocessor type.
    pub(crate) fn schema_for(&self, analyzer_type: &str) -> Option<&AnalyzerSchema> {
        self.schemas.get(analyzer_type)
    }

    /// Get the execution category for a registered preprocessor type.
    pub(crate) fn category_for(&self, preprocessor_type: &str) -> Option<PreprocessorCategory> {
        self.categories.get(preprocessor_type).copied()
    }
}

// ── Per-Deck Instance Management ────────────────────────────────────────────

struct AnalyzerInstance {
    frame_size: Option<u32>,
    refcount: usize,
    thread: Option<JoinHandle<()>>,
    latest: Arc<ArcSwap<AnalyzerSnapshot>>,
    stop: Arc<AtomicBool>,
    frame_tx: Sender<AnalyzerInput>,
    /// Disconnects when the worker thread exits (the matching sender is owned by
    /// the thread). Used for a bounded, non-blocking stop on shutdown.
    done_rx: Receiver<()>,
}

/// Grace period to wait for an analyzer worker to exit before detaching it, so a
/// thread wedged in a blocking FFI call (e.g. ONNX Runtime) can never freeze
/// application shutdown.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// Manages running analyzer instances for a single deck.
pub(crate) struct DeckAnalyzers {
    instances: HashMap<String, AnalyzerInstance>,
    /// Reduction target and asynchronous readback, only while pixels are needed.
    capture: Option<capture::FrameCapture>,
}

impl DeckAnalyzers {
    pub(crate) fn new() -> Self {
        Self {
            instances: HashMap::new(),
            capture: None,
        }
    }

    /// Request an analyzer type. If already running, increments refcount.
    pub(crate) fn request(
        &mut self,
        analyzer_type: &str,
        registry: &AnalyzerRegistry,
        options: &serde_json::Value,
    ) -> Option<Arc<ArcSwap<AnalyzerSnapshot>>> {
        if let Some(inst) = self.instances.get_mut(analyzer_type) {
            inst.refcount += 1;
            log::debug!("Analyzer '{analyzer_type}' refcount -> {}", inst.refcount);
            return Some(Arc::clone(&inst.latest));
        }

        let analyzer = registry.create(analyzer_type)?;
        let frame_size = analyzer.frame_size().map(|size| size.max(1));

        // Schema is static and does not require init(), so we can build the
        // initial default snapshot before the worker thread runs.
        let schema = analyzer.output_schema();
        let initial = AnalyzerSnapshot::from_defaults(&schema);
        let latest = Arc::new(ArcSwap::from_pointee(initial));
        let stop = Arc::new(AtomicBool::new(false));
        let (frame_tx, frame_rx) = crossbeam_channel::bounded(2);
        let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(0);

        let thread_latest = Arc::clone(&latest);
        let thread_stop = Arc::clone(&stop);
        let type_name = analyzer_type.to_owned();
        // init() can be expensive (e.g. loading + optimizing ONNX models), so it
        // runs inside the worker thread to keep the UI/render thread responsive.
        let options = options.clone();

        let thread = std::thread::Builder::new()
            .name(format!("analyzer-{type_name}"))
            .spawn(move || {
                // Dropped when the thread exits (normally or via panic),
                // disconnecting `done_rx` so stoppers can wait with a timeout.
                let _done = done_tx;
                analyzer_thread(
                    analyzer,
                    &options,
                    &frame_rx,
                    &thread_latest,
                    &thread_stop,
                    &type_name,
                );
            })
            .ok()?;

        log::info!("Spawned analyzer '{analyzer_type}'");
        let handle = Arc::clone(&latest);
        self.instances.insert(
            analyzer_type.to_owned(),
            AnalyzerInstance {
                frame_size,
                refcount: 1,
                thread: Some(thread),
                latest,
                stop,
                frame_tx,
                done_rx,
            },
        );
        Some(handle)
    }

    /// Release an analyzer reference. Stops when refcount reaches zero.
    pub(crate) fn release(&mut self, analyzer_type: &str) {
        let should_remove = if let Some(inst) = self.instances.get_mut(analyzer_type) {
            inst.refcount = inst.refcount.saturating_sub(1);
            inst.refcount == 0
        } else {
            false
        };

        if should_remove {
            if let Some(inst) = self.instances.remove(analyzer_type) {
                stop_instance(inst, analyzer_type, "");
            }
        }
    }

    /// Get the latest snapshot for a specific analyzer type.
    pub(crate) fn latest_snapshot(
        &self,
        analyzer_type: &str,
    ) -> Option<arc_swap::Guard<Arc<AnalyzerSnapshot>>> {
        self.instances
            .get(analyzer_type)
            .map(|inst| inst.latest.load())
    }

    /// Iterate over all active analyzer snapshots: (`analyzer_type`, snapshot).
    pub(crate) fn all_snapshots(
        &self,
    ) -> impl Iterator<Item = (String, arc_swap::Guard<Arc<AnalyzerSnapshot>>)> + '_ {
        self.instances
            .iter()
            .map(|(k, inst)| (k.clone(), inst.latest.load()))
    }

    /// Send a frame to all running analyzers (non-blocking, drops if full).
    pub(crate) fn send_frame(&self, input: &AnalyzerInput) {
        for (name, inst) in &self.instances {
            match inst.frame_tx.try_send(input.clone()) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    log::warn!("Analyzer '{name}' channel disconnected");
                }
            }
        }
    }

    /// Remove instances whose worker thread has exited (e.g. `init()` failed
    /// because a dependency like ONNX Runtime is unavailable). Without this a
    /// dead instance lingers in the map, causing the render loop to perform a
    /// per-frame GPU readback and spam "channel disconnected" warnings on the
    /// hot path. Pruning is cheap: a non-blocking `try_recv` on the rendezvous
    /// channel that disconnects when the worker drops its sender.
    fn prune_dead(&mut self) {
        let dead: Vec<String> = self
            .instances
            .iter()
            .filter(|(_, inst)| matches!(inst.done_rx.try_recv(), Err(TryRecvError::Disconnected)))
            .map(|(name, _)| name.clone())
            .collect();

        for name in dead {
            if let Some(mut inst) = self.instances.remove(&name) {
                if let Some(thread) = inst.thread.take() {
                    let _ = thread.join();
                }
                log::warn!("Analyzer '{name}' worker exited; removing instance");
            }
        }
    }

    /// Capture the current deck texture for analysis and deliver previous frame's data to analyzers.
    /// Call this from the render loop after effects are applied.
    /// Returns a command buffer with the readback copy command, or None if no analyzers are active.
    pub(crate) fn capture_frame(
        &mut self,
        device: &wgpu::Device,
        source_texture: &wgpu::Texture,
    ) -> Option<wgpu::CommandBuffer> {
        self.prune_dead();
        if self.instances.is_empty() {
            self.capture = None;
            return None;
        }

        let source_size = (source_texture.width(), source_texture.height());
        let wanted = self
            .instances
            .values()
            .filter_map(|inst| inst.frame_size)
            .max();
        let Some(long_side) = wanted else {
            self.capture = None;
            self.send_frame(&AnalyzerInput {
                frame: Arc::new(Vec::new()),
                width: source_size.0,
                height: source_size.1,
                timestamp: std::time::Instant::now(),
            });
            return None;
        };
        let size = capture::capture_size(source_size, long_side);
        if self
            .capture
            .as_ref()
            .is_none_or(|capture| !capture.fits(source_size, size))
        {
            self.capture = Some(capture::FrameCapture::new(device, source_size, size));
        }
        if let Some(frame) = self
            .capture
            .as_mut()
            .and_then(|capture| capture.try_read(device))
        {
            let input = AnalyzerInput {
                width: frame.width(),
                height: frame.height(),
                timestamp: frame.made(),
                frame: Arc::new(frame.into_bytes()),
            };
            self.send_frame(&input);
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Analyzer readback"),
        });
        self.capture
            .as_mut()
            .unwrap()
            .encode(device, &mut encoder, source_texture);
        Some(encoder.finish())
    }

    /// Check if any analyzer instances are currently running.
    pub(crate) fn has_active_instances(&self) -> bool {
        !self.instances.is_empty()
    }

    pub(crate) fn running_types(&self) -> Vec<String> {
        self.instances.keys().cloned().collect()
    }

    /// Stop all running instances.
    pub(crate) fn shutdown(&mut self) {
        let types: Vec<String> = self.instances.keys().cloned().collect();
        for t in types {
            if let Some(inst) = self.instances.remove(&t) {
                stop_instance(inst, &t, " (deck shutdown)");
            }
        }
    }
}

impl Drop for DeckAnalyzers {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ── Analyzer Thread ─────────────────────────────────────────────────────────

fn analyzer_thread(
    mut analyzer: Box<dyn Analyzer>,
    options: &serde_json::Value,
    frame_rx: &Receiver<AnalyzerInput>,
    latest: &ArcSwap<AnalyzerSnapshot>,
    stop: &AtomicBool,
    type_name: &str,
) {
    // Run potentially-expensive initialization off the caller's thread. On
    // failure the thread exits and the deck keeps the default snapshot.
    if let Err(e) = analyzer.init(options) {
        log::error!("Failed to init analyzer '{type_name}': {e}");
        return;
    }
    log::info!("Analyzer thread '{type_name}' started");
    while !stop.load(Ordering::Relaxed) {
        match frame_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(input) => match analyzer.analyze(&input) {
                Ok(snapshot) => {
                    latest.store(Arc::new(snapshot));
                }
                Err(e) => {
                    log::error!("Analyzer '{type_name}' error: {e}");
                }
            },
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    analyzer.shutdown();
    log::info!("Analyzer thread '{type_name}' stopped");
}

/// Stop a single analyzer instance with a bounded wait. Signals the worker to
/// stop and waits up to [`STOP_GRACE`] for it to exit; if it does, the handle is
/// joined (reaped). If it does not — e.g. the thread is wedged in a blocking FFI
/// call — the handle is detached so application shutdown is never frozen.
fn stop_instance(mut inst: AnalyzerInstance, type_name: &str, suffix: &str) {
    inst.stop.store(true, Ordering::Relaxed);
    drop(inst.frame_tx);

    let exited = !matches!(
        inst.done_rx.recv_timeout(STOP_GRACE),
        Err(crossbeam_channel::RecvTimeoutError::Timeout)
    );

    if exited {
        if let Some(thread) = inst.thread.take() {
            let _ = thread.join();
        }
        log::info!("Stopped analyzer '{type_name}'{suffix}");
    } else {
        // Detach the wedged thread; the OS reclaims it on process exit.
        let _ = inst.thread.take();
        log::warn!("Analyzer '{type_name}'{suffix} did not stop within {STOP_GRACE:?}; detaching");
    }
}

// ── Default Registry ────────────────────────────────────────────────────────

/// Build the default analyzer registry with all built-in analyzers.
pub(crate) fn default_registry() -> AnalyzerRegistry {
    #[allow(unused_mut)]
    let mut registry = AnalyzerRegistry::new().register("brightness", || {
        Box::new(brightness::BrightnessAnalyzer::new())
    });
    #[cfg(feature = "face-detection")]
    {
        registry = registry.register("face_detect", || {
            Box::new(face_detect::FaceDetectAnalyzer::new())
        });
    }
    // Device-backed GPU preprocessor: no factory, no worker thread. Registered
    // unconditionally — without the `depth` feature no sensor enumerates, so a
    // shader declaring it fails its pre-flight with a clear message rather than
    // an "unknown preprocessor type". See /spec/depth-sensor-preprocessor.md.
    registry = registry.register_gpu(
        crate::depth::preprocess::PREPROCESSOR_TYPE,
        PreprocessorCategory::GpuDeviceBacked,
        crate::depth::preprocess::schema(),
    );
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn mixed_analyzers_share_the_largest_capture_and_metadata_only_skips_it() {
        let gpu = match crate::renderer::GpuContext::new_headless() {
            Ok(gpu) => gpu,
            Err(error) => {
                assert!(
                    std::env::var_os("VARDA_REQUIRE_GPU").is_none(),
                    "GPU required: {error}"
                );
                return;
            }
        };
        let source = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mixed analyzer fixture"),
            size: wgpu::Extent3d {
                width: 1024,
                height: 512,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mut deck = DeckAnalyzers::new();
        let mut guards = Vec::new();
        let mut add = |name: &str, frame_size| {
            let (frame_tx, received) = crossbeam_channel::bounded(4);
            let (done, done_rx) = crossbeam_channel::bounded(1);
            guards.push(done);
            deck.instances.insert(
                name.into(),
                AnalyzerInstance {
                    frame_size,
                    refcount: 1,
                    thread: None,
                    latest: Arc::new(ArcSwap::from_pointee(AnalyzerSnapshot::empty())),
                    stop: Arc::default(),
                    frame_tx,
                    done_rx,
                },
            );
            received
        };
        let small = add("small", Some(64));
        let large = add("large", Some(256));
        let metadata = add("metadata", None);
        let deadline = Instant::now() + Duration::from_secs(5);
        let first = loop {
            let encoded = deck.capture_frame(&gpu.device, &source).unwrap();
            gpu.submit([encoded]);
            gpu.device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            if let Ok(frame) = small.try_recv() {
                break frame;
            }
            assert!(Instant::now() < deadline, "capture never arrived");
        };
        let second = large.try_recv().unwrap();
        assert_eq!((first.width, first.height), (256, 128));
        assert!(
            Arc::ptr_eq(&first.frame, &second.frame),
            "one readback serves both workers"
        );
        assert_eq!(first.timestamp, second.timestamp);
        deck.instances.remove("large");
        assert!(deck.capture_frame(&gpu.device, &source).is_some());
        assert!(deck.capture.as_ref().unwrap().fits((1024, 512), (64, 32)));
        deck.instances.remove("small");
        metadata.try_iter().for_each(drop);
        assert!(deck.capture_frame(&gpu.device, &source).is_none());
        assert!(deck.capture.is_none());
        let frame = metadata.try_recv().unwrap();
        assert!(frame.frame.is_empty());
        assert_eq!((frame.width, frame.height), (1024, 512));
        deck.instances.clear();
        assert!(deck.capture_frame(&gpu.device, &source).is_none());
    }

    #[test]
    fn registry_builder_pattern() {
        let registry = default_registry();
        let types = registry.available_types();
        assert!(types.contains(&"brightness"));
        assert!(registry.schema_for("brightness").is_some());
        assert!(registry.schema_for("nonexistent").is_none());
    }

    #[test]
    fn registry_create_instance() {
        let registry = default_registry();
        let instance = registry.create("brightness");
        assert!(instance.is_some());
        assert_eq!(instance.unwrap().analyzer_type(), "brightness");
    }

    #[test]
    fn deck_analyzers_lifecycle() {
        let registry = default_registry();
        let mut deck = DeckAnalyzers::new();

        let handle = deck
            .request("brightness", &registry, &serde_json::Value::Null)
            .expect("should create");
        assert!(deck.has_active_instances());

        let handle2 = deck
            .request("brightness", &registry, &serde_json::Value::Null)
            .expect("should reuse");
        let _ = (handle, handle2);

        deck.release("brightness");
        assert!(deck.has_active_instances());

        deck.release("brightness");
        assert!(!deck.has_active_instances());
    }

    #[test]
    fn deck_analyzers_send_and_read() {
        let registry = default_registry();
        let mut deck = DeckAnalyzers::new();

        let _handle = deck
            .request("brightness", &registry, &serde_json::Value::Null)
            .expect("should create");

        let input = AnalyzerInput {
            frame: Arc::new(vec![255u8; 4 * 4 * 4]),
            width: 4,
            height: 4,
            timestamp: Instant::now(),
        };
        deck.send_frame(&input);
        std::thread::sleep(Duration::from_millis(200));

        let snapshot = deck
            .latest_snapshot("brightness")
            .expect("should have snapshot");
        let brightness = snapshot.scalar("brightness");
        assert!(
            brightness > 0.9,
            "expected brightness ~1.0, got {brightness}"
        );
        deck.shutdown();
    }

    #[cfg(feature = "face-detection")]
    #[test]
    fn dead_worker_is_pruned() {
        // Force face_detect's init() to fail by pointing it at a missing model
        // file. The worker thread then exits; the instance must be pruned so the
        // render loop stops per-frame GPU readback and "channel disconnected"
        // log spam. Shutdown must also stay fast (the worker already exited).
        let registry = default_registry();
        let mut deck = DeckAnalyzers::new();
        let opts = serde_json::json!({
            "model_path": "/nonexistent/__varda_missing_model__.onnx"
        });
        let _handle = deck
            .request("face_detect", &registry, &opts)
            .expect("should spawn worker");

        // Poll for the worker to run init() (which fails) and exit, then be
        // pruned. A single fixed sleep is racy: under CPU load the worker may not
        // have finished the failing ONNX init() and dropped its done_tx yet.
        // Poll prune_dead() until the dead instance is removed, bounded by a
        // generous timeout so a genuine hang still fails the test.
        let deadline = Instant::now() + Duration::from_secs(10);
        while deck.has_active_instances() && Instant::now() < deadline {
            deck.prune_dead();
            if !deck.has_active_instances() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !deck.has_active_instances(),
            "dead worker instance should be pruned"
        );

        let start = Instant::now();
        deck.shutdown();
        assert!(
            start.elapsed() < STOP_GRACE,
            "shutdown must be fast when the worker already exited, took {:?}",
            start.elapsed()
        );
    }
}
