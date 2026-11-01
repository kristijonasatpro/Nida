//! One Audio Unit effect, hosted: asked to run out of Nida's process (a
//! plug-in that crashes takes its own host down, not the show), set to
//! 48 kHz stereo, and fed a block at a time by one thread — its render
//! owner — on one sample timeline. Design and Codex's review:
//! research/2026-10-30-overlays/DESIGN-au-inserts.md.
//!
//! Its render block is synchronous: a plug-in that hangs hangs its caller.
//! So it is never called by the mixer itself (see `insert`), only by a
//! thread of its own the mixer hands blocks to and never waits on.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_audio_toolbox::{
    AUAudioUnit, AUAudioUnitStatus, AudioComponentDescription, AudioComponentInstantiationOptions,
    AudioUnitRenderActionFlags,
};
use objc2_avf_audio::AVAudioFormat;
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp, AudioTimeStampFlags};
use objc2_foundation::{
    NSData, NSDictionary, NSError, NSPropertyListFormat, NSPropertyListReadOptions,
    NSPropertyListSerialization, NSString,
};

use super::reader::Frame;

/// The most frames one render takes: a mixer block is 480; more is split.
pub const MOST_FRAMES: usize = 4096;

/// How long a plug-in may take to load before it is given up.
const LOAD_TIMEOUT: Duration = Duration::from_secs(15);

/// `kAudioUnitErr_TooManyFramesToProcess`.
const TOO_MANY_FRAMES: AUAudioUnitStatus = -10874;

/// An `AudioBufferList` with room for two buffers: the bindings' type has
/// room for one, and a stereo planar list needs two.
#[repr(C)]
struct StereoBuffers {
    number: u32,
    buffers: [AudioBuffer; 2],
}

/// Left and right, each with room for [`MOST_FRAMES`]: made once, never
/// moved (the pull block reads it by address).
struct Planar {
    left: Vec<f32>,
    right: Vec<f32>,
}

impl Planar {
    fn new() -> Self {
        Self {
            left: vec![0.0; MOST_FRAMES],
            right: vec![0.0; MOST_FRAMES],
        }
    }
}

type RenderFn = dyn Fn(
    NonNull<AudioUnitRenderActionFlags>,
    NonNull<AudioTimeStamp>,
    u32,
    isize,
    NonNull<AudioBufferList>,
    *mut block2::DynBlock<PullFn>,
) -> AUAudioUnitStatus;

type PullFn = dyn Fn(
    NonNull<AudioUnitRenderActionFlags>,
    NonNull<AudioTimeStamp>,
    u32,
    isize,
    NonNull<AudioBufferList>,
) -> AUAudioUnitStatus;

/// A hosted effect, ready to render.
pub struct Hosted {
    unit: Retained<AUAudioUnit>,
    render: RcBlock<RenderFn>,
    pull: RcBlock<PullFn>,
    /// What the pull block hands the plug-in: one allocation, reached only
    /// through this pointer (written before a render, read by the pull block
    /// during it), freed in `Drop` once rendering has stopped.
    input: *mut Planar,
    output: Box<Planar>,
    list: Box<StereoBuffers>,
    sample_time: f64,
    /// It runs inside Nida's process after all (some plug-ins and Apple's
    /// own may).
    pub in_process: bool,
    /// The delay it adds, seconds, as it says.
    pub latency_secs: f64,
}

/// A unit handed from the completion handler's queue to the opening thread.
struct Handed(Option<Retained<AUAudioUnit>>, Option<String>);
// SAFETY: the unit is only handed over, then used by the one thread that
// received it; AUAudioUnit may be used from any thread outside rendering.
unsafe impl Send for Handed {}

/// `setMaximumFramesToRender:` as the unit's own class declares it: the
/// proxy of a unit out of process takes an `unsigned long long` where the
/// header has a 32-bit count, and objc2 checks the type in debug builds.
///
/// # Safety
///
/// `unit` not rendering.
unsafe fn set_most_frames(unit: &AUAudioUnit, frames: u32) {
    let wide = unit
        .class()
        .instance_method(objc2::sel!(setMaximumFramesToRender:))
        .and_then(|m| m.argument_type(2))
        .is_some_and(|t| t.to_bytes() == b"Q");
    if wide {
        let () = objc2::msg_send![unit, setMaximumFramesToRender: u64::from(frames)];
    } else {
        unit.setMaximumFramesToRender(frames);
    }
}

/// A component description from `type:subtype:manufacturer` codes.
pub fn description(component: &str) -> Option<AudioComponentDescription> {
    let [t, s, m] = super::plugins::component_codes(component)?;
    Some(AudioComponentDescription {
        componentType: t,
        componentSubType: s,
        componentManufacturer: m,
        componentFlags: 0,
        componentFlagsMask: 0,
    })
}

