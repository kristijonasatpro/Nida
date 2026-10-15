//! A camera or capture card's picture format: its size and frame rate.

/// One format a camera or capture card can send, as vMix lists them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub struct CameraMode {
    pub width: u32,
    pub height: u32,
    /// Frames a second.
    pub fps: u32,
}

impl CameraMode {
    /// "1920×1080 60p". Rates come in whole frames, so NTSC's 59.94 arrives
    /// as 59 (and a camera at 59 is unheard of): those read as broadcast
    /// names them.
    pub fn label(&self) -> String {
        let rate = match self.fps {
            23 => "23.98".to_string(),
            29 => "29.97".to_string(),
            59 => "59.94".to_string(),
            119 => "119.88".to_string(),
            fps => fps.to_string(),
        };
        format!("{}×{} {rate}p", self.width, self.height)
    }

    fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// How a camera's picture is read into colours: the levels it spans and the
/// matrix its colours are coded with. Each is taken as the camera says
/// (`Auto`) unless set by hand — for a camera that says one thing and sends
/// another, as an ATEM's USB picture can: tagged limited range and BT.601
/// for HD, its blacks came out crushed and its highlights clipped next to
/// its own HDMI output (a laptop, 2026-10-02).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
pub struct CameraColour {
    #[serde(default)]
    pub range: CameraRange,
    #[serde(default)]
    pub matrix: CameraMatrix,
}

impl CameraColour {
    /// Both taken as the camera says.
    pub fn is_auto(&self) -> bool {
        *self == Self::default()
    }

    /// Packed into a byte, for a capture thread to read every frame without
    /// a lock (see [`Self::from_bits`]).
    pub fn to_bits(self) -> u8 {
        (self.range as u8) | ((self.matrix as u8) << 4)
    }

    /// Unpacked from [`Self::to_bits`]; anything else reads as `Auto`.
    pub fn from_bits(bits: u8) -> Self {
        Self {
            range: CameraRange::ALL
                .get(usize::from(bits & 0x0f))
                .copied()
                .unwrap_or_default(),
            matrix: CameraMatrix::ALL
                .get(usize::from(bits >> 4))
                .copied()
                .unwrap_or_default(),
        }
    }
}

/// The levels a camera's picture spans.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CameraRange {
    /// As its pixel format says: most cameras send limited.
    #[default]
    Auto = 0,
    /// Video levels: black at 16, white at 235.
    Limited = 1,
    /// Black at 0, white at 255.
    Full = 2,
}

impl CameraRange {
    /// Every choice, in the order they are offered.
    pub const ALL: [Self; 3] = [Self::Auto, Self::Limited, Self::Full];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (as the camera says)",
            Self::Limited => "Limited (16–235)",
            Self::Full => "Full (0–255)",
        }
    }
}

/// The matrix a camera's colours are coded with.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CameraMatrix {
    /// As the picture is tagged; untagged, BT.601 below 720 lines and
    /// BT.709 from there.
    #[default]
    Auto = 0,
    Bt601 = 1,
    Bt709 = 2,
    Bt2020 = 3,
}

impl CameraMatrix {
    /// Every choice, in the order they are offered.
    pub const ALL: [Self; 4] = [Self::Auto, Self::Bt601, Self::Bt709, Self::Bt2020];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (as the camera says)",
            Self::Bt601 => "BT.601",
            Self::Bt709 => "BT.709",
            Self::Bt2020 => "BT.2020",
        }
    }
}

/// What a camera's picture is coded as: Rec.709 video, shown as it comes, or
/// a log or HLG picture that is turned into Rec.709 video for the show (see
/// `renderer::camera_profile`) — as the camera is set, which Nida cannot see.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CameraProfile {
    /// Rec.709 video: shown as it comes.
    #[default]
    Normal,
    /// Sony HLG (and HLG1–3): ITU-R BT.2100 HLG on BT.2020 primaries.
    SonyHlg,
    /// Sony S-Log2 on S-Gamut.
    SonySLog2,
    /// Sony S-Log3 on S-Gamut3.Cine — Sony's usual pairing.
    SonySLog3Cine,
    /// Sony S-Log3 on S-Gamut3.
    SonySLog3,
    /// Blackmagic Film Generation 5 on Blackmagic Wide Gamut: Film on the
    /// URSA Broadcast G2, Pocket Cinema Camera 4K and 6K, Cinema Camera 6K
    /// and URSA Mini Pro 12K with current camera software.
    BlackmagicFilmGen5,
    /// A LUT file (.cube or .3dl) — a name in the show's LUT folder, or a
    /// path — applied to the camera's code values: for a curve only published
    /// as a LUT (the first URSA Broadcast's Film, Generation 4 software), or a
    /// manufacturer's own look.
    Lut { file: String },
}

impl CameraProfile {
    /// The profiles built in, in the order they are offered.
    pub const BUILT_IN: [Self; 6] = [
        Self::Normal,
        Self::SonyHlg,
        Self::SonySLog2,
        Self::SonySLog3Cine,
        Self::SonySLog3,
        Self::BlackmagicFilmGen5,
    ];

    /// Shown as it comes: nothing to convert.
    pub fn is_normal(&self) -> bool {
        *self == Self::Normal
    }

