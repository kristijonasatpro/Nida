//! Sound devices a bus plays on: the Mac's own output, HDMI to a switcher, a
//! USB interface into the desk.
//!
//! Each device runs its callback on its own clock. The mix pushes blocks into
//! an [`OutputRing`]; the callback reads the ring through a [`RateReader`] held
//! at a fixed depth by [`DriftControl`], so a device that runs a little fast or
//! slow never over- or under-runs. A multi-channel interface gets the bus on
//! the pair of outputs starting at `first_channel`, and silence elsewhere.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::core::{OutputRing, SAMPLE_RATE};
use super::live::{LiveMics, LiveMix, LIVE_MOST_CALL, LIVE_MOST_FRAMES};
use super::reader::{DriftControl, Frame, RateReader};

/// How deep a device's ring is held: the output latency the mix adds.
const OUTPUT_DEPTH_SECS: f64 = 0.03;

/// A sound device playing one bus. Dropping it stops the device.
pub struct DeviceOutput {
    failed: Arc<AtomicBool>,
    /// A live bus's: the times its mics' sound ran short (see [`open_live`]).
    underruns: Option<Arc<AtomicU64>>,
    /// How it plays when not as set: another pair (see [`fit_pair`]).
    note: Option<String>,
    /// A live bus's: what its callback did with its mics' queue.
    events: Option<Arc<super::live::LiveEvents>>,
    /// Faded out and playing silence (see [`Self::hush`]); the frames it
    /// has played wholly silent since; its last callback's frames.
    hush: Arc<AtomicBool>,
    silent: Arc<AtomicU64>,
    call_frames: Arc<AtomicU64>,
    _stream: cpal::Stream,
}

/// What a stream's callback needs to fade out and back in (see
/// [`DeviceOutput::hush`]), and to tell how long it has been silent.
#[derive(Clone, Default)]
struct Hush {
    on: Arc<AtomicBool>,
    silent: Arc<AtomicU64>,
    call_frames: Arc<AtomicU64>,
}

impl Hush {
    /// `block` (a callback's) faded toward silence while hushed, back toward
    /// full after; counted.
    fn shade(&self, block: &mut [Frame], heard: &mut f32) {
        let hushed = self.on.load(Ordering::Relaxed);
        self.call_frames.store(block.len() as u64, Ordering::Relaxed);
        // Counted only from a block that starts silent: the fading one is not.
        let silent_through = hushed && *heard == 0.0;
        if hushed || *heard < 1.0 {
            fade(block, heard, hushed);
        }
        if silent_through {
            self.silent.fetch_add(block.len() as u64, Ordering::Relaxed);
        } else if !hushed {
            self.silent.store(0, Ordering::Relaxed);
        }
    }
}

impl DeviceOutput {
    /// Whether the device has reported an error (usually: unplugged).
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// How it plays when not as set: on another pair of outputs.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// A live bus's: the times its mics' sound ran short so far.
    pub fn underruns(&self) -> Option<u64> {
        self.underruns.as_ref().map(|u| u.load(Ordering::Relaxed))
    }

    /// Fade out over 10 ms and play silence from then on — before it is
    /// closed for a stream with a smaller buffer on the same device: the
    /// device replays what it played 250 ms before as it changes its
    /// buffer (Max5: a burst of old sound at every such open), and silence
    /// is what it then replays.
    pub fn hush(&self) {
        self.hush.store(true, Ordering::Relaxed);
    }

    /// Back from [`Self::hush`]: faded in over 10 ms.
    pub fn unhush(&self) {
        self.hush.store(false, Ordering::Relaxed);
    }

    /// Frames played wholly silent since hushed, and its callbacks' size.
    pub fn silent_frames(&self) -> (u64, u64) {
        (
            self.silent.load(Ordering::Relaxed),
            self.call_frames.load(Ordering::Relaxed),
        )
    }

    /// A live bus's: what its callback did with its mics' queue since asked.
    pub fn events(&self) -> Vec<super::live::LiveEvent> {
        self.events.as_ref().map(|e| e.drain()).unwrap_or_default()
    }
}

/// The pair of outputs a bus plays on, on a device with `channels` of them:
/// the pair asked for if the device has it, else its last pair. A bus set to
/// outputs 3-4 on a two-output interface plays on 1-2 — before, it played
/// its left side on output 2 alone and the right nowhere. With it, what to
/// say when that is not the pair asked for (a one-output device mixing a
/// bus set to 1-2 to its one output is as expected, not said).
fn fit_pair(name: &str, first_channel: u16, channels: u16) -> (u16, Option<String>) {
    if first_channel == 0 || first_channel.saturating_add(2) <= channels {
        return (first_channel, None);
    }
    let first = channels.saturating_sub(2);
    let asked = format!(
        "{}-{}",
        u32::from(first_channel) + 1,
        u32::from(first_channel) + 2
    );
    let note = if channels <= 1 {
        format!("'{name}' has one output: plays on it, both sides, not on {asked}")
    } else {
        format!(
            "'{name}' has {channels} outputs: plays on {}-{}, not on {asked}",
            first + 1,
            first + 2
        )
    };
    (first, Some(note))
}