impl Hosted {
    /// Load the effect `component` ("aufx:lpas:appl") out of process and
    /// make it ready to render 48 kHz stereo. Waits for it to load — call
    /// it on a thread of its own, never the main thread (that can
    /// deadlock). One loaded into Nida's own process is refused unless
    /// `allow_in_process` (Apple's own, trusted).
    ///
    /// # Errors
    ///
    /// Not installed, would not load in time, runs in process when that is
    /// not allowed, takes no 48 kHz stereo, or would not get ready.
    #[cfg(test)]
    pub fn open(component: &str, allow_in_process: bool) -> Result<Self, String> {
        Self::open_as(component, allow_in_process, None)
    }

    /// As [`Self::open`], with its settings as saved (see
    /// [`SharedUnit::settings`]) restored before it gets ready; settings it
    /// will not take are left out, said in the log.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_as(
        component: &str,
        allow_in_process: bool,
        settings: Option<&[u8]>,
    ) -> Result<Self, String> {
        Self::open_with(
            component,
            allow_in_process,
            AudioComponentInstantiationOptions::LoadOutOfProcess,
            settings,
        )
    }

    fn open_with(
        component: &str,
        allow_in_process: bool,
        options: AudioComponentInstantiationOptions,
        settings: Option<&[u8]>,
    ) -> Result<Self, String> {
        let desc =
            description(component).ok_or_else(|| format!("not a component: '{component}'"))?;
        if !installed(desc) {
            return Err(format!("'{component}' is not installed on this Mac"));
        }
        let (tx, rx) = std::sync::mpsc::channel::<Handed>();
        let handler = RcBlock::new(move |unit: *mut AUAudioUnit, error: *mut NSError| {
            // SAFETY: the pointers are the framework's, valid for the call;
            // the unit is retained here, as the documentation asks.
            let unit = unsafe { Retained::retain(unit) };
            let why = (!error.is_null()).then(|| {
                // SAFETY: as above.
                unsafe { (*error).localizedDescription() }.to_string()
            });
            let _ = tx.send(Handed(unit, why));
        });
        // SAFETY: a plain class method; the handler is retained by the call.
        unsafe {
            AUAudioUnit::instantiateWithComponentDescription_options_completionHandler(
                desc, options, &handler,
            );
        }
        let Handed(unit, why) = rx.recv_timeout(LOAD_TIMEOUT).map_err(|_| {
            format!(
                "'{component}' did not load within {} s",
                LOAD_TIMEOUT.as_secs()
            )
        })?;
        let unit =
            unit.ok_or_else(|| why.unwrap_or_else(|| format!("'{component}' would not load")))?;
        // SAFETY: plain property reads and writes on a unit not rendering.
        unsafe {
            let in_process = unit.isLoadedInProcess();
            if in_process && !allow_in_process {
                return Err(format!(
                    "'{component}' would run inside Nida, not on its own"
                ));
            }
            let format = AVAudioFormat::initStandardFormatWithSampleRate_channels(
                AVAudioFormat::alloc(),
                f64::from(super::SAMPLE_RATE),
                2,
            )
            .ok_or("no 48 kHz stereo format")?;
            let (inputs, outputs) = (unit.inputBusses(), unit.outputBusses());
            if inputs.count() < 1 || outputs.count() < 1 {
                return Err(format!("'{component}' has no input or no output"));
            }
            for bus in [
                inputs.objectAtIndexedSubscript(0),
                outputs.objectAtIndexedSubscript(0),
            ] {
                // The bindings leave `setFormat:error:` out (AVFAudio would
                // depend on itself): sent by hand.
                let set: Result<(), Retained<NSError>> =
                    objc2::msg_send![&*bus, setFormat: &*format, error: _];
                set.map_err(|e| {
                    format!(
                        "'{component}' takes no 48 kHz stereo: {}",
                        e.localizedDescription()
                    )
                })?;
                // A bus starts disabled: rendered so, it pulls no input and
                // fails (kAudioUnitErr_NoConnection).
                bus.setEnabled(true);
            }
            set_most_frames(&unit, MOST_FRAMES as u32);
            unit.setRenderingOffline(false);
            // Its settings as saved, before it gets ready (a plug-in may
            // size itself by them).
            if let Some(bytes) = settings {
                match settings_dictionary(bytes) {
                    Some(saved) => unit.setFullStateForDocument(Some(&saved)),
                    None => log::warn!(
                        "Audio insert '{component}': its saved settings unreadable: left out"
                    ),
                }
            }
            // Fetched before its resources are made, as the documentation
            // asks, and kept.
            let render = RcBlock::copy(unit.renderBlock().cast::<block2::DynBlock<RenderFn>>())
                .ok_or_else(|| format!("'{component}' gave no render block"))?;
            unit.allocateRenderResourcesAndReturnError().map_err(|e| {
                format!(
                    "'{component}' would not get ready: {}",
                    e.localizedDescription()
                )
            })?;
            let input: *mut Planar = Box::into_raw(Box::new(Planar::new()));
            let source = input as usize;
            let pull = RcBlock::new(
                move |_flags: NonNull<AudioUnitRenderActionFlags>,
                      _time: NonNull<AudioTimeStamp>,
                      frames: u32,
                      _bus: isize,
                      list: NonNull<AudioBufferList>|
                      -> AUAudioUnitStatus {
                    let n = frames as usize;
                    if n > MOST_FRAMES {
                        return TOO_MANY_FRAMES;
                    }
                    // SAFETY: `source` is the Hosted's input allocation,
                    // freed only after rendering stopped, and not written
                    // while a render runs; the list is the plug-in's, with
                    // `mNumberBuffers` buffers.
                    let input = &*(source as *const Planar);
                    let list = list.as_ptr();
                    let count = (*list).mNumberBuffers as usize;
                    let first = std::ptr::addr_of_mut!((*list).mBuffers).cast::<AudioBuffer>();
                    for i in 0..count.min(2) {
                        let buffer = &mut *first.add(i);
                        let side = if i == 0 { &input.left } else { &input.right };
                        if buffer.mData.is_null() {
                            buffer.mData = side.as_ptr().cast_mut().cast::<c_void>();
                        } else {
                            std::ptr::copy_nonoverlapping(
                                side.as_ptr(),
                                buffer.mData.cast::<f32>(),
                                n,
                            );
                        }
                        buffer.mDataByteSize = (n * 4) as u32;
                    }
                    0
                },
            );
            let latency_secs = unit.latency();
            Ok(Self {
                unit,
                render,
                pull,
                input,
                output: Box::new(Planar::new()),
                list: Box::new(StereoBuffers {
                    number: 2,
                    buffers: [AudioBuffer {
                        mNumberChannels: 1,
                        mDataByteSize: 0,
                        mData: std::ptr::null_mut(),
                    }; 2],
                }),
                sample_time: 0.0,
                in_process,
                latency_secs,
            })
        }
    }

    /// Run `block` through the effect in place. On an error from it, the
    /// block is left as it came (dry) and the code returned.
    ///
    /// # Errors
    ///
    /// The plug-in's own status, non-zero.
    pub fn process(&mut self, block: &mut [Frame]) -> Result<(), AUAudioUnitStatus> {
        for part in block.chunks_mut(MOST_FRAMES) {
            self.process_part(part)?;
        }
        Ok(())
    }

    fn process_part(&mut self, block: &mut [Frame]) -> Result<(), AUAudioUnitStatus> {
        let n = block.len();
        {
            // SAFETY: our own allocation; the pull block reads it only during
            // the render below, after this borrow has ended.
            let input = unsafe { &mut *self.input };
            for (i, f) in block.iter().enumerate() {
                input.left[i] = f[0];
                input.right[i] = f[1];
            }
        }
        let bytes = (n * 4) as u32;
        self.list.number = 2;
        self.list.buffers[0] = AudioBuffer {
            mNumberChannels: 1,
            mDataByteSize: bytes,
            mData: self.output.left.as_mut_ptr().cast::<c_void>(),
        };
        self.list.buffers[1] = AudioBuffer {
            mNumberChannels: 1,
            mDataByteSize: bytes,
            mData: self.output.right.as_mut_ptr().cast::<c_void>(),
        };
        let mut flags = AudioUnitRenderActionFlags(0);
        // SAFETY: a plain C struct; all zero is "nothing valid".
        let mut time: AudioTimeStamp = unsafe { std::mem::zeroed() };
        time.mSampleTime = self.sample_time;
        time.mFlags = AudioTimeStampFlags::SampleTimeValid;
        // The timeline moves on by what was asked, whatever comes back.
        self.sample_time += n as f64;
        let pull = std::ptr::from_ref::<block2::DynBlock<PullFn>>(&self.pull).cast_mut();
        let list = NonNull::from(&mut *self.list).cast::<AudioBufferList>();
        // The render block is the plug-in's, kept since it was made ready;
        // the list, its buffers and the pull block's input are ours and
        // outlive the call; called by this one thread only.
        let status = self.render.call((
            NonNull::from(&mut flags),
            NonNull::from(&mut time),
            n as u32,
            0,
            list,
            pull,
        ));
        if status != 0 {
            return Err(status);
        }
        // It may have pointed the buffers at memory of its own.
        let (left, right) = (self.list.buffers[0].mData, self.list.buffers[1].mData);
        if left.is_null() || right.is_null() {
            return Err(-50);
        }
        for (i, frame) in block.iter_mut().enumerate() {
            // SAFETY: each buffer holds `n` floats, ours or the plug-in's
            // (valid until it next renders).
            let (l, r) = unsafe { (*left.cast::<f32>().add(i), *right.cast::<f32>().add(i)) };
            *frame = [
                if l.is_finite() { l } else { 0.0 },
                if r.is_finite() { r } else { 0.0 },
            ];
        }
        Ok(())
    }
}

