//! Blackmagic devices taking pictures and sound in — an UltraStudio Recorder,
//! a DeckLink card — through Blackmagic's own DeckLink API, as Desktop Video
//! has it (see the shim): the format coming in is followed as the device
//! tells it, its sound comes with its pictures, and the device says when
//! nothing is plugged into it. Listed among the cameras (see
//! `crate::camera`), whose capture threads open them here.

/// What a camera's unique ID starts with when it is a Blackmagic input.
pub const UNIQUE_PREFIX: &str = "decklink:";

/// Whether the camera of `unique_id` is a Blackmagic input.
pub fn is_blackmagic(unique_id: &str) -> bool {
    unique_id.starts_with(UNIQUE_PREFIX)
}

/// One of the formats a device takes pictures in.
#[derive(Debug, Clone, PartialEq)]
pub struct InputMode {
    /// As Blackmagic names it: "1080p50", "1080i59.94" …
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Frames a second (two fields make one).
    pub fps: f64,
    /// Whole frames, not two fields woven.
    pub progressive: bool,
    /// The API's code for it.
    pub code: u32,
}

/// The box a camera or sound device is, by name: without the "Blackmagic"
/// macOS puts before it, or the " (Blackmagic)" Nida puts after a box read
/// through Blackmagic's own way — "UltraStudio Mini Recorder 12G" either
/// way, lower case.
pub fn box_name(name: &str) -> String {
    let name = name.trim();
    let name = name.strip_suffix(" (Blackmagic)").unwrap_or(name);
    let name = name.strip_prefix("Blackmagic ").unwrap_or(name);
    name.trim().to_lowercase()
}

/// Whether two names are the same box (see [`box_name`]) — never a name
/// that only starts like another ("UltraStudio 4K", "UltraStudio 4K
/// Extreme").
pub fn same_box(a: &str, b: &str) -> bool {
    let a = box_name(a);
    !a.is_empty() && a == box_name(b)
}

/// A connection a device takes video in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Sdi,
    Hdmi,
    OpticalSdi,
    Component,
    Composite,
    SVideo,
}

impl Connection {
    const ALL: [Self; 6] = [
        Self::Sdi,
        Self::Hdmi,
        Self::OpticalSdi,
        Self::Component,
        Self::Composite,
        Self::SVideo,
    ];

    /// The API's bit for it (`BMDVideoConnection`).
    pub fn bit(self) -> i64 {
        match self {
            Self::Sdi => 1,
            Self::Hdmi => 1 << 1,
            Self::OpticalSdi => 1 << 2,
            Self::Component => 1 << 3,
            Self::Composite => 1 << 4,
            Self::SVideo => 1 << 5,
        }
    }

    /// The connections among `bits`, in the API's order.
    pub fn from_bits(bits: i64) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|c| bits & c.bit() != 0)
            .collect()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sdi => "SDI",
            Self::Hdmi => "HDMI",
            Self::OpticalSdi => "optical SDI",
            Self::Component => "component",
            Self::Composite => "composite",
            Self::SVideo => "S-Video",
        }
    }
}

/// A device that can take pictures in.
#[derive(Debug, Clone, PartialEq)]
pub struct InputDevice {
    pub name: String,
    /// What tells it from another of its model across plugging in and out:
    /// [`UNIQUE_PREFIX`] and Blackmagic's persistent ID, else its name.
    pub unique_id: String,
    /// This arrival of it: a capture opens on one.
    pub arrival: u64,
    /// Whether it tells the format coming in, and is opened to follow it.
    pub detects: bool,
    /// The connections it takes video in on.
    pub connections: Vec<Connection>,
    pub modes: Vec<InputMode>,
}

/// The unique ID of a device of `persistent` ID (0 when it has none) and
/// `name`.
pub fn unique_id(persistent: i64, name: &str) -> String {
    if persistent == 0 {
        format!("{UNIQUE_PREFIX}{name}")
    } else {
        format!("{UNIQUE_PREFIX}{persistent:x}")
    }
}