/// Of a device's configurations at the mix rate, the first with `wanted`
/// channels or more; failing that, the one with the most (its pair is then
/// fitted, see [`fit_pair`]).
fn pick_config(
    configs: impl IntoIterator<Item = cpal::SupportedStreamConfig>,
    wanted: u16,
) -> Option<cpal::SupportedStreamConfig> {
    let mut most: Option<cpal::SupportedStreamConfig> = None;
    for config in configs {
        if config.channels() >= wanted {
            return Some(config);
        }
        if most.as_ref().is_none_or(|m| config.channels() > m.channels()) {
            most = Some(config);
        }
    }
    most
}

/// For [`open`]: of a device's configurations, one at the mix rate in a
/// sample format it plays (f32, i16, u16), as [`pick_config`] picks. One
/// in another format is passed over, not chosen for having the pair and
/// then refused — the device's default would have played.
fn pick_playable(
    configs: impl IntoIterator<Item = cpal::SupportedStreamConfigRange>,
    wanted: u16,
) -> Option<cpal::SupportedStreamConfig> {
    pick_config(
        configs
            .into_iter()
            .filter(|c| {
                matches!(
                    c.sample_format(),
                    cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::U16
                )
            })
            .filter_map(|c| c.try_with_sample_rate(SAMPLE_RATE)),
        wanted,
    )
}

/// Play a live bus on the output device called `name`, on the channel pair
/// starting at `first_channel` (fitted, see [`fit_pair`]): its mics (inputs
/// on this same device, one clock) mixed inside the device's own callback
/// at a buffer of `frames` (clamped to what the device takes), each through
/// its processing, fader and balance, then the bus's processing, `bus_gain`
/// and delay (see [`LiveMix`]). No mixing thread, no drift buffers between:
/// the sound of a mic is out about three buffers and the limiter's 1.5 ms
/// after it came in.
///
/// # Errors
///
/// No such device, no 48 kHz configuration, a device that takes no buffer
/// that small, or a stream that cannot be built or started.
pub fn open_live(
    name: &str,
    first_channel: u16,
    frames: u32,
    mics: LiveMics,
    bus_gain: Arc<AtomicU32>,
) -> Result<DeviceOutput> {
    let host = cpal::default_host();
    let device = host
        .output_devices()
        .context("listing sound devices")?
        .find(|d| device_name(d).as_deref() == Some(name))
        .with_context(|| format!("no sound device called '{name}'"))?;
    let supported = pick_config(
        device
            .supported_output_configs()
            .context("its configurations")?
            .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
            .filter_map(|c| c.try_with_sample_rate(SAMPLE_RATE)),
        first_channel.saturating_add(2),
    )
    .context("no 48 kHz configuration")?;
    let frames = match supported.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => frames.clamp(*min, *max),
        cpal::SupportedBufferSize::Unknown => frames,
    };
    anyhow::ensure!(
        frames <= LIVE_MOST_FRAMES,
        "'{name}' takes no buffer under {frames} frames: played the usual way"
    );
    let (first, note) = fit_pair(name, first_channel, supported.channels());
    if let Some(note) = &note {
        log::warn!("Audio out (live): {note}");
    }
    let first = usize::from(first);
    let channels = usize::from(supported.channels().max(1));
    let mut config: cpal::StreamConfig = supported.into();
    config.buffer_size = cpal::BufferSize::Fixed(frames);
    log::info!(
        "Audio out '{name}' live: {} mic(s) mixed in its callback, {frames} frames a call ({:.1} ms)",
        mics.gains.len(),
        f64::from(frames) * 1000.0 / f64::from(SAMPLE_RATE)
    );
    let failed = Arc::new(AtomicBool::new(false));
    let underruns = Arc::new(AtomicU64::new(0));
    let mut mix = LiveMix::new(mics, bus_gain, frames as usize, Arc::clone(&underruns));
    let events = mix.events();
    let hush = Hush::default();
    let hushed = hush.clone();
    let mut heard = 1.0_f32;
    let mut block: Vec<Frame> = vec![[0.0; 2]; LIVE_MOST_CALL];
    let errors = Arc::clone(&failed);
    let overflow = Arc::clone(&underruns);
    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let n = data.len() / channels;
            if n > block.len() {
                // A callback far bigger than asked: silence, counted.
                data.fill(0.0);
                overflow.fetch_add(1, Ordering::Relaxed);
                return;
            }
            mix.mix(&mut block[..n]);
            hushed.shade(&mut block[..n], &mut heard);
            data.fill(0.0);
            for (frame, lr) in data.chunks_mut(channels).zip(&block[..n]) {
                for (c, sample) in frame.iter_mut().enumerate() {
                    *sample = if channels == 1 {
                        f32::midpoint(lr[0], lr[1]).clamp(-1.0, 1.0)
                    } else if c == first {
                        lr[0].clamp(-1.0, 1.0)
                    } else if c == first + 1 {
                        lr[1].clamp(-1.0, 1.0)
                    } else {
                        0.0
                    };
                }
            }
        },
        move |err| {
            log::warn!("Audio output error (live): {err}");
            errors.store(true, Ordering::Relaxed);
        },
        None,
    )?;
    stream.play().context("starting the sound device")?;
    Ok(DeviceOutput {
        failed,
        underruns: Some(underruns),
        events: Some(events),
        hush: hush.on,
        silent: hush.silent,
        call_frames: hush.call_frames,
        note,
        _stream: stream,
    })
}

