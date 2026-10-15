//! An app's sound as an input — a Zoom or Teams call's, a browser's — taken
//! straight from the app with a Core Audio process tap (macOS 14.2 and later),
//! so it reaches the mixer with no virtual cable or driver installed.
//!
//! A tap is wrapped in a private aggregate device, as Apple's own example
//! does it: the tap gives the device its input, a sound output its clock. The
//! clock is a device with no inputs of its own — a USB interface's microphones
//! would come in beside the tap — and the device is checked to hold exactly
//! the tap's two channels before it is used. The mixer opens it by its name
//! like any input (`audio_mix::device::open_input`); being private, only this
//! process sees it. macOS asks once whether Nida may record other apps' sound.
//!
//! The app is muted on the Mac's own speakers while Nida takes its sound, so a
//! room does not hear a call twice: it is heard through Nida's buses.

/// An app making sound, or that has, which can be taken as an input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct RunningApp {
    pub bundle_id: String,
    /// What the app calls itself ("zoom.us"), or its bundle ID.
    pub name: String,
    /// Whether it is making sound now.
    pub playing: bool,
}

/// The device an app's sound is listed under, as an input's.
pub fn device_name(bundle_id: &str) -> String {
    format!("App sound: {bundle_id}")
}

/// The application a process belongs to, from its executable's path: the
/// outermost `.app` bundle in it — a helper's ("…/Google Chrome.app/…/Google
/// Chrome Helper.app/…") is its app's — or none outside any app. Not by
/// bundle ID: "com.google.Chrome.canary" is an app of its own, not Chrome's.
pub fn owning_app(path: &str) -> Option<&str> {
    // Lowercased in ASCII only, so its byte offsets are the path's.
    let lower = path.to_ascii_lowercase();
    let end = lower.find(".app/").map(|at| at + 4).or_else(|| {
        std::path::Path::new(path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
            .then_some(path.len())
    })?;
    Some(&path[..end])
}

/// The apps among `processes` (the app each belongs to, making sound now):
/// one entry an app, making sound when any of its processes is; those making
/// sound first, then by bundle ID. No app, no entry.
pub fn apps_of(processes: &[(String, bool)]) -> Vec<(String, bool)> {
    let mut apps: Vec<(String, bool)> = Vec::new();
    for (app, playing) in processes {
        if app.is_empty() {
            continue;
        }
        match apps.iter_mut().find(|(known, _)| known == app) {
            Some((_, any)) => *any |= *playing,
            None => apps.push((app.clone(), *playing)),
        }
    }
    apps.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    apps
}

/// Whether a tap's device has, as input, exactly the tap's two channels —
/// its clock adding none of its own. `channels` holds each input stream's.
pub fn only_the_tap(channels: &[u32]) -> bool {
    channels.iter().sum::<u32>() == 2
}

#[cfg(target_os = "macos")]
pub use mac::{app_name, processes, Tap};

#[cfg(not(target_os = "macos"))]
pub use other::{app_name, processes, Tap};

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_void, CStr};
    use std::ptr::NonNull;

    use anyhow::{bail, Context as _, Result};
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::AllocAnyThread as _;
    use objc2_core_audio::{
        kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
        kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
        kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
        kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
        kAudioDevicePropertyDeviceUID, kAudioDevicePropertyStreams,
        kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
        kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
        kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject, kAudioProcessPropertyBundleID,
        kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID,
        kAudioStreamPropertyVirtualFormat, kAudioSubDeviceUIDKey,
        kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, AudioHardwareCreateAggregateDevice,
        AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
        AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData,
        AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
        CATapDescription, CATapMuteBehavior,
    };
    use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_foundation::{NSArray, NSBundle, NSNumber, NSString};

    use super::{device_name, only_the_tap, owning_app};

    const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

    fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: scope,
            mElement: kAudioObjectPropertyElementMain,
        }
    }

    /// A property of fixed size, read into `T`.
    fn read<T: Copy>(object: AudioObjectID, selector: u32, scope: u32, mut value: T) -> Option<T> {
        let mut at = address(selector, scope);
        let mut size = u32::try_from(std::mem::size_of::<T>()).ok()?;
        // SAFETY: `value` is a `T` of `size` bytes, the size the property is
        // asked for; Core Audio writes at most that.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&mut at),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast::<c_void>(),
            )
        };
        (status == 0).then_some(value)
    }

    /// A property that is a list of object IDs.
    fn read_ids(object: AudioObjectID, selector: u32, scope: u32) -> Vec<AudioObjectID> {
        let mut at = address(selector, scope);
        let mut size = 0_u32;
        // SAFETY: asks only for the size, into `size`.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(
                object,
                NonNull::from(&mut at),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
            )
        };
        let count = size as usize / std::mem::size_of::<AudioObjectID>();
        if status != 0 || count == 0 {
            return Vec::new();
        }
        let mut ids = vec![0 as AudioObjectID; count];
        // SAFETY: `ids` holds `size` bytes, as the property reported.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&mut at),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(ids.as_mut_ptr().cast::<c_void>()).expect("a vector is non-null"),
            )
        };
        if status != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / std::mem::size_of::<AudioObjectID>());
        ids
    }

    /// A property that is a string, handed over by Core Audio.
    fn read_string(object: AudioObjectID, selector: u32) -> Option<CFRetained<CFString>> {
        let raw = read::<*const CFString>(
            object,
            selector,
            kAudioObjectPropertyScopeGlobal,
            std::ptr::null(),
        )?;
        // SAFETY: the bundle ID and device UID properties make the caller
        // responsible for releasing the string; taking it does, when dropped.
        NonNull::new(raw.cast_mut()).map(|raw| unsafe { CFRetained::from_raw(raw) })
    }

    /// A stream's format, as `AudioStreamBasicDescription` lays it out.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct StreamFormat {
        sample_rate: f64,
        format_id: u32,
        format_flags: u32,
        bytes_per_packet: u32,
        frames_per_packet: u32,
        bytes_per_frame: u32,
        channels_per_frame: u32,
        bits_per_channel: u32,
        reserved: u32,
    }

    /// The channels of each of a device's input streams.
    fn input_channels(device: AudioObjectID) -> Vec<u32> {
        read_ids(device, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput)
            .into_iter()
            .map(|stream| {
                read(
                    stream,
                    kAudioStreamPropertyVirtualFormat,
                    kAudioObjectPropertyScopeGlobal,
                    StreamFormat::default(),
                )
                .map_or(0, |f| f.channels_per_frame)
            })
            .collect()
    }

    fn streams(device: AudioObjectID, scope: u32) -> usize {
        read_ids(device, kAudioDevicePropertyStreams, scope).len()
    }

    /// The UID of a sound output with no inputs, to clock a tap: the default
    /// output when it has none, else the first such device there is.
    fn output_only_clock() -> Option<CFRetained<CFString>> {
        let output_only = |device: AudioObjectID| {
            streams(device, kAudioObjectPropertyScopeOutput) > 0
                && streams(device, kAudioObjectPropertyScopeInput) == 0
        };
        let default = read(
            SYSTEM,
            kAudioHardwarePropertyDefaultOutputDevice,
            kAudioObjectPropertyScopeGlobal,
            0 as AudioObjectID,
        )
        .filter(|&device| device != 0 && output_only(device));
        let device = default.or_else(|| {
            read_ids(SYSTEM, kAudioHardwarePropertyDevices, kAudioObjectPropertyScopeGlobal)
                .into_iter()
                .find(|&device| output_only(device))
        })?;
        read_string(device, kAudioDevicePropertyDeviceUID)
    }

    /// The path of a process's executable.
    fn executable(pid: i32) -> Option<String> {
        let mut buffer = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `buffer` holds the size passed.
        let written = unsafe {
            libc::proc_pidpath(
                pid,
                buffer.as_mut_ptr().cast::<c_void>(),
                u32::try_from(buffer.len()).ok()?,
            )
        };
        let written = usize::try_from(written).ok().filter(|&n| n > 0)?;
        buffer.truncate(written);
        String::from_utf8(buffer).ok()
    }

    /// The bundle ID of the app bundle at `path`.
    fn bundle_id_at(path: &str) -> Option<String> {
        let bundle = NSBundle::bundleWithPath(&NSString::from_str(path))?;
        bundle.bundleIdentifier().map(|id| id.to_string())
    }

    /// Every process Core Audio knows — Nida itself left out — as (its audio
    /// object, the bundle ID of the app it belongs to, making sound now). A
    /// helper is its app's (see [`owning_app`]); a process outside any app
    /// goes by its own bundle ID.
    pub fn processes() -> Vec<(AudioObjectID, String, bool)> {
        autoreleasepool(|_| {
            let own = i32::try_from(std::process::id()).unwrap_or(-1);
            read_ids(
                SYSTEM,
                kAudioHardwarePropertyProcessObjectList,
                kAudioObjectPropertyScopeGlobal,
            )
            .into_iter()
            .filter_map(|process| {
                let pid = read(process, kAudioProcessPropertyPID, kAudioObjectPropertyScopeGlobal, -1_i32)?;
                if pid == own {
                    return None;
                }
                let own_id = read_string(process, kAudioProcessPropertyBundleID)
                    .map(|id| id.to_string())
                    .unwrap_or_default();
                let app = executable(pid)
                    .as_deref()
                    .and_then(owning_app)
                    .and_then(bundle_id_at)
                    .unwrap_or(own_id);
                let playing = read(
                    process,
                    kAudioProcessPropertyIsRunningOutput,
                    kAudioObjectPropertyScopeGlobal,
                    0_u32,
                )
                .is_some_and(|on| on != 0);
                Some((process, app, playing))
            })
            .collect()
        })
    }

    /// What an app calls itself, from its bundle; its bundle ID otherwise.
    pub fn app_name(bundle_id: &str) -> String {
        use objc2_app_kit::NSRunningApplication;
        autoreleasepool(|_| {
            let id = NSString::from_str(bundle_id);
            let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&id);
            apps.firstObject()
                .and_then(|app| app.localizedName())
                .map_or_else(|| bundle_id.to_string(), |name| name.to_string())
        })
    }

    fn cf_key(key: &CStr) -> CFRetained<CFString> {
        CFString::from_str(key.to_str().unwrap_or_default())
    }

    /// An app's sound, tapped: the tap, the private device it is read through
    /// and that device's name. Dropping it takes both away and gives the app
    /// its speakers back.
    pub struct Tap {
        process: AudioObjectID,
        aggregate: AudioObjectID,
        device: String,
    }

    impl Tap {
        /// Tap the processes `processes` of the app `bundle_id`, muted on the
        /// Mac's own speakers while tapped.
        ///
        /// # Errors
        ///
        /// When there is nothing to tap, no output without inputs to clock
        /// it, Core Audio refuses the tap or its device, or the device holds
        /// more than the tap's sound.
        pub fn create(bundle_id: &str, processes: &[AudioObjectID]) -> Result<Self> {
            if processes.is_empty() {
                bail!("{bundle_id} is not running");
            }
            autoreleasepool(|_| {
                let clock = output_only_clock()
                    .context("no sound output without inputs of its own to clock it")?;
                let numbers: Vec<Retained<NSNumber>> =
                    processes.iter().map(|&p| NSNumber::new_u32(p)).collect();
                let list = NSArray::from_retained_slice(&numbers);
                // SAFETY: a fresh description, from a list of process objects.
                let description = unsafe {
                    CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &list)
                };
                // SAFETY: plain setters on the description made above.
                unsafe {
                    description.setName(&NSString::from_str(&format!("Nida: {bundle_id}")));
                    description.setPrivate(true);
                    description.setMuteBehavior(CATapMuteBehavior::MutedWhenTapped);
                }
                let mut tap: AudioObjectID = 0;
                // SAFETY: the description is valid; `tap` receives its object.
                let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &raw mut tap) };
                if status != 0 {
                    bail!("the tap was refused (status {status})");
                }
                // SAFETY: the description's UUID, a plain getter.
                let tap_uid = unsafe { description.UUID() }.UUIDString().to_string();
                let made = aggregate(bundle_id, &clock, &tap_uid).and_then(|(aggregate, device)| {
                    let channels = input_channels(aggregate);
                    if only_the_tap(&channels) {
                        Ok(Self {
                            process: tap,
                            aggregate,
                            device,
                        })
                    } else {
                        // SAFETY: the device made just above.
                        unsafe { AudioHardwareDestroyAggregateDevice(aggregate) };
                        bail!("its device has input channels {channels:?}, not only the tap's two")
                    }
                });
                if made.is_err() {
                    // SAFETY: the tap made above, in no device now.
                    unsafe { AudioHardwareDestroyProcessTap(tap) };
                }
                made
            })
        }

        /// The name of the device the tap is read through: this tap's alone.
        pub fn device(&self) -> &str {
            &self.device
        }
    }

    /// The private device a tap is read through, clocked by `clock`, and its
    /// name — numbered, so two taps of one app are never confused.
    fn aggregate(
        bundle_id: &str,
        clock: &CFString,
        tap_uid: &str,
    ) -> Result<(AudioObjectID, String)> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static MADE: AtomicU32 = AtomicU32::new(0);
        let number = MADE.fetch_add(1, Ordering::Relaxed);
        let device = format!("{} ({number})", device_name(bundle_id));
        let sub = CFDictionary::<CFString, CFType>::from_slices(
            &[&*cf_key(kAudioSubDeviceUIDKey)],
            &[clock.as_ref()],
        );
        let subs = CFArray::<CFDictionary>::from_objects(&[sub.as_ref()]);
        let tap_uid = CFString::from_str(tap_uid);
        let one = CFNumber::new_i32(1);
        let zero = CFNumber::new_i32(0);
        let tap = CFDictionary::<CFString, CFType>::from_slices(
            &[
                &*cf_key(kAudioSubTapUIDKey),
                &*cf_key(kAudioSubTapDriftCompensationKey),
            ],
            &[tap_uid.as_ref(), one.as_ref()],
        );
        let taps = CFArray::<CFDictionary>::from_objects(&[tap.as_ref()]);
        let name = CFString::from_str(&device);
        let uid = CFString::from_str(&format!(
            "lt.proconf.nida.appsound.{bundle_id}.{}.{number}",
            std::process::id(),
        ));
        let keys = [
            cf_key(kAudioAggregateDeviceNameKey),
            cf_key(kAudioAggregateDeviceUIDKey),
            cf_key(kAudioAggregateDeviceIsPrivateKey),
            cf_key(kAudioAggregateDeviceIsStackedKey),
            cf_key(kAudioAggregateDeviceTapAutoStartKey),
            cf_key(kAudioAggregateDeviceMainSubDeviceKey),
            cf_key(kAudioAggregateDeviceSubDeviceListKey),
            cf_key(kAudioAggregateDeviceTapListKey),
        ];
        let keys: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
        let values: [&CFType; 8] = [
            name.as_ref(),
            uid.as_ref(),
            one.as_ref(),
            zero.as_ref(),
            one.as_ref(),
            clock.as_ref(),
            subs.as_ref(),
            taps.as_ref(),
        ];
        let description = CFDictionary::from_slices(&keys, &values);
        let mut aggregate: AudioObjectID = 0;
        // SAFETY: a complete aggregate description; `aggregate` receives its
        // object.
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(description.as_opaque(), NonNull::from(&mut aggregate))
        };
        if status != 0 {
            bail!("its device was refused (status {status})");
        }
        Ok((aggregate, device))
    }

    impl Drop for Tap {
        fn drop(&mut self) {
            // SAFETY: the device and tap this value made; the device goes
            // first, as it holds the tap.
            unsafe {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
                AudioHardwareDestroyProcessTap(self.process);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod other {
    use anyhow::{bail, Result};

    /// No app's sound can be taken here: no processes are listed.
    pub fn processes() -> Vec<(u32, String, bool)> {
        Vec::new()
    }

    pub fn app_name(bundle_id: &str) -> String {
        bundle_id.to_string()
    }

    /// Only macOS can tap an app's sound.
    pub struct Tap;

    impl Tap {
        /// # Errors
        ///
        /// Always: only macOS taps an app's sound.
        pub fn create(_bundle_id: &str, _processes: &[u32]) -> Result<Self> {
            bail!("an app's sound can be taken only on macOS")
        }

        pub fn device(&self) -> &str {
            ""
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{apps_of, device_name, only_the_tap, owning_app};

    /// A process belongs to the outermost app in its path: a helper to its
    /// app, Chrome Canary to itself and not to Chrome, a system service to
    /// no app.
    #[test]
    fn a_process_belongs_to_the_outermost_app_in_its_path() {
        assert_eq!(
            owning_app("/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper"),
            Some("/Applications/Google Chrome.app")
        );
        assert_eq!(
            owning_app("/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary"),
            Some("/Applications/Google Chrome Canary.app")
        );
        assert_eq!(
            owning_app("/Applications/Microsoft Teams.app/Contents/Helpers/Microsoft Teams WebView.app/Contents/MacOS/Microsoft Teams WebView"),
            Some("/Applications/Microsoft Teams.app")
        );
        assert_eq!(
            owning_app("/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU"),
            None
        );
        assert_eq!(owning_app("/Applications/zoom.us.app"), Some("/Applications/zoom.us.app"));
        assert_eq!(owning_app("/Applications/Old.APP/Contents/MacOS/Old"), Some("/Applications/Old.APP"));
    }

    /// One entry an app, making sound when any of its processes is; those
    /// making sound first; nameless processes left out.
    #[test]
    fn each_app_is_listed_once_the_playing_first() {
        let processes = [
            ("com.google.Chrome".to_string(), false),
            ("com.google.Chrome".to_string(), true),
            ("com.google.Chrome.canary".to_string(), false),
            ("us.zoom.xos".to_string(), false),
            (String::new(), true),
        ];
        assert_eq!(
            apps_of(&processes),
            [
                ("com.google.Chrome".to_string(), true),
                ("com.google.Chrome.canary".to_string(), false),
                ("us.zoom.xos".to_string(), false),
            ]
        );
    }

    /// A tap's device is used only when its input is the tap's two channels.
    #[test]
    fn a_taps_device_must_hold_only_the_tap() {
        assert!(only_the_tap(&[2]));
        assert!(!only_the_tap(&[2, 2]), "an interface's inputs beside it");
        assert!(!only_the_tap(&[]));
    }

    #[test]
    fn an_app_is_listed_under_its_own_device_name() {
        assert_eq!(device_name("us.zoom.xos"), "App sound: us.zoom.xos");
    }
}
