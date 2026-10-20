//! Bounded effect construction. The show holds an editable shell while workers
//! compile and allocate resources. Dropping a shell cancels queued work and
//! disconnects its reply, so a removed effect can never return to the chain.

use std::sync::{Arc, Weak};

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};

use super::Effect;
use crate::engine::types::EffectLoadState;
use crate::isf::ISFShader;
use crate::renderer::GpuContext;

const WORKERS: usize = 2;
const QUEUED: usize = 32;

pub(super) enum EffectBuild {
    Ready,
    Building {
        reply: Receiver<Result<Effect, String>>,
        // Identity belongs to this particular shell, not its persisted UUID.
        _alive: Arc<()>,
    },
    Failed(String),
}

struct Job {
    shader: ISFShader,
    format: wgpu::TextureFormat,
    alive: Weak<()>,
    reply: Sender<Result<Effect, String>>,
}

pub(crate) struct EffectLoader {
    jobs: Sender<Job>,
}

impl EffectLoader {
    pub(crate) fn new(context: &GpuContext) -> Result<Self> {
        let context = context.clone();
        Self::with_builder(move |shader, format| {
            // wgpu 30 scopes are thread-local. A bad build must not be blamed
            // on whichever live deck the show thread happens to be drawing.
            let validation = context
                .device
                .push_error_scope(wgpu::ErrorFilter::Validation);
            let internal = context.device.push_error_scope(wgpu::ErrorFilter::Internal);
            let memory = context
                .device
                .push_error_scope(wgpu::ErrorFilter::OutOfMemory);
            let mut result = Effect::new_with_format(&context, shader, format);
            for scope in [memory, internal, validation] {
                if let Some(error) = pollster::block_on(scope.pop()) {
                    result = Err(anyhow::anyhow!("{error}"));
                }
            }
            result
        })
    }

    pub(crate) fn with_builder(
        builder: impl Fn(ISFShader, wgpu::TextureFormat) -> Result<Effect> + Send + Sync + 'static,
    ) -> Result<Self> {
        let (jobs, receiver) = bounded::<Job>(QUEUED);
        let builder = Arc::new(builder);
        for index in 0..WORKERS {
            let receiver = receiver.clone();
            let builder = Arc::clone(&builder);
            std::thread::Builder::new()
                .name(format!("effect-loader-{index}"))
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        if job.alive.upgrade().is_none() {
                            continue;
                        }
                        let started = std::time::Instant::now();
                        let name = job.shader.name();
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            builder(job.shader, job.format)
                        }))
                        .map_err(|_| {
                            "Effect loader panicked while constructing resources".to_string()
                        })
                        .and_then(|result| result.map_err(|error| format!("{error:#}")));
                        log::debug!("Built effect {name} in {:?}", started.elapsed());
                        // A single reply fits without waiting for another frame.
                        let _ = job.reply.send(result);
                    }
                })
                .context("Could not start effect loader")?;
        }
        Ok(Self { jobs })
    }

    pub(crate) fn load(&self, shader: ISFShader, format: wgpu::TextureFormat) -> Result<Effect> {
        let mut effect = Effect::pending(shader.clone(), format);
        let (send, reply) = bounded(1);
        let alive = Arc::new(());
        self.jobs
            .try_send(Job {
                shader,
                format,
                alive: Arc::downgrade(&alive),
                reply: send,
            })
            .map_err(|_| {
                anyhow::anyhow!("Effect loading queue is full or unavailable; try again shortly")
            })?;
        effect.build = EffectBuild::Building {
            reply,
            _alive: alive,
        };
        Ok(effect)
    }
}

impl Effect {
    pub(crate) fn load_state(&self) -> EffectLoadState {
        match &self.build {
            EffectBuild::Ready => EffectLoadState::Ready,
            EffectBuild::Building { .. } => EffectLoadState::Building,
            EffectBuild::Failed(message) => EffectLoadState::Failed {
                message: message.clone(),
            },
        }
    }

    /// True only when this effect can contribute to the current picture.
    pub fn is_active(&self) -> bool {
        self.enabled && self.pipeline.is_some()
    }

