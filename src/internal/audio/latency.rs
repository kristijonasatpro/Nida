//! What a sound device says of its own lateness on the way in — the
//! device's latency, its safety offset, its input stream's latency and its
//! buffer, in frames — logged as Nida opens it: a capture box whose sound
//! comes well behind its picture shows whether its driver says so.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_audio::{
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyLatency,
    kAudioDevicePropertySafetyOffset, kAudioDevicePropertyStreams,
    kAudioHardwarePropertyTranslateUIDToDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput, kAudioObjectSystemObject,
    kAudioStreamPropertyLatency, AudioObjectGetPropertyData, AudioObjectID,
    AudioObjectPropertyAddress,
};
use objc2_core_foundation::CFString;

/// A device's lateness on the way in, as it says, in frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputLatency {
    pub device: u32,
    pub safety: u32,
    pub stream: u32,
    pub buffer: u32,
}

impl InputLatency {
    /// As the log says it, at `rate` frames a second.
    pub fn describe(&self, rate: u32) -> String {
        let ms = |frames: u32| f64::from(frames) * 1000.0 / f64::from(rate.max(1));
        let total = self.device + self.safety + self.stream + self.buffer;
        format!(
            "device {:.1} ms, safety offset {:.1} ms, stream {:.1} ms, buffer {:.1} ms — {:.1} ms in all",
            ms(self.device),
            ms(self.safety),
            ms(self.stream),
            ms(self.buffer),
            ms(total)
        )
    }
}

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// A `u32` property of `object`.
fn u32_of(object: AudioObjectID, selector: u32, scope: u32) -> Option<u32> {
    let address = address(selector, scope);
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the property is a `u32`; `value` has room for one, and the
    // size says so.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == 0).then_some(value)
}

/// The device of `uid` (as cpal's device id carries it).
fn device_of(uid: &str) -> Option<AudioObjectID> {
    let address = address(
        kAudioHardwarePropertyTranslateUIDToDevice,
        kAudioObjectPropertyScopeGlobal,
    );
    let uid = CFString::from_str(uid);
    let qualifier: *const CFString = std::ptr::from_ref::<CFString>(&uid);
    let mut device: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    // SAFETY: the qualifier is the UID as a CFString reference, its size
    // said; the answer is one device id, with room for it.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            std::mem::size_of::<*const CFString>() as u32,
            std::ptr::from_ref(&qualifier).cast::<c_void>(),
            NonNull::from(&mut size),
            NonNull::from(&mut device).cast(),
        )
    };
    (status == 0 && device != 0).then_some(device)
}

/// The first input stream of `device`.
fn first_input_stream(device: AudioObjectID) -> Option<AudioObjectID> {
    let address = address(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput);
    let mut streams = [0 as AudioObjectID; 16];
    let mut size = std::mem::size_of_val(&streams) as u32;
    // SAFETY: room for sixteen stream ids, the size says so; CoreAudio
    // writes as many as fit and says how many bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut streams).cast(),
        )
    };
    (status == 0 && size as usize >= std::mem::size_of::<AudioObjectID>()).then_some(streams[0])
}

/// What the device of `uid` says of its lateness on the way in; none when
/// it says nothing.
pub fn of_input(uid: &str) -> Option<InputLatency> {
    let device = device_of(uid)?;
    let input = kAudioObjectPropertyScopeInput;
    Some(InputLatency {
        device: u32_of(device, kAudioDevicePropertyLatency, input).unwrap_or(0),
        safety: u32_of(device, kAudioDevicePropertySafetyOffset, input).unwrap_or(0),
        stream: first_input_stream(device)
            .and_then(|s| {
                u32_of(
                    s,
                    kAudioStreamPropertyLatency,
                    kAudioObjectPropertyScopeGlobal,
                )
            })
            .unwrap_or(0),
        buffer: u32_of(
            device,
            kAudioDevicePropertyBufferFrameSize,
            kAudioObjectPropertyScopeGlobal,
        )
        .unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lateness is told in milliseconds at the device's rate, and in all.
    #[test]
    fn lateness_is_told_in_milliseconds() {
        let l = InputLatency {
            device: 480,
            safety: 48,
            stream: 0,
            buffer: 512,
        };
        assert_eq!(
            l.describe(48_000),
            "device 10.0 ms, safety offset 1.0 ms, stream 0.0 ms, buffer 10.7 ms — 21.7 ms in all"
        );
    }

    /// A device not there says nothing.
    #[test]
    fn a_device_not_there_says_nothing() {
        assert_eq!(of_input("no-such-device-uid"), None);
    }
}
