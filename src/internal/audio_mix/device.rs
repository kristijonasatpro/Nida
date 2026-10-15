//! Sound devices a bus plays on: the Mac's own output, HDMI to a switcher, a
//! USB interface into the desk.
//!
//! Each device runs its callback on its own clock. The mix pushes blocks into
//! an [`OutputRing`]; the callback reads the ring through a [`RateReader`] held
//! at a fixed depth by [`DriftControl`], so a device that runs a little fast or
//! slow never over- or under-runs. A multi-channel interface gets the bus on
//! the pair of outputs starting at `first_channel`, and silence elsewhere.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::core::{OutputRing, SAMPLE_RATE};
use super::reader::{DriftControl, Frame, RateReader};

/// How deep a device's ring is held: the output latency the mix adds.
const OUTPUT_DEPTH_SECS: f64 = 0.03;

/// A sound device playing one bus. Dropping it stops the device.
pub struct DeviceOutput {
    failed: Arc<AtomicBool>,
    _stream: cpal::Stream,
}

impl DeviceOutput {
    /// Whether the device has reported an error (usually: unplugged).
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

fn device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_string())
}

/// Every sound output device, by name.
pub fn output_device_names() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|devices| devices.filter_map(|d| device_name(&d)).collect())
        .unwrap_or_default()
}

/// Every sound input device, by name, with its channel count.
pub fn input_devices() -> Vec<(String, u16)> {
    cpal::default_host()
        .input_devices()
        .map(|devices| {
            devices
                .filter_map(|d| {
                    let name = device_name(&d)?;
                    let channels = d.default_input_config().map_or(2, |c| c.channels());
                    Some((name, channels))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Start playing `ring` on the output device called `name`, on the channel
/// pair starting at `first_channel`.
///
/// # Errors
///
/// Returns an error if no output device has that name, if it has no usable
/// configuration, if its sample format is not f32/i16/u16, or if the stream
/// cannot be built or started.
pub fn open(name: &str, first_channel: u16, ring: Arc<OutputRing>) -> Result<DeviceOutput> {
    let host = cpal::default_host();
    let device = host
        .output_devices()
        .context("listing sound devices")?
        .find(|d| device_name(d).as_deref() == Some(name))
        .with_context(|| format!("no sound device called '{name}'"))?;
    let wanted_channels = first_channel.saturating_add(2);
    // The mix rate if the device offers it (no resampling at all), with
    // enough channels for the pair; otherwise whatever it prefers.
    let config = device
        .supported_output_configs()
        .ok()
        .and_then(|mut configs| {
            configs.find_map(|c| {
                (c.channels() >= wanted_channels)
                    .then(|| c.try_with_sample_rate(SAMPLE_RATE))
                    .flatten()
            })
        })
        .map_or_else(|| device.default_output_config(), Ok)
        .context("no usable output configuration")?;
    let rate = config.sample_rate();
    let channels = config.channels();
    let format = config.sample_format();
    log::info!("Audio out '{name}': {rate} Hz, {channels} channel(s), {format:?}");
    let failed = Arc::new(AtomicBool::new(false));
    let stream_config: cpal::StreamConfig = config.into();
    let stream = match format {
        cpal::SampleFormat::F32 => {
            build::<f32>(&device, &stream_config, ring, first_channel, failed.clone())?
        }
        cpal::SampleFormat::I16 => {
            build::<i16>(&device, &stream_config, ring, first_channel, failed.clone())?
        }
        cpal::SampleFormat::U16 => {
            build::<u16>(&device, &stream_config, ring, first_channel, failed.clone())?
        }
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };
    stream.play().context("starting the sound device")?;
    Ok(DeviceOutput {
        failed,
        _stream: stream,
    })
}

/// A sound device read as an input — an app's sound, through its tap's
/// device. Dropping it stops the device.
pub struct DeviceInput {
    failed: Arc<AtomicBool>,
    _stream: cpal::Stream,
}

impl DeviceInput {
    /// Whether the device has reported an error (gone).
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

/// Start reading the input device called `name`: its raw PCM on the returned
/// channel, with its rate and channel count.
///
/// # Errors
///
/// Returns an error if no input device has that name, it has no f32 input
/// configuration, or the stream cannot be built or started.
pub fn open_input(name: &str) -> Result<(DeviceInput, super::LiveFeed)> {
    let host = cpal::default_host();
    let device = host
        .input_devices()
        .context("listing sound devices")?
        .find(|d| device_name(d).as_deref() == Some(name))
        .with_context(|| format!("no sound input called '{name}'"))?;
    let config = device
        .default_input_config()
        .context("no usable input configuration")?;
    if config.sample_format() != cpal::SampleFormat::F32 {
        anyhow::bail!("unsupported sample format {:?}", config.sample_format());
    }
    let rate = config.sample_rate();
    let channels = config.channels();
    log::info!("Audio in '{name}': {rate} Hz, {channels} channel(s)");
    let failed = Arc::new(AtomicBool::new(false));
    let (tx, rx) = crossbeam_channel::bounded::<crate::audio::PcmChunk>(64);
    // Samples dropped on a full channel, owed as silence before the next
    // chunk (see `PcmChunk::lost_before`).
    let mut lost = 0_u64;
    let failure = failed.clone();
    let stream = device
        .build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let chunk = crate::audio::PcmChunk {
                    samples: data.to_vec(),
                    lost_before: lost,
                };
                match tx.try_send(chunk) {
                    Ok(()) => lost = 0,
                    Err(crossbeam_channel::TrySendError::Full(chunk)) => {
                        lost = chunk.lost_before + chunk.samples.len() as u64;
                    }
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
                }
            },
            move |e| {
                log::warn!("Sound input error: {e}");
                failure.store(true, Ordering::Relaxed);
            },
            None,
        )
        .context("building the input stream")?;
    stream.play().context("starting the sound input")?;
    Ok((
        DeviceInput {
            failed,
            _stream: stream,
        },
        super::LiveFeed { rx, rate, channels },
    ))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: Arc<OutputRing>,
    first_channel: u16,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = usize::from(config.channels.max(1));
    let first = usize::from(first_channel).min(channels.saturating_sub(1));
    let nominal = f64::from(SAMPLE_RATE) / f64::from(config.sample_rate.max(1));
    let depth = (f64::from(SAMPLE_RATE) * OUTPUT_DEPTH_SECS) as usize;
    let mut reader = RateReader::default();
    let mut drift = DriftControl::new(depth);
    let mut block: Vec<Frame> = Vec::new();
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let frames = data.len() / channels;
            block.clear();
            block.resize(frames, [0.0; 2]);
            if let Ok(mut fifo) = ring.frames.lock() {
                if let Some(step) = drift.step(&mut fifo, nominal) {
                    if reader.read(&mut fifo, step, &mut block) < frames {
                        drift.ran_dry();
                    }
                }
            }
            for (frame, lr) in data.chunks_mut(channels).zip(&block) {
                for (c, sample) in frame.iter_mut().enumerate() {
                    let v = if channels == 1 {
                        f32::midpoint(lr[0], lr[1])
                    } else if c == first {
                        lr[0]
                    } else if c == first + 1 {
                        lr[1]
                    } else {
                        0.0
                    };
                    *sample = T::from_sample(v.clamp(-1.0, 1.0));
                }
            }
        },
        move |err| {
            log::warn!("Audio output error: {err}");
            failed.store(true, Ordering::Relaxed);
        },
        None,
    )?;
    Ok(stream)
}