/// The devices that can take pictures in, as discovery has them: none
/// without Desktop Video. Cheap: never reaches the driver.
pub fn input_devices() -> Vec<InputDevice> {
    sdk::input_devices()
}

/// Changes whenever a Blackmagic device arrives or goes: the devices are to
/// be looked at again. Starts discovery, which tells of the devices there
/// a moment after.
/// Nida is quitting: every Blackmagic capture stopped and its callback let
/// go before the process ends (quit from macOS ends it without the
/// captures closed, and a frame arriving then aborted it).
pub fn shutdown() {
    sdk::shutdown();
}

pub fn generation() -> u64 {
    sdk::generation()
}

/// How a picture's bytes are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pixels {
    /// 8-bit 4:2:2 YUV, '2vuy': U Y V Y.
    Uyvy,
    /// 8-bit RGB, B G R A.
    Bgra,
}

/// The colour matrix a format is coded with, as the device says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colorspace {
    Rec601,
    Rec709,
    Rec2020,
    /// Not said: taken by size (SD is Rec. 601).
    Unsaid,
}

impl Colorspace {
    /// From the format's own flags (`BMDDisplayModeFlags`).
    pub fn from_flags(flags: u32) -> Self {
        const REC601: u32 = 1 << 1;
        const REC709: u32 = 1 << 2;
        const REC2020: u32 = 1 << 3;
        if flags & REC2020 != 0 {
            Self::Rec2020
        } else if flags & REC709 != 0 {
            Self::Rec709
        } else if flags & REC601 != 0 {
            Self::Rec601
        } else {
            Self::Unsaid
        }
    }
}

/// A picture come in, borrowed for the call.
pub struct Picture<'a> {
    /// `height` rows of `row_bytes`.
    pub data: &'a [u8],
    pub row_bytes: usize,
    pub width: u32,
    pub height: u32,
    /// None for a layout Nida does not read.
    pub pixels: Option<Pixels>,
    pub colorspace: Colorspace,
    /// Nothing is plugged into the device: what it holds is no picture.
    pub no_signal: bool,
    /// Whole frames; else two fields woven, line by line.
    pub progressive: bool,
    /// The format's rate, when it says.
    pub fps: Option<f64>,
}

/// What takes a device's pictures and sound, on the API's thread: never
/// both at once.
pub trait Taker: Send {
    fn picture(&mut self, picture: &Picture<'_>);
    /// `samples` interleaved, `channels` to a frame, 48 kHz.
    fn sound(&mut self, samples: &[i32], channels: usize);
}

/// Pictures and sound coming in from a device, till dropped.
pub struct Capture {
    inner: sdk::Capture,
}

impl Capture {
    /// Start taking pictures in from the device of arrival `arrival` in the
    /// format of API code `code` — following the format coming in, when the
    /// device tells it — each handed to `taker`.
    ///
    /// # Errors
    ///
    /// The device has gone, has no such format, or would not start (another
    /// app taking pictures from it).
    pub fn open(arrival: u64, code: u32, taker: Box<dyn Taker>) -> Result<Self, String> {
        sdk::Capture::open(arrival, code, taker).map(|inner| Self { inner })
    }

    /// Whether its device is still connected, and taking pictures in at
    /// the format that comes.
    pub fn alive(&self) -> bool {
        self.inner.alive()
    }

    /// Take video in on `connection` from now, as Desktop Video Setup's own
    /// setting would — the box was seen to keep it after Nida closed.
    /// Whether the device took it.
    pub fn set_connection(&self, connection: Connection) -> bool {
        self.inner.set_connection(connection.bit())
    }

    /// The connection it takes video in on, when the device says.
    pub fn connection(&self) -> Option<Connection> {
        Connection::from_bits(self.inner.connection())
            .first()
            .copied()
    }

    /// The format that came in (its API code), once it differed from the
    /// one opened.
    pub fn detected_mode(&self) -> Option<u32> {
        Some(self.inner.detected()).filter(|code| *code != 0)
    }
}

#[cfg(target_os = "macos")]
mod sdk {
    use super::{Colorspace, Connection, InputDevice, InputMode, Picture, Pixels, Taker};
    use std::ffi::{c_char, c_void, CStr};