impl super::insert::Effect for Hosted {
    fn process(&mut self, block: &mut [Frame]) -> Result<(), i32> {
        Hosted::process(self, block)
    }

    fn jumped(&mut self, frames: u64) {
        Hosted::jumped(self, frames);
    }

    fn settings(&self) -> Option<super::insert::ReadSettings> {
        let unit = self.shared();
        Some(Box::new(move || unit.settings()))
    }

    fn handle(&self) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        Some(std::sync::Arc::new(self.shared()))
    }

    fn latency_secs(&self) -> f64 {
        self.latency_secs
    }
}

/// Whether an effect of `desc` is installed here: asked before loading it, so
/// a show from another Mac says so plainly, not as a framework error code.
fn installed(desc: AudioComponentDescription) -> bool {
    objc2::rc::autoreleasepool(|_| {
        // SAFETY: a plain query of the shared manager, off the main thread.
        unsafe {
            objc2_avf_audio::AVAudioUnitComponentManager::sharedAudioUnitComponentManager()
                .componentsMatchingDescription(desc)
                .count()
                > 0
        }
    })
}

/// A hosted unit, for its window and its settings, from threads other than
/// its render thread — as Apple's hosts use it (the main thread asks for its
/// view; a control thread reads its state) while it renders.
#[derive(Clone)]
pub struct SharedUnit(pub Retained<AUAudioUnit>);