    /// Install only resources. Edits, bypass, masks, UUID and chain position
    /// belong to the live shell and survive completion unchanged.
    pub(crate) fn poll_load(&mut self) -> bool {
        let EffectBuild::Building { reply, .. } = &self.build else {
            return false;
        };
        let result = match reply.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                Err("Effect loader stopped before completing".into())
            }
        };
        match result {
            Ok(ready) => {
                self.pipeline = ready.pipeline;
                self.pass_buffers = ready.pass_buffers;
                self.imported_textures = ready.imported_textures;
                self.preprocessor_textures = ready.preprocessor_textures;
                self.build = EffectBuild::Ready;
            }
            Err(message) => self.build = EffectBuild::Failed(message),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn shader() -> ISFShader {
        ISFShader::from_string(
            r#"/*{"ISFVSN":"2","TYPE":"FILTER",
            "INPUTS":[{"NAME":"amount","TYPE":"float","DEFAULT":0.5}]}*/
            void main() { gl_FragColor = IMG_THIS_PIXEL(inputImage); }"#,
        )
        .unwrap()
    }

    #[test]
    fn effect_queue_is_bounded_and_removed_requests_are_skipped() {
        let (started, starts) = bounded(64);
        let (release, gate) = bounded::<()>(64);
        let loader = EffectLoader::with_builder(move |_, _| {
            started.send(()).unwrap();
            gate.recv().unwrap();
            anyhow::bail!("deliberate build failure")
        })
        .unwrap();
        let mut effects = Vec::new();
        for _ in 0..WORKERS {
            effects.push(
                loader
                    .load(shader(), crate::renderer::context::COLOR_PATH_FORMAT)
                    .unwrap(),
            );
        }
        for _ in 0..WORKERS {
            starts.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        for _ in 0..QUEUED {
            effects.push(
                loader
                    .load(shader(), crate::renderer::context::COLOR_PATH_FORMAT)
                    .unwrap(),
            );
        }
        assert!(loader
            .load(shader(), crate::renderer::context::COLOR_PATH_FORMAT)
            .is_err());
        effects.truncate(WORKERS); // All queued jobs are now cancelled.
        for _ in 0..WORKERS {
            release.send(()).unwrap();
        }
        let until = Instant::now() + Duration::from_secs(5);
        let mut survivor = loop {
            if let Ok(effect) = loader.load(shader(), crate::renderer::context::COLOR_PATH_FORMAT) {
                break effect;
            }
            assert!(
                Instant::now() < until,
                "cancelled requests did not leave the queue"
            );
            std::thread::yield_now();
        };
        starts.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            starts.try_recv().is_err(),
            "removed requests must never build"
        );
        release.send(()).unwrap();
        while !survivor.poll_load() {
            assert!(Instant::now() < until);
            std::thread::yield_now();
        }
        assert!(matches!(
            survivor.load_state(),
            EffectLoadState::Failed { .. }
        ));
        assert!(!survivor.is_active());
    }

    #[test]
    fn loader_failure_preserves_edits_and_disconnect_is_reported() {
        let mut effect = Effect::pending(shader(), crate::renderer::context::COLOR_PATH_FORMAT);
        let (sender, reply) = bounded(1);
        effect.build = EffectBuild::Building {
            reply,
            _alive: Arc::new(()),
        };
        let uuid = effect.uuid().to_owned();
        effect.enabled = false;
        effect
            .params
            .set("amount", crate::params::ParamValue::Float(0.8));
        assert!(!effect.poll_load(), "an unfinished build must not wait");
        drop(sender);
        assert!(effect.poll_load());
        assert_eq!(effect.uuid(), uuid);
        assert!(!effect.enabled);
        assert!(
            matches!(effect.params.values["amount"], crate::params::ParamValue::Float(v) if (v - 0.8).abs() < 1e-6)
        );
        assert!(
            matches!(effect.load_state(), EffectLoadState::Failed { message } if message.contains("stopped"))
        );
    }

    #[test]
    fn a_broken_shader_fails_without_stopping_other_builds() {
        let gpu = match GpuContext::new_headless() {
            Ok(gpu) => gpu,
            Err(error) => {
                assert!(
                    std::env::var_os("VARDA_REQUIRE_GPU").is_none(),
                    "headless GPU required: {error}"
                );
                return;
            }
        };
        let loader = EffectLoader::new(&gpu).unwrap();
        let shader = ISFShader::from_file(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders/invert.fs"),
        )
        .unwrap();
        let mut broken = shader.clone();
        broken.fragment_source = "not valid GLSL".into();
        let mut broken = loader.load(broken, gpu.compositing_format).unwrap();
        let mut good = loader.load(shader, gpu.compositing_format).unwrap();
        let faults = gpu.errors.fault_count();
        let until = Instant::now() + Duration::from_secs(20);
        while matches!(broken.load_state(), EffectLoadState::Building)
            || matches!(good.load_state(), EffectLoadState::Building)
        {
            broken.poll_load();
            good.poll_load();
            assert!(Instant::now() < until, "effect loader did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            matches!(broken.load_state(), EffectLoadState::Failed { message } if message.contains("compile"))
        );
        assert!(!broken.is_active());
        assert_eq!(good.load_state(), EffectLoadState::Ready);
        assert!(good.is_active());
        assert_eq!(
            gpu.errors.fault_count(),
            faults,
            "build errors must stay off the live renderer"
        );
    }
}