    #[repr(C)]
    struct Mode {
        code: u32,
        width: i32,
        height: i32,
        frame_duration: i64,
        time_scale: i64,
        progressive: i32,
        name: [c_char; 64],
    }

    #[repr(C)]
    struct Info {
        id: u64,
        persistent: i64,
        connections: i64,
        detects: i32,
        name: [c_char; 256],
    }

    type VideoFn = extern "C" fn(
        ctx: *mut c_void,
        data: *const u8,
        row_bytes: i32,
        width: i32,
        height: i32,
        pixel_format: u32,
        colorspace: u32,
        no_signal: i32,
        progressive: i32,
        frame_duration: i64,
        time_scale: i64,
    );
    type AudioFn = extern "C" fn(ctx: *mut c_void, samples: *const i32, frames: i32, channels: i32);

    extern "C" {
        fn nida_dl_inputs(infos: *mut Info, max: i32) -> i32;
        fn nida_dl_input_modes(id: u64, modes: *mut Mode, max: i32) -> i32;
        fn nida_dl_capture_open(
            id: u64,
            mode: u32,
            ctx: *mut c_void,
            video: VideoFn,
            audio: AudioFn,
            err: *mut c_char,
            errlen: i32,
        ) -> *mut c_void;
        fn nida_dl_capture_alive(handle: *mut c_void) -> i32;
        fn nida_dl_capture_set_connection(handle: *mut c_void, connection: i64) -> i32;
        fn nida_dl_capture_connection(handle: *mut c_void) -> i64;
        fn nida_dl_capture_detected(handle: *mut c_void) -> u32;
        fn nida_dl_capture_close(handle: *mut c_void);
        fn nida_dl_generation() -> u64;
        fn nida_dl_shutdown();
    }

    pub(super) fn generation() -> u64 {
        // SAFETY: no arguments; loads the API when there is one.
        unsafe { nida_dl_generation() }
    }

    pub(super) fn shutdown() {
        // SAFETY: no arguments; open captures stay valid handles (stopped,
        // not let go), so their `Drop` may still close them.
        unsafe { nida_dl_shutdown() }
    }

    const UYVY: u32 = u32::from_be_bytes(*b"2vuy");
    const BGRA: u32 = u32::from_be_bytes(*b"BGRA");

    #[allow(clippy::cast_precision_loss)]
    fn rate(frame_duration: i64, time_scale: i64) -> Option<f64> {
        (frame_duration > 0 && time_scale > 0).then(|| time_scale as f64 / frame_duration as f64)
    }

