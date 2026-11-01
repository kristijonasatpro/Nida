//! The clicker read through macOS's HID manager: keyboard devices' keys,
//! whatever app is in front — with Input Monitoring allowed.
//!
//! The manager lives on the main thread and is scheduled on its run loop
//! (winit's), so its callbacks come there: where Nida can ask whether it is
//! in front.

use std::cell::RefCell;
use std::ffi::c_void;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFRunLoop, CFString, CFType};

use super::{Access, DeviceInfo, Reading};

type IOHIDManagerRef = *mut c_void;
type IOHIDValueRef = *mut c_void;
type IOHIDElementRef = *mut c_void;
type IOHIDDeviceRef = *mut c_void;
type IOReturn = i32;

type InputValueCallback =
    extern "C" fn(context: *mut c_void, result: IOReturn, sender: *mut c_void, value: IOHIDValueRef);
type DeviceCallback =
    extern "C" fn(context: *mut c_void, result: IOReturn, sender: *mut c_void, device: IOHIDDeviceRef);

/// `kIOHIDRequestTypeListenEvent`: Input Monitoring.
const LISTEN_EVENT: u32 = 1;
/// `kIOHIDAccessTypeGranted`, `kIOHIDAccessTypeDenied`.
const GRANTED: u32 = 0;
const DENIED: u32 = 1;
/// The keyboard page; the generic desktop page's keyboard.
const KEYBOARD_PAGE: i32 = 0x07;
const GENERIC_DESKTOP: i32 = 0x01;
const KEYBOARD_USAGE: i32 = 0x06;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> IOHIDManagerRef;
    fn IOHIDManagerSetDeviceMatching(manager: IOHIDManagerRef, matching: *const c_void);
    fn IOHIDManagerSetInputValueMatching(manager: IOHIDManagerRef, matching: *const c_void);
    fn IOHIDManagerRegisterInputValueCallback(
        manager: IOHIDManagerRef,
        callback: Option<InputValueCallback>,
        context: *mut c_void,
    );
    fn IOHIDManagerRegisterDeviceMatchingCallback(
        manager: IOHIDManagerRef,
        callback: Option<DeviceCallback>,
        context: *mut c_void,
    );
    fn IOHIDManagerRegisterDeviceRemovalCallback(
        manager: IOHIDManagerRef,
        callback: Option<DeviceCallback>,
        context: *mut c_void,
    );
    fn IOHIDManagerScheduleWithRunLoop(
        manager: IOHIDManagerRef,
        run_loop: *const c_void,
        mode: *const c_void,
    );
    fn IOHIDManagerUnscheduleFromRunLoop(
        manager: IOHIDManagerRef,
        run_loop: *const c_void,
        mode: *const c_void,
    );
    fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: u32) -> IOReturn;
    fn IOHIDManagerClose(manager: IOHIDManagerRef, options: u32) -> IOReturn;
    fn IOHIDValueGetElement(value: IOHIDValueRef) -> IOHIDElementRef;
    fn IOHIDValueGetIntegerValue(value: IOHIDValueRef) -> isize;
    fn IOHIDValueGetTimeStamp(value: IOHIDValueRef) -> u64;
    fn IOHIDElementGetUsagePage(element: IOHIDElementRef) -> u32;
    fn IOHIDElementGetUsage(element: IOHIDElementRef) -> u32;
    fn IOHIDElementGetDevice(element: IOHIDElementRef) -> IOHIDDeviceRef;
    fn IOHIDDeviceGetProperty(device: IOHIDDeviceRef, key: *const c_void) -> *const c_void;
    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// The manager open, scheduled on the main run loop.
struct Manager(IOHIDManagerRef);

impl Drop for Manager {
    fn drop(&mut self) {
        // Safety: a manager this module made, on the main thread that
        // scheduled it; released once.
        unsafe {
            if let (Some(run_loop), Some(mode)) = (CFRunLoop::main(), common_modes()) {
                IOHIDManagerUnscheduleFromRunLoop(self.0, ptr(&*run_loop), mode);
            }
            IOHIDManagerClose(self.0, 0);
            CFRelease(self.0.cast_const());
        }
    }
}

thread_local! {
    /// The manager, made and dropped on the main thread.
    static MANAGER: RefCell<Option<Manager>> = const { RefCell::new(None) };
}

fn ptr<T: ?Sized>(r: &T) -> *const c_void {
    std::ptr::from_ref(r).cast()
}

fn common_modes() -> Option<*const c_void> {
    // Safety: a constant CoreFoundation exports.
    unsafe { objc2_core_foundation::kCFRunLoopCommonModes }.map(ptr)
}

/// Whether macOS lets Nida read input devices.
pub(super) fn access() -> Access {
    // Safety: a plain query.
    match unsafe { IOHIDCheckAccess(LISTEN_EVENT) } {
        GRANTED => Access::Granted,
        DENIED => Access::Denied,
        _ => Access::Unknown,
    }
}