    pub fn label(&self) -> String {
        match self {
            Self::Normal => "Normal (Rec.709)".into(),
            Self::SonyHlg => "Sony HLG".into(),
            Self::SonySLog2 => "Sony S-Log2 (S-Gamut)".into(),
            Self::SonySLog3Cine => "Sony S-Log3 (S-Gamut3.Cine)".into(),
            Self::SonySLog3 => "Sony S-Log3 (S-Gamut3)".into(),
            Self::BlackmagicFilmGen5 => "Blackmagic Film Gen 5".into(),
            Self::Lut { file } => {
                let name = std::path::Path::new(file)
                    .file_name()
                    .map_or_else(|| file.clone(), |n| n.to_string_lossy().into_owned());
                format!("LUT: {name}")
            }
        }
    }
}

/// Every format once, the largest pictures first and, for each size, the
/// fastest.
pub fn sorted_modes(modes: impl IntoIterator<Item = CameraMode>) -> Vec<CameraMode> {
    let mut modes: Vec<CameraMode> = modes.into_iter().collect();
    modes.sort_by(|a, b| {
        b.pixels()
            .cmp(&a.pixels())
            .then(b.width.cmp(&a.width))
            .then(b.fps.cmp(&a.fps))
    });
    modes.dedup();
    modes
}

/// The format taken when nobody chose one: the largest picture that moves
/// smoothly (25 frames a second or more), at its fastest rate up to 60. A
/// webcam's 4K at 5 fps is passed over for its 1080p at 30, and a capture
/// card's 640×480 at 120 for its 1080p at 60. With nothing smooth, the
/// largest picture at its fastest.
pub fn default_mode(modes: &[CameraMode]) -> Option<CameraMode> {
    const SMOOTH: u32 = 25;
    const FASTEST: u32 = 60;
    let pick = |pool: Vec<&CameraMode>| -> Option<CameraMode> {
        let largest = pool.iter().map(|m| m.pixels()).max()?;
        let at_size: Vec<&&CameraMode> = pool.iter().filter(|m| m.pixels() == largest).collect();
        at_size
            .iter()
            .filter(|m| m.fps <= FASTEST)
            .max_by_key(|m| m.fps)
            .or_else(|| at_size.iter().min_by_key(|m| m.fps))
            .map(|m| ***m)
    };
    pick(modes.iter().filter(|m| m.fps >= SMOOTH).collect())
        .or_else(|| pick(modes.iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(width: u32, height: u32, fps: u32) -> CameraMode {
        CameraMode { width, height, fps }
    }

    #[test]
    fn the_largest_smooth_picture_is_taken_at_its_fastest_up_to_60() {
        // A webcam: 4K only at 5 fps.
        let webcam = [
            m(3840, 2160, 5),
            m(1920, 1080, 30),
            m(1920, 1080, 15),
            m(640, 480, 60),
        ];
        assert_eq!(default_mode(&webcam), Some(m(1920, 1080, 30)));
        // A capture card: small pictures faster than 60.
        let card = [
            m(640, 480, 120),
            m(1920, 1080, 60),
            m(1920, 1080, 30),
            m(1920, 1080, 50),
        ];
        assert_eq!(default_mode(&card), Some(m(1920, 1080, 60)));
        // Only faster than 60 at the largest size: the slowest of those.
        let fast = [m(1280, 720, 120), m(1280, 720, 240)];
        assert_eq!(default_mode(&fast), Some(m(1280, 720, 120)));
        // Nothing smooth: the largest at its fastest.
        let slow = [m(1600, 1200, 10), m(1600, 1200, 5), m(800, 600, 15)];
        assert_eq!(default_mode(&slow), Some(m(1600, 1200, 10)));
        assert_eq!(default_mode(&[]), None);
    }

    #[test]
    fn modes_are_listed_largest_and_fastest_first_once_each() {
        let listed = sorted_modes([
            m(1280, 720, 30),
            m(1920, 1080, 30),
            m(1920, 1080, 60),
            m(1280, 720, 30),
        ]);
        assert_eq!(
            listed,
            vec![m(1920, 1080, 60), m(1920, 1080, 30), m(1280, 720, 30)]
        );
        assert_eq!(m(1920, 1080, 60).label(), "1920×1080 60p");
        // An ATEM Mini's webcam output, as macOS lists it.
        assert_eq!(m(1920, 1080, 59).label(), "1920×1080 59.94p");
        assert_eq!(m(1920, 1080, 29).label(), "1920×1080 29.97p");
    }

    /// A camera's colour is written by name, read back the same, and taken
    /// as the camera says when a show has none — or only half of it.
    #[test]
    fn a_cameras_colour_is_saved_by_name_and_left_out_means_auto() {
        let colour = CameraColour {
            range: CameraRange::Full,
            matrix: CameraMatrix::Bt709,
        };
        let json = serde_json::to_string(&colour).unwrap();
        assert_eq!(json, r#"{"range":"full","matrix":"bt709"}"#);
        assert_eq!(serde_json::from_str::<CameraColour>(&json).unwrap(), colour);
        let half: CameraColour = serde_json::from_str(r#"{"range":"limited"}"#).unwrap();
        assert_eq!(half.matrix, CameraMatrix::Auto);
        assert!(serde_json::from_str::<CameraColour>("{}").unwrap().is_auto());
    }

    /// Every colour survives the byte a capture thread reads it from, and a
    /// byte that is no colour reads as the camera says.
    #[test]
    fn every_colour_survives_packing_into_a_byte() {
        for range in CameraRange::ALL {
            for matrix in CameraMatrix::ALL {
                let colour = CameraColour { range, matrix };
                assert_eq!(CameraColour::from_bits(colour.to_bits()), colour);
            }
        }
        assert!(CameraColour::from_bits(0xff).is_auto());
    }
}