    fn text(chars: &[c_char]) -> String {
        // SAFETY: NUL-terminated by the shim, within the array.
        unsafe { CStr::from_ptr(chars.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    fn modes(id: u64) -> Vec<InputMode> {
        let mut modes: Vec<Mode> = (0..128)
            .map(|_| Mode {
                code: 0,
                width: 0,
                height: 0,
                frame_duration: 0,
                time_scale: 0,
                progressive: 0,
                name: [0; 64],
            })
            .collect();
        // SAFETY: room for the 128 asked for.
        let found = unsafe { nida_dl_input_modes(id, modes.as_mut_ptr(), 128) };
        modes.truncate(usize::try_from(found).unwrap_or(0));
        modes
            .iter()
            .map(|m| InputMode {
                name: text(&m.name),
                width: u32::try_from(m.width).unwrap_or(0),
                height: u32::try_from(m.height).unwrap_or(0),
                fps: rate(m.frame_duration, m.time_scale).unwrap_or(0.0),
                progressive: m.progressive != 0,
                code: m.code,
            })
            .collect()
    }

    pub(super) fn input_devices() -> Vec<InputDevice> {
        let mut infos: Vec<Info> = (0..16)
            .map(|_| Info {
                id: 0,
                persistent: 0,
                connections: 0,
                detects: 0,
                name: [0; 256],
            })
            .collect();
        // SAFETY: room for the 16 asked for; loads the API when there is one.
        let found = unsafe { nida_dl_inputs(infos.as_mut_ptr(), 16) };
        infos.truncate(usize::try_from(found).unwrap_or(0).min(16));
        infos
            .iter()
            .map(|info| {
                let name = text(&info.name);
                InputDevice {
                    unique_id: super::unique_id(info.persistent, &name),
                    name,
                    arrival: info.id,
                    detects: info.detects != 0,
                    connections: Connection::from_bits(info.connections),
                    modes: modes(info.id),
                }
            })
            .collect()
    }

    extern "C" fn on_video(
        ctx: *mut c_void,
        data: *const u8,
        row_bytes: i32,
        width: i32,
        height: i32,
        pixel_format: u32,
        colorspace: u32,
        no_signal: i32,
        progressive: i32,
        frame_duration: i64,
        time_scale: i64,
    ) {
        let (Ok(row_bytes), Ok(width), Ok(height)) = (
            usize::try_from(row_bytes),
            u32::try_from(width),
            u32::try_from(height),
        ) else {
            return;
        };
        if data.is_null() || ctx.is_null() {
            return;
        }
        // SAFETY: the frame's bytes, `height` rows of `row_bytes`, live for
        // the call; the shim hands each frame once.
        let data = unsafe { std::slice::from_raw_parts(data, row_bytes * height as usize) };
        let picture = Picture {
            data,
            row_bytes,
            width,
            height,
            pixels: match pixel_format {
                UYVY => Some(Pixels::Uyvy),
                BGRA => Some(Pixels::Bgra),
                _ => None,
            },
            colorspace: Colorspace::from_flags(colorspace),
            no_signal: no_signal != 0,
            progressive: progressive != 0,
            fps: rate(frame_duration, time_scale),
        };
        // SAFETY: `ctx` is the capture's taker, alive till the shim has
        // closed it, called on one thread at a time (see the shim).
        let taker = unsafe { &mut *ctx.cast::<Box<dyn Taker>>() };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| taker.picture(&picture)));
    }

    extern "C" fn on_audio(ctx: *mut c_void, samples: *const i32, frames: i32, channels: i32) {
        let (Ok(frames), Ok(channels)) = (usize::try_from(frames), usize::try_from(channels))
        else {
            return;
        };
        if samples.is_null() || ctx.is_null() || channels == 0 {
            return;
        }
        // SAFETY: the packet's samples live for the call.
        let samples = unsafe { std::slice::from_raw_parts(samples, frames * channels) };
        // SAFETY: as for `on_video`.
        let taker = unsafe { &mut *ctx.cast::<Box<dyn Taker>>() };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            taker.sound(samples, channels);
        }));
    }

    pub(super) struct Capture {
        handle: *mut c_void,
        taker: *mut Box<dyn Taker>,
    }

    // SAFETY: the handle is used by one thread at a time — the camera's —
    // and the taker only by the API's thread till the capture is closed.
    unsafe impl Send for Capture {}

    impl Capture {
        pub(super) fn open(arrival: u64, code: u32, taker: Box<dyn Taker>) -> Result<Self, String> {
            let taker = Box::into_raw(Box::new(taker));
            let mut err = [0 as c_char; 256];
            // SAFETY: `taker` lives till the capture is closed (`Drop`), or
            // is taken back here when it did not open.
            let handle = unsafe {
                nida_dl_capture_open(
                    arrival,
                    code,
                    taker.cast(),
                    on_video,
                    on_audio,
                    err.as_mut_ptr(),
                    256,
                )
            };
            if handle.is_null() {
                // SAFETY: not opened: the shim holds no pointer to it.
                drop(unsafe { Box::from_raw(taker) });
                return Err(text(&err));
            }
            Ok(Self { handle, taker })
        }

        pub(super) fn alive(&self) -> bool {
            // SAFETY: an open capture.
            unsafe { nida_dl_capture_alive(self.handle) != 0 }
        }

        pub(super) fn set_connection(&self, connection: i64) -> bool {
            // SAFETY: an open capture.
            unsafe { nida_dl_capture_set_connection(self.handle, connection) == 0 }
        }

        pub(super) fn connection(&self) -> i64 {
            // SAFETY: an open capture.
            unsafe { nida_dl_capture_connection(self.handle) }
        }

        pub(super) fn detected(&self) -> u32 {
            // SAFETY: an open capture.
            unsafe { nida_dl_capture_detected(self.handle) }
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            // SAFETY: an open capture, closed once; on return no more calls
            // are made with the taker, which is then let go.
            unsafe {
                nida_dl_capture_close(self.handle);
                drop(Box::from_raw(self.taker));
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod sdk {
    use super::{InputDevice, Taker};

    pub(super) fn input_devices() -> Vec<InputDevice> {
        Vec::new()
    }

    pub(super) fn generation() -> u64 {
        0
    }

    pub(super) fn shutdown() {}

    pub(super) struct Capture;

    impl Capture {
        pub(super) fn open(
            _arrival: u64,
            _code: u32,
            _taker: Box<dyn Taker>,
        ) -> Result<Self, String> {
            Err("Blackmagic inputs are taken on a Mac".into())
        }

        #[allow(clippy::unused_self)]
        pub(super) fn alive(&self) -> bool {
            false
        }

        #[allow(clippy::unused_self)]
        pub(super) fn set_connection(&self, _connection: i64) -> bool {
            false
        }

        #[allow(clippy::unused_self)]
        pub(super) fn connection(&self) -> i64 {
            0
        }

        #[allow(clippy::unused_self)]
        pub(super) fn detected(&self) -> u32 {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stopping every capture as Nida quits is safe with none open, and
    /// twice (both quit paths may run it).
    #[test]
    fn shutting_captures_down_is_safe_with_none_open_and_twice() {
        shutdown();
        shutdown();
    }

    /// A device is told by its persistent ID across plugging in and out,
    /// else by its name; either way known as a Blackmagic input.
    #[test]
    fn a_device_is_told_by_its_persistent_id_else_its_name() {
        let by_id = unique_id(0x1a2b, "UltraStudio Recorder 3G");
        assert_eq!(by_id, "decklink:1a2b");
        let by_name = unique_id(0, "UltraStudio Recorder 3G");
        assert_eq!(by_name, "decklink:UltraStudio Recorder 3G");
        assert!(is_blackmagic(&by_id) && is_blackmagic(&by_name));
        assert!(!is_blackmagic("0x1430000005ac8600"));
    }

    /// A box is the same whichever way it is listed, and only that box.
    #[test]
    fn a_box_is_known_whichever_way_it_is_listed() {
        let ours = "UltraStudio Mini Recorder 12G (Blackmagic)";
        assert!(same_box("Blackmagic UltraStudio Mini Recorder 12G", ours));
        assert!(!same_box(
            "Blackmagic UltraStudio 4K Extreme",
            "UltraStudio 4K (Blackmagic)"
        ));
        assert!(!same_box("", " (Blackmagic)"));
    }

    /// A device's connections are read from the API's bits, in its order.
    #[test]
    fn connections_are_read_from_their_bits() {
        assert_eq!(
            Connection::from_bits(0b11),
            vec![Connection::Sdi, Connection::Hdmi]
        );
        assert_eq!(Connection::from_bits(0), Vec::<Connection>::new());
        assert_eq!(Connection::Hdmi.bit(), 2);
    }

    /// The matrix a format is coded with is read from its flags, the newest
    /// standard first; none said, none taken.
    #[test]
    fn the_colour_matrix_is_read_from_the_formats_flags() {
        assert_eq!(Colorspace::from_flags(1 << 2), Colorspace::Rec709);
        assert_eq!(Colorspace::from_flags(1 << 1), Colorspace::Rec601);
        assert_eq!(
            Colorspace::from_flags((1 << 3) | (1 << 2)),
            Colorspace::Rec2020
        );
        assert_eq!(Colorspace::from_flags(1), Colorspace::Unsaid);
    }
}