/// `block` faded from `heard` toward silence (`out`) or back to full, over
/// 10 ms (see [`DeviceOutput::hush`]), `heard` left where it ended.
fn fade(block: &mut [Frame], heard: &mut f32, out: bool) {
    for frame in block {
        *heard = if out {
            (*heard - 1.0 / 480.0).max(0.0)
        } else {
            (*heard + 1.0 / 480.0).min(1.0)
        };
        frame[0] *= *heard;
        frame[1] *= *heard;
    }
}

fn device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_string())
}

/// Every sound output device, by name, with its most outputs (0 when it
/// would not say).
pub fn output_devices() -> Vec<(String, u16)> {
    cpal::default_host()
        .output_devices()
        .map(|devices| {
            devices
                .filter_map(|d| {
                    let channels = d
                        .supported_output_configs()
                        .ok()
                        .and_then(|configs| configs.map(|c| c.channels()).max())
                        .unwrap_or(0);
                    device_name(&d).map(|name| (name, channels))
                })
                .collect()
        })
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
    // The mix rate if the device offers it (no resampling at all), with
    // enough channels for the pair, or as many as it has; otherwise
    // whatever it prefers.
    let config = device
        .supported_output_configs()
        .ok()
        .and_then(|configs| pick_playable(configs, first_channel.saturating_add(2)))
        .map_or_else(|| device.default_output_config(), Ok)
        .context("no usable output configuration")?;
    let rate = config.sample_rate();
    let channels = config.channels();
    let format = config.sample_format();
    log::info!("Audio out '{name}': {rate} Hz, {channels} channel(s), {format:?}");
    let (first_channel, note) = fit_pair(name, first_channel, channels);
    if let Some(note) = &note {
        log::warn!("Audio out: {note}");
    }
    let failed = Arc::new(AtomicBool::new(false));
    let hush = Hush::default();
    let stream_config: cpal::StreamConfig = config.into();
    let stream = match format {
        cpal::SampleFormat::F32 => build::<f32>(
            &device,
            &stream_config,
            ring,
            first_channel,
            failed.clone(),
            hush.clone(),
        )?,
        cpal::SampleFormat::I16 => build::<i16>(
            &device,
            &stream_config,
            ring,
            first_channel,
            failed.clone(),
            hush.clone(),
        )?,
        cpal::SampleFormat::U16 => build::<u16>(
            &device,
            &stream_config,
            ring,
            first_channel,
            failed.clone(),
            hush.clone(),
        )?,
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };
    stream.play().context("starting the sound device")?;
    Ok(DeviceOutput {
        failed,
        underruns: None,
        events: None,
        hush: hush.on,
        silent: hush.silent,
        call_frames: hush.call_frames,
        note,
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
    hush: Hush,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let mut heard = 1.0_f32;
    let channels = usize::from(config.channels.max(1));
    let first = usize::from(first_channel).min(channels.saturating_sub(1));
    let nominal = f64::from(SAMPLE_RATE) / f64::from(config.sample_rate.max(1));
    let depth = (f64::from(SAMPLE_RATE) * OUTPUT_DEPTH_SECS) as usize;
    let mut reader = RateReader::default();
    let mut drift = DriftControl::starting_empty(depth);
    let mut block: Vec<Frame> = Vec::new();
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let frames = data.len() / channels;
            block.clear();
            block.resize(frames, [0.0; 2]);
            if let Ok(mut fifo) = ring.frames.lock() {
                if let Some(step) = drift.step_for(&mut fifo, nominal, frames) {
                    if reader.read(&mut fifo, step, &mut block) < frames {
                        drift.ran_dry();
                    }
                }
            }
            hush.shade(&mut block, &mut heard);
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

#[cfg(test)]
mod tests {
    /// Hushed, a stream fades to silence over 10 ms and stays there,
    /// counting its silent frames; let back, it fades in over 10 ms and
    /// plays as it is.
    #[test]
    fn a_hushed_stream_fades_out_counts_its_silence_and_comes_back() {
        let hush = super::Hush::default();
        let mut heard = 1.0;
        let mut plain = vec![[0.5_f32, -0.5]; 64];
        hush.shade(&mut plain, &mut heard);
        assert!(plain.iter().all(|f| *f == [0.5, -0.5]), "untouched until hushed");
        hush.on.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut block = vec![[0.5_f32, -0.5]; 600];
        hush.shade(&mut block, &mut heard);
        assert!(block[0][0] > 0.49 && block[240][0] > 0.2, "fading");
        assert!(block[480..].iter().all(|f| *f == [0.0, 0.0]), "silent from 10 ms");
        let mut more = vec![[0.5_f32, -0.5]; 64];
        hush.shade(&mut more, &mut heard);
        assert!(more.iter().all(|f| *f == [0.0, 0.0]), "and after");
        assert_eq!(
            hush.silent.load(std::sync::atomic::Ordering::Relaxed),
            64,
            "its silent frames: the block that faded is not"
        );
        hush.on.store(false, std::sync::atomic::Ordering::Relaxed);
        let mut back = vec![[0.5_f32, -0.5]; 600];
        hush.shade(&mut back, &mut heard);
        assert!(back[0][0] < 0.01 && back[479][0] == 0.5 && back[599][0] == 0.5, "faded in");
        assert_eq!(hush.silent.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    /// A bus set to a pair the device does not have plays on its last pair,
    /// said; one it has, or a one-output device's mix of 1-2, as asked.
    #[test]
    fn a_pair_the_device_lacks_plays_on_its_last_pair() {
        assert_eq!(super::fit_pair("Solo", 0, 2), (0, None));
        assert_eq!(super::fit_pair("Octo", 4, 8), (4, None));
        assert_eq!(super::fit_pair("Speaker", 0, 1), (0, None));
        let (first, note) = super::fit_pair("Solo", 2, 2);
        assert_eq!(first, 0, "outputs 1-2, not output 2 alone");
        assert_eq!(
            note.as_deref(),
            Some("'Solo' has 2 outputs: plays on 1-2, not on 3-4")
        );
        assert_eq!(super::fit_pair("Six", 6, 6).0, 4, "5-6");
        let (first, note) = super::fit_pair("Speaker", 2, 1);
        assert_eq!(first, 0);
        assert!(note.is_some_and(|n| n.contains("one output")));
    }

    /// The configuration with enough outputs for the pair, else the one
    /// with the most.
    #[test]
    fn a_config_with_the_pair_else_the_most_outputs() {
        let config = |channels| {
            cpal::SupportedStreamConfig::new(
                channels,
                48_000,
                cpal::SupportedBufferSize::Unknown,
                cpal::SampleFormat::F32,
            )
        };
        let picked = |list: &[u16], wanted| {
            super::pick_config(list.iter().map(|&c| config(c)), wanted).map(|c| c.channels())
        };
        assert_eq!(picked(&[2, 8, 4], 4), Some(8), "the first that fits");
        assert_eq!(picked(&[2, 4, 3], 6), Some(4), "the most");
        assert_eq!(picked(&[], 2), None);
    }

    /// Only a format the usual way plays is picked: a 4-output i32 one
    /// passed over for a 2-output f32 one; none at all, none (the device's
    /// default is then tried).
    #[test]
    fn only_a_playable_format_is_picked() {
        let range = |channels, format| {
            cpal::SupportedStreamConfigRange::new(
                channels,
                44_100,
                48_000,
                cpal::SupportedBufferSize::Unknown,
                format,
            )
        };
        let picked = super::pick_playable(
            [
                range(4, cpal::SampleFormat::I32),
                range(2, cpal::SampleFormat::F32),
            ],
            4,
        )
        .map(|c| (c.channels(), c.sample_format()));
        assert_eq!(picked, Some((2, cpal::SampleFormat::F32)));
        assert!(super::pick_playable([range(8, cpal::SampleFormat::U8)], 2).is_none());
    }
}
