//! Process-owned requests avoid the thread affinity of `SetThreadExecutionState`:
//! the engine can move between the UI thread and its render thread.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    System::{
        Power::{
            PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired,
            PowerRequestSystemRequired, PowerSetRequest,
        },
        SystemServices::POWER_REQUEST_CONTEXT_VERSION,
        Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0},
    },
};

pub(super) struct PowerRequest(OwnedHandle);

impl PowerRequest {
    pub(super) fn new(reason: &str) -> io::Result<Self> {
        let mut reason: Vec<u16> = reason.encode_utf16().chain(Some(0)).collect();
        let context = REASON_CONTEXT {
            Version: POWER_REQUEST_CONTEXT_VERSION,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: reason.as_mut_ptr(),
            },
        };
        // SAFETY: the context and its null-terminated reason stay alive for the
        // call. Windows copies the reason into the new request object.
        let raw = unsafe { PowerCreateRequest(&raw const context) };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: PowerCreateRequest returned a new, valid handle owned by us.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: the handle is valid and remains owned throughout these calls.
        unsafe {
            if PowerSetRequest(raw, PowerRequestDisplayRequired) == 0 {
                return Err(io::Error::last_os_error());
            }
            if PowerSetRequest(raw, PowerRequestSystemRequired) == 0 {
                let error = io::Error::last_os_error();
                PowerClearRequest(raw, PowerRequestDisplayRequired);
                return Err(error);
            }
        }
        Ok(Self(handle))
    }
}

impl Drop for PowerRequest {
    fn drop(&mut self) {
        // SAFETY: construction succeeded with both requests set, and we still
        // own the handle. OwnedHandle closes it after both counts are cleared.
        unsafe {
            PowerClearRequest(self.0.as_raw_handle(), PowerRequestSystemRequired);
            PowerClearRequest(self.0.as_raw_handle(), PowerRequestDisplayRequired);
        }
    }
}