// SAFETY: AUAudioUnit is made to be used from the main or a control thread
// for its view and state while its render block runs on the render thread;
// Nida never renders through this.
unsafe impl Send for SharedUnit {}
// SAFETY: as above.
unsafe impl Sync for SharedUnit {}

impl SharedUnit {
    /// Its settings as a show saves them: its document state
    /// (`fullStateForDocument`), as a binary property list. A plug-in out
    /// of process answers over its connection: call on a thread that may
    /// wait, never the main or the render thread.
    pub fn settings(&self) -> Option<Vec<u8>> {
        objc2::rc::autoreleasepool(|_| {
            // SAFETY: a property read and a serialisation, on a control
            // thread, of objects owned here.
            unsafe {
                let state = self.0.fullStateForDocument()?;
                let data = NSPropertyListSerialization::dataWithPropertyList_format_options_error(
                    &state,
                    NSPropertyListFormat::BinaryFormat_v1_0,
                    0,
                )
                .ok()?;
                Some(data.to_vec())
            }
        })
    }
}

/// Saved settings back to the dictionary a unit takes.
fn settings_dictionary(
    bytes: &[u8],
) -> Option<Retained<NSDictionary<NSString, objc2::runtime::AnyObject>>> {
    let data = NSData::with_bytes(bytes);
    // SAFETY: plain parsing of our own bytes; the result is checked to be a
    // dictionary before it is used as one.
    unsafe {
        let parsed = NSPropertyListSerialization::propertyListWithData_options_format_error(
            &data,
            NSPropertyListReadOptions::Immutable,
            std::ptr::null_mut(),
        )
        .ok()?;
        let dictionary = parsed.downcast::<NSDictionary>().ok()?;
        Some(Retained::cast_unchecked(dictionary))
    }
}

impl Hosted {
    /// The unit, for its window and its settings (see [`SharedUnit`]).
    pub fn shared(&self) -> SharedUnit {
        SharedUnit(self.unit.clone())
    }