/// macOS is being asked to let Nida read input devices: its answer waits
/// on the user.
static ASKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Read what `reading` asks — the clicker learned, every keyboard, or
/// nothing — asking macOS first when `ask` and it has not been asked; how
/// macOS stands, and whether the reader is set. On the main thread.
///
/// macOS answers the question once the user has (`IOHIDRequestAccess`
/// waits for them): it is asked on a thread of its own, the reader set at a
/// frame after the answer — the engine, held while the control window
/// draws, is never kept waiting on it.
pub(super) fn read(reading: &Reading, ask: bool) -> (Access, bool) {
    use std::sync::atomic::Ordering;
    MANAGER.with(|m| m.borrow_mut().take());
    if *reading == Reading::Nothing {
        return (access(), true);
    }
    if ASKING.load(Ordering::SeqCst) {
        return (Access::Unknown, false);
    }
    let access = access();
    if access == Access::Unknown && ask {
        ASKING.store(true, Ordering::SeqCst);
        let asked = std::thread::Builder::new()
            .name("clicker-access".into())
            .spawn(|| {
                // Safety: a plain request; macOS asks the user.
                unsafe {
                    IOHIDRequestAccess(LISTEN_EVENT);
                }
                ASKING.store(false, Ordering::SeqCst);
            });
        if asked.is_err() {
            ASKING.store(false, Ordering::SeqCst);
            return (Access::Denied, true);
        }
        return (Access::Unknown, false);
    }
    if access != Access::Granted {
        return (access, true);
    }
    match open(reading) {
        Ok(manager) => {
            MANAGER.with(|m| *m.borrow_mut() = Some(manager));
            (Access::Granted, true)
        }
        Err(e) => {
            log::warn!("Clicker: the keyboards could not be read: {e:#x}");
            (Access::Denied, true)
        }
    }
}

/// A manager reading `reading`'s keyboards' keys.
fn open(reading: &Reading) -> Result<Manager, IOReturn> {
    let number = |v: i32| CFNumber::new_i32(v);
    let key = CFString::from_str;
    let (keys, values): (Vec<CFRetained<CFString>>, Vec<CFRetained<CFNumber>>) = match reading {
        Reading::Clicker(device) => (
            vec![
                key("DeviceUsagePage"),
                key("DeviceUsage"),
                key("VendorID"),
                key("ProductID"),
            ],
            vec![
                number(GENERIC_DESKTOP),
                number(KEYBOARD_USAGE),
                number(i32::try_from(device.vendor).unwrap_or(i32::MAX)),
                number(i32::try_from(device.product).unwrap_or(i32::MAX)),
            ],
        ),
        _ => (
            vec![key("DeviceUsagePage"), key("DeviceUsage")],
            vec![number(GENERIC_DESKTOP), number(KEYBOARD_USAGE)],
        ),
    };
    let keys: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&CFNumber> = values.iter().map(|v| &**v).collect();
    let devices = CFDictionary::from_slices(&keys, &values);
    let page_key = key("UsagePage");
    let page = number(KEYBOARD_PAGE);
    let elements = CFDictionary::from_slices(&[&*page_key], &[&*page]);
    // Safety: a manager made here, given dictionaries that outlive the
    // calls (the manager copies them), scheduled on the main run loop this
    // thread turns; the callback takes no context.
    unsafe {
        let manager = IOHIDManagerCreate(std::ptr::null(), 0);
        if manager.is_null() {
            return Err(-1);
        }
        let manager = Manager(manager);
        IOHIDManagerSetDeviceMatching(manager.0, ptr(&*devices));
        IOHIDManagerSetInputValueMatching(manager.0, ptr(&*elements));
        IOHIDManagerRegisterInputValueCallback(manager.0, Some(on_value), std::ptr::null_mut());
        // The clicker's devices coming and going: each one come is given the
        // remap that keeps its keys from other apps.
        if matches!(reading, Reading::Clicker(_)) {
            IOHIDManagerRegisterDeviceMatchingCallback(
                manager.0,
                Some(on_device_here),
                std::ptr::null_mut(),
            );
            IOHIDManagerRegisterDeviceRemovalCallback(
                manager.0,
                Some(on_device_gone),
                std::ptr::null_mut(),
            );
        }
        if let (Some(run_loop), Some(mode)) = (CFRunLoop::main(), common_modes()) {
            IOHIDManagerScheduleWithRunLoop(manager.0, ptr(&*run_loop), mode);
        }
        let opened = IOHIDManagerOpen(manager.0, 0);
        if opened != 0 {
            return Err(opened);
        }
        Ok(manager)
    }
}

