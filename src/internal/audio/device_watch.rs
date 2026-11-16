//! Whether the sound devices plugged in have changed: macOS says so as a
//! device comes or goes, and the inputs are listed again — a capture box
//! plugged in after Nida started was never listed, and its sound, added to
//! the mixer, stayed silent with no word why (an M5 Max, 2026-10-28).

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by macOS's call as the devices change; taken by [`take_changed`].
static CHANGED: AtomicBool = AtomicBool::new(false);

/// Ask macOS to say when the sound devices change. Once is enough; later
/// calls do nothing.
pub fn start() {
    #[cfg(target_os = "macos")]
    mac::start();
}

/// Whether the sound devices changed since this was last asked.
pub fn take_changed() -> bool {
    CHANGED.swap(false, Ordering::Relaxed)
}

/// Note a change, as macOS's call does — for tests.
#[cfg(test)]
pub(crate) fn note_change_for_tests() {
    CHANGED.store(true, Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::Ordering;
    use std::sync::Once;

    use objc2_core_audio::{
        kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, AudioObjectAddPropertyListener,
        AudioObjectID, AudioObjectPropertyAddress,
    };

    static STARTED: Once = Once::new();

    /// macOS's call: the devices changed. Only an atomic store — it runs on
    /// a `CoreAudio` thread.
    unsafe extern "C-unwind" fn changed(
        _object: AudioObjectID,
        _count: u32,
        _addresses: NonNull<AudioObjectPropertyAddress>,
        _data: *mut c_void,
    ) -> i32 {
        super::CHANGED.store(true, Ordering::Relaxed);
        0
    }

    pub fn start() {
        STARTED.call_once(|| {
            let mut address = AudioObjectPropertyAddress {
                mSelector: kAudioHardwarePropertyDevices,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain,
            };
            // SAFETY: the address lives through the call (CoreAudio copies
            // it); the listener is a plain function that only stores an
            // atomic, and is never removed, so no client data is needed.
            let status = unsafe {
                AudioObjectAddPropertyListener(
                    kAudioObjectSystemObject as AudioObjectID,
                    NonNull::from(&mut address),
                    Some(changed),
                    std::ptr::null_mut(),
                )
            };
            if status != 0 {
                log::warn!(
                    "Sound devices plugged in later will not be noticed (CoreAudio {status})"
                );
            }
        });
    }
}