    /// The sound jumped (the mixer held up, a block not handed over):
    /// `frames` passed unheard. What the plug-in held — a reverb's tail, a
    /// delay's echoes — is from before: cleared, and its time moved on.
    pub fn jumped(&mut self, frames: u64) {
        // SAFETY: between renders, on its one render thread.
        unsafe { self.unit.reset() };
        self.sample_time += frames as f64;
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        // SAFETY: rendering has stopped: this thread was its only caller;
        // the input allocation is ours, read by nothing from here on.
        unsafe {
            self.unit.deallocateRenderResources();
            drop(Box::from_raw(self.input));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Hosted;
    use crate::internal::audio_mix::reader::Frame;

    fn sine(hz: f64, secs: f64) -> Vec<Frame> {
        (0..(secs * 48_000.0) as usize)
            .map(|i| {
                let v = (0.5 * (std::f64::consts::TAU * hz * i as f64 / 48_000.0).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    fn through(effect: &mut Hosted, mut sound: Vec<Frame>) -> f32 {
        for block in sound.chunks_mut(480) {
            effect.process(block).expect("rendered");
        }
        // Settled: the second half.
        sound[sound.len() / 2..]
            .iter()
            .map(|f| f[0].abs().max(f[1].abs()))
            .fold(0.0, f32::max)
    }

    /// Apple's low-pass, hosted: a high tone comes out well down, a low one
    /// as it went in.
    #[test]
    fn apples_low_pass_is_hosted_and_heard() {
        let mut effect = Hosted::open("aufx:lpas:appl", true).expect("AULowpass");
        let high = through(&mut effect, sine(18_000.0, 1.0));
        let low = through(&mut effect, sine(200.0, 1.0));
        assert!(high < 0.5 * 0.35, "18 kHz came out at {high}");
        assert!((low - 0.5).abs() < 0.05, "200 Hz came out at {low}");
    }

    /// Out of process as asked, and what a 10 ms block costs that way.
    #[test]
    fn apples_low_pass_runs_out_of_process_in_time() {
        let mut effect = Hosted::open("aufx:lpas:appl", false).expect("out of process");
        assert!(!effect.in_process);
        let mut block = vec![[0.1_f32; 2]; 480];
        let mut worst = std::time::Duration::ZERO;
        let start = std::time::Instant::now();
        for _ in 0..500 {
            let one = std::time::Instant::now();
            effect.process(&mut block).expect("rendered");
            worst = worst.max(one.elapsed());
        }
        let mean = start.elapsed() / 500;
        eprintln!("out of process, 480 frames: mean {mean:?}, worst {worst:?}");
        assert!(mean < std::time::Duration::from_millis(2), "mean {mean:?}");
    }

    /// An effect's settings, read as a show saves them, come back on a new
    /// one: Apple's low-pass set to 1 kHz is at 1 kHz again, and sounds it.
    #[test]
    fn an_effects_settings_come_back_on_a_new_one() {
        let cutoff = |effect: &Hosted| {
            // SAFETY: plain parameter reads on a unit not rendering.
            unsafe {
                effect
                    .unit
                    .parameterTree()
                    .and_then(|tree| tree.parameterWithAddress(0))
                    .map(|p| p.value())
            }
        };
        let set = Hosted::open("aufx:lpas:appl", true).expect("AULowpass");
        let default = cutoff(&set).expect("a cutoff");
        // SAFETY: as above.
        unsafe {
            set.unit
                .parameterTree()
                .and_then(|tree| tree.parameterWithAddress(0))
                .expect("its cutoff")
                .setValue(1_000.0);
        }
        let settings = set.shared().settings().expect("its settings");
        let mut fresh = Hosted::open_as("aufx:lpas:appl", true, Some(&settings)).expect("again");
        assert!(
            (cutoff(&fresh).expect("a cutoff") - 1_000.0).abs() < 1.0,
            "was {default}"
        );
        let mut plain = Hosted::open("aufx:lpas:appl", true).expect("a plain one");
        let at_3k_set = through(&mut fresh, sine(3_000.0, 0.5));
        let at_3k_plain = through(&mut plain, sine(3_000.0, 0.5));
        assert!(
            at_3k_set < at_3k_plain * 0.5,
            "{at_3k_set} not well under {at_3k_plain}"
        );
        let unreadable =
            Hosted::open_as("aufx:lpas:appl", true, Some(b"not a plist")).expect("loads");
        assert!(
            (cutoff(&unreadable).expect("a cutoff") - default).abs() < 1.0,
            "left out"
        );
    }

    #[test]
    fn a_component_is_read_from_its_codes() {
        let d = super::description("aufx:lpas:appl").expect("codes");
        assert_eq!(d.componentType, 0x6175_6678);
        assert_eq!(d.componentSubType, u32::from_be_bytes(*b"lpas"));
        assert!(super::description("aufx:lpas").is_none());
        assert!(super::description("aufx:lpass:appl").is_none());
    }
}