/// A key's value changed on a keyboard: its page, usage and device to the
/// clicker's logic. Never unwinds into `IOKit`.
extern "C" fn on_value(
    _context: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
    value: IOHIDValueRef,
) {
    let _ = std::panic::catch_unwind(|| {
        if value.is_null() {
            return;
        }
        // Safety: a value IOKit hands the callback, alive for its length;
        // its element and device likewise.
        let (page, usage, down, device, at) = unsafe {
            let element = IOHIDValueGetElement(value);
            if element.is_null() {
                return;
            }
            (
                IOHIDElementGetUsagePage(element),
                IOHIDElementGetUsage(element),
                IOHIDValueGetIntegerValue(value) != 0,
                IOHIDElementGetDevice(element),
                IOHIDValueGetTimeStamp(value),
            )
        };
        if page != 0x07 || device.is_null() {
            return;
        }
        let info = DeviceInfo {
            vendor: number_property(device, "VendorID").unwrap_or(0),
            product: number_property(device, "ProductID").unwrap_or(0),
            name: string_property(device, "Product").unwrap_or_default(),
            built_in: bool_property(device, "Built-In").unwrap_or(false)
                || string_property(device, "Transport").is_some_and(|t| t == "FIFO" || t == "SPI"),
        };
        super::on_key(&info, device as usize, usage, down, in_front(), at);
    });
}

/// A device of the clicker came. Never unwinds into `IOKit`.
extern "C" fn on_device_here(
    _context: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
    device: IOHIDDeviceRef,
) {
    if !device.is_null() {
        let _ = std::panic::catch_unwind(|| super::on_device(device as usize, true));
    }
}

/// A device of the clicker went. Never unwinds into `IOKit`.
extern "C" fn on_device_gone(
    _context: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
    device: IOHIDDeviceRef,
) {
    if !device.is_null() {
        let _ = std::panic::catch_unwind(|| super::on_device(device as usize, false));
    }
}

/// Ask macOS (`hidutil`) to turn the device's keys `pairs` — usages, each
/// into its spare's — for every other app (none: lifted); whether it took
/// it. Waits for `hidutil`, three seconds at most.
pub(super) fn remap(vendor: u32, product: u32, pairs: &[(u32, u32)]) -> bool {
    let entries: Vec<String> = pairs
        .iter()
        .map(|(from, to)| {
            format!(
                "{{\"HIDKeyboardModifierMappingSrc\":{},\"HIDKeyboardModifierMappingDst\":{}}}",
                super::PAGE | u64::from(*from),
                super::PAGE | u64::from(*to)
            )
        })
        .collect();
    let set = format!("{{\"UserKeyMapping\":[{}]}}", entries.join(","));
    let took = hidutil(&["property", "--matching", &matching(vendor, product), "--set", &set]).is_some();
    if !took {
        log::warn!("Clicker: macOS did not take its keys' remap; they reach other apps");
    }
    took
}

/// Whether every one of the device's services has its keys `pairs` turned
/// so, as macOS reads them back. Waits for `hidutil`, three seconds at most.
pub(super) fn remapped(vendor: u32, product: u32, pairs: &[(u32, u32)]) -> bool {
    hidutil(&["property", "--matching", &matching(vendor, product), "--get", "UserKeyMapping"])
        .is_some_and(|read| super::turned_on_every_service(&read, pairs))
}

/// macOS's clock, as key presses carry it.
pub(super) fn now() -> u64 {
    // Safety: a plain query of the clock.
    unsafe { mach_absolute_time() }
}

extern "C" {
    fn mach_absolute_time() -> u64;
}

/// The device by maker and product, as `hidutil` matches services.
fn matching(vendor: u32, product: u32) -> String {
    format!("{{\"VendorID\":{vendor},\"ProductID\":{product}}}")
}

/// `hidutil` run with `args`: what it wrote, when it ended well within three
/// seconds.
fn hidutil(args: &[&str]) -> Option<String> {
    use std::io::Read as _;
    let mut child = std::process::Command::new("/usr/bin/hidutil")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                return status.success().then_some(out);
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Whether Nida is the app in front (its control window may have the
/// keyboard then).
fn in_front() -> bool {
    objc2::rc::autoreleasepool(|_| {
        objc2_app_kit::NSRunningApplication::currentApplication().isActive()
    })
}

/// A device's property, read by `read` while the device is `IOKit`'s to
/// lend (within the callback).
fn property<R>(
    device: IOHIDDeviceRef,
    name: &str,
    read: impl FnOnce(&CFType) -> Option<R>,
) -> Option<R> {
    let key = CFString::from_str(name);
    // Safety: a device IOKit handed the callback; the property is the
    // device's (not retained here), read before the callback returns.
    let value = unsafe { IOHIDDeviceGetProperty(device, ptr(&*key)).cast::<CFType>().as_ref() }?;
    read(value)
}

fn number_property(device: IOHIDDeviceRef, name: &str) -> Option<u32> {
    property(device, name, |v| {
        u32::try_from(v.downcast_ref::<CFNumber>()?.as_i64()?).ok()
    })
}

fn string_property(device: IOHIDDeviceRef, name: &str) -> Option<String> {
    property(device, name, |v| Some(v.downcast_ref::<CFString>()?.to_string()))
}

fn bool_property(device: IOHIDDeviceRef, name: &str) -> Option<bool> {
    property(device, name, |v| Some(v.downcast_ref::<CFBoolean>()?.as_bool()))
}
