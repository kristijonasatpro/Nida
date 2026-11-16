//! Lower-third value types: rows of names and titles, how they are drawn —
//! plate, colours, place, motion — and the reading of a CSV or TSV list of
//! them, as an organiser's Excel saves it (UTF-8, or Windows-1257 with `;`).
//!
//! The layer that draws them and moves them in and out is
//! [`crate::deck::lower_third`].

/// One person, or one caption: a name and up to two lines under it.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub struct LowerThirdRow {
    pub name: String,
    #[serde(default)]
    pub title: String,
    /// A second line: an organisation, or the title in another language.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title2: String,
    /// What the organiser's list calls it, if anything.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
}

/// Where on the frame a lower third sits.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LowerThirdPlace {
    #[default]
    LowerLeft,
    LowerCentre,
    LowerRight,
    UpperLeft,
}

impl LowerThirdPlace {
    pub const ALL: [Self; 4] = [
        Self::LowerLeft,
        Self::LowerCentre,
        Self::LowerRight,
        Self::UpperLeft,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::LowerLeft => "Lower left",
            Self::LowerCentre => "Lower centre",
            Self::LowerRight => "Lower right",
            Self::UpperLeft => "Upper left",
        }
    }
}

/// How a lower third comes in and goes out.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LowerThirdMotion {
    /// At once.
    Cut,
    /// Faded.
    Fade,
    /// Slid in from its side, fading.
    Slide,
    /// The plate revealed from its side, then the text faded in.
    #[default]
    Wipe,
}

impl LowerThirdMotion {
    pub const ALL: [Self; 4] = [Self::Cut, Self::Fade, Self::Slide, Self::Wipe];

    pub fn label(self) -> &'static str {
        match self {
            Self::Cut => "Cut",
            Self::Fade => "Fade",
            Self::Slide => "Slide",
            Self::Wipe => "Wipe",
        }
    }
}

/// The accent bar on a lower third's plate.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LowerThirdBar {
    /// Down its leading edge.
    #[default]
    Edge,
    /// Along its top.
    Top,
    /// Under the name.
    Under,
    None,
}

impl LowerThirdBar {
    pub const ALL: [Self; 4] = [Self::Edge, Self::Top, Self::Under, Self::None];

    pub fn label(self) -> &'static str {
        match self {
            Self::Edge => "Down its edge",
            Self::Top => "Along its top",
            Self::Under => "Under the name",
            Self::None => "None",
        }
    }
}

/// The shape a lower third's plate takes.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LowerThirdStyle {
    /// A plate with slightly rounded corners, and its bar.
    #[default]
    Plate,
    /// The name on a band of the bar's colour, the lines under it on the
    /// plate's.
    TwoTone,
    /// A plate with slanted ends.
    Ribbon,
    /// A plate with round ends.
    Pill,
    /// No plate: the text with a soft shadow under it.
    TextOnly,
    /// A picture of one's own (a PNG with the event's design) as the plate.
    Image,
    /// A picture of one's own over the whole frame, as drawn — a PNG made
    /// at the show's size, clear but for its plate — with the text in a box
    /// over it; each with a motion of its own.
    FullFrame,
}

impl LowerThirdStyle {
    pub const ALL: [Self; 7] = [
        Self::Plate,
        Self::TwoTone,
        Self::Ribbon,
        Self::Pill,
        Self::TextOnly,
        Self::Image,
        Self::FullFrame,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Plate => "Plate",
            Self::TwoTone => "Two-tone",
            Self::Ribbon => "Ribbon",
            Self::Pill => "Pill",
            Self::TextOnly => "Text only",
            Self::Image => "Picture as plate",
            Self::FullFrame => "Full-frame picture",
        }
    }
}

/// How a lower third looks and moves.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct LowerThirdLook {
    /// A Broadcast palette (1–12, as the backgrounds have them), or 0 for
    /// the colours below.
    pub palette: u8,
    /// sRGB, 0–1: the plate, the accent bar, the name, the lines under it.
    pub plate: [f32; 3],
    pub bar_colour: [f32; 3],
    pub name_colour: [f32; 3],
    pub title_colour: [f32; 3],
    /// How solid the plate is: 0 shows the text alone.
    pub plate_opacity: f32,
    pub place: LowerThirdPlace,
    /// Inside the graphics-safe area (5% in from each edge), as broadcast
    /// asks; off for an LED wall shown edge to edge.
    pub safe_area: bool,
    /// The name's height, as a share of the frame's height.
    pub size: f32,
    /// A font installed on this Mac, by family name; empty for the system's.
    pub font: String,
    pub bar: LowerThirdBar,
    /// The plate's shape.
    pub style: LowerThirdStyle,
    /// The picture used as the plate, for the "Picture as plate" style (and
    /// the full-frame picture): a PNG or
    /// JPEG file.
    pub image: String,
    /// The text comes in once the plate is in (and goes before it): the
    /// picture first, then the name.
    pub text_after: bool,
    pub motion: LowerThirdMotion,
    /// How long it takes to come in or go out, in seconds.
    pub motion_secs: f32,
    /// Taken out by itself after this many seconds on air; 0 stays.
    pub auto_out_secs: f32,
    /// Full-frame picture: its size (1 as drawn, the frame's) and how far it
    /// is moved (shares of the frame's width and height).
    pub picture_scale: f32,
    pub picture_offset: [f32; 2],
    /// Full-frame picture: the box the text goes in — x, y, width, height,
    /// shares of the frame — and how its lines line up in it.
    pub text_box: [f32; 4],
    pub text_align: crate::engine::value::text::TextAlign,
    /// Full-frame picture: how the picture comes in (and goes out, the
    /// other way), how long it takes and after how long.
    pub picture_motion: LowerThirdMotion,
    pub picture_secs: f32,
    pub picture_delay: f32,
    /// Full-frame picture: the same for the text.
    pub text_motion: LowerThirdMotion,
    pub text_secs: f32,
    pub text_delay: f32,
}

impl Default for LowerThirdLook {
    fn default() -> Self {
        Self {
            palette: 1,
            plate: [0.08, 0.14, 0.22],
            bar_colour: [0.12, 0.44, 0.85],
            name_colour: [1.0, 1.0, 1.0],
            title_colour: [0.44, 0.89, 1.0],
            plate_opacity: 0.85,
            place: LowerThirdPlace::default(),
            safe_area: true,
            size: 0.045,
            font: String::new(),
            bar: LowerThirdBar::default(),
            style: LowerThirdStyle::default(),
            image: String::new(),
            text_after: false,
            motion: LowerThirdMotion::default(),
            motion_secs: 0.5,
            auto_out_secs: 0.0,
            picture_scale: 1.0,
            picture_offset: [0.0, 0.0],
            text_box: [0.06, 0.78, 0.5, 0.14],
            text_align: crate::engine::value::text::TextAlign::Left,
            picture_motion: LowerThirdMotion::Wipe,
            picture_secs: 0.5,
            picture_delay: 0.0,
            text_motion: LowerThirdMotion::Fade,
            text_secs: 0.4,
            text_delay: 0.3,
        }
    }
}

/// The Broadcast palettes (sRGB hex, as the backgrounds have them):
/// background, main, accent.
#[allow(clippy::unreadable_literal)] // colours read as #RRGGBB
const PALETTES: [(&str, [u32; 3]); 12] = [
    ("Corporate Blue", [0x0A1A2F, 0x1E6FD9, 0x6FE3FF]),
    ("Graphite Teal", [0x111518, 0x1F8A8A, 0x9BF2E1]),
    ("Violet Tech", [0x120E2A, 0x5B3FD9, 0xB69CFF]),
    ("Ice", [0x03080D, 0x00B8D4, 0xE0F7FF]),
    ("Neon", [0x0B0618, 0xFF2E88, 0x2EE6FF]),
    ("Terminal", [0x020A05, 0x19C37D, 0xB8FFD9]),
    ("Gala Gold", [0x1C0F0A, 0xD98E2B, 0xFFD58A]),
    ("Sunset", [0x2A0F1F, 0xF2664B, 0xFFC07A]),
    ("Rosé", [0x24121A, 0xC9637B, 0xF7C6B5]),
    ("Nordic Aurora", [0x06121F, 0x2BB39A, 0x9C8CFF]),
    ("Lagoon", [0x062433, 0x2A9DB5, 0xBFF1EE]),
    ("Lavender Mist", [0x1E2433, 0x9A8FC9, 0xE9E4F5]),
];

/// The palettes' names, by number (1–12); 0 is Custom.
pub fn palette_name(palette: u8) -> &'static str {
    match palette {
        1..=12 => PALETTES[usize::from(palette) - 1].0,
        _ => "Custom",
    }
}

pub const PALETTE_COUNT: u8 = 12;

/// A palette's colours, sRGB: background, main, accent — the Broadcast
/// looks' (`BC_PALETTES` in their shaders) and the lower thirds'. None for
/// Custom (0) or past the last.
pub fn palette_srgb(palette: u8) -> Option<[[u8; 3]; 3]> {
    let (_, colours) = PALETTES.get(usize::from(palette).checked_sub(1)?)?;
    Some(colours.map(rgb))
}

fn rgb(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

fn to8(c: [f32; 3]) -> [u8; 3] {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [f(c[0]), f(c[1]), f(c[2])]
}

/// The colours a lower third is drawn in, sRGB: plate, bar, name, lines
/// under it. A palette's: the plate its background lightened a little, the
/// bar its main, the name white, the lines its accent.
pub fn colours(look: &LowerThirdLook) -> [[u8; 3]; 4] {
    match look.palette {
        1..=12 => {
            let [bg, main, accent] = PALETTES[usize::from(look.palette) - 1].1;
            let lift =
                |c: [u8; 3]| c.map(|v| v.saturating_add(((255 - u16::from(v)) * 8 / 100) as u8));
            [lift(rgb(bg)), rgb(main), [255, 255, 255], rgb(accent)]
        }
        _ => [
            to8(look.plate),
            to8(look.bar_colour),
            to8(look.name_colour),
            to8(look.title_colour),
        ],
    }
}

/// A lower-third input: its rows, the one cued next, how they look, and the
/// list they were read from.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub struct LowerThirdSettings {
    #[serde(default)]
    pub rows: Vec<LowerThirdRow>,
    /// The row Show puts on air.
    #[serde(default)]
    pub cued: usize,
    #[serde(default)]
    pub look: LowerThirdLook,
    /// The CSV or TSV file the rows were read from, read again as it
    /// changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The buses (by UUID) it is shown on too, over whatever each shows, in
    /// its names area — the LED wall's screens, say. Main as ever: the
    /// input on Program.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
}

impl LowerThirdSettings {
    /// In range: the cued row one there is, sizes and times sensible.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.cued = self.cued.min(self.rows.len().saturating_sub(1));
        let mut seen = std::collections::HashSet::new();
        self.targets.retain(|uuid| !uuid.trim().is_empty() && seen.insert(uuid.clone()));
        let look = &mut self.look;
        look.palette = look.palette.min(PALETTE_COUNT);
        look.plate_opacity = finite_or(look.plate_opacity, 0.85).clamp(0.0, 1.0);
        look.size = finite_or(look.size, 0.045).clamp(0.015, 0.15);
        look.motion_secs = finite_or(look.motion_secs, 0.5).clamp(0.0, 3.0);
        look.auto_out_secs = finite_or(look.auto_out_secs, 0.0).clamp(0.0, 3600.0);
        look.picture_scale = finite_or(look.picture_scale, 1.0).clamp(0.1, 4.0);
        for v in &mut look.picture_offset {
            *v = finite_or(*v, 0.0).clamp(-1.0, 1.0);
        }
        let [x, y, w, h] = &mut look.text_box;
        *x = finite_or(*x, 0.06).clamp(0.0, 0.98);
        *y = finite_or(*y, 0.78).clamp(0.0, 0.98);
        *w = finite_or(*w, 0.5).clamp(0.02, 1.0);
        *h = finite_or(*h, 0.14).clamp(0.02, 1.0);
        for v in [
            &mut look.picture_secs,
            &mut look.picture_delay,
            &mut look.text_secs,
            &mut look.text_delay,
        ] {
            *v = finite_or(*v, 0.0).clamp(0.0, 3.0);
        }
        for c in [
            &mut look.plate,
            &mut look.bar_colour,
            &mut look.name_colour,
            &mut look.title_colour,
        ] {
            for v in c.iter_mut() {
                *v = finite_or(*v, 0.0).clamp(0.0, 1.0);
            }
        }
        self
    }

    /// The row `by` rows from the cued one, staying in the list.
    pub fn stepped(&self, by: i64) -> usize {
        let last = self.rows.len().saturating_sub(1);
        let at = i64::try_from(self.cued).unwrap_or(0).saturating_add(by);
        usize::try_from(at.max(0)).unwrap_or(0).min(last)
    }
}

fn finite_or(v: f32, or: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        or
    }
}

/// What a lower-third input is doing, as the panel and the API show it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct LowerThirdSnapshot {
    pub settings: LowerThirdSettings,
    /// Where the name on air is in the list, when it is there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_air: Option<usize>,
    /// The name on air — coming in, shown or going out — as shown, whether
    /// or not it is still in the list; none when off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<LowerThirdRow>,
    /// Why its list could not be read, when it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// Windows-1257 (Baltic) bytes 0x80–0xFF, as Unicode.
const CP1257_HIGH: [char; 128] = [
    '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{FFFD}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{FFFD}', '\u{2030}', '\u{FFFD}', '\u{2039}', '\u{FFFD}', '\u{00A8}', '\u{02C7}', '\u{00B8}',
    '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{FFFD}', '\u{2122}', '\u{FFFD}', '\u{203A}', '\u{FFFD}', '\u{00AF}', '\u{02DB}', '\u{FFFD}',
    '\u{00A0}', '\u{FFFD}', '\u{00A2}', '\u{00A3}', '\u{00A4}', '\u{FFFD}', '\u{00A6}', '\u{00A7}',
    '\u{00D8}', '\u{00A9}', '\u{0156}', '\u{00AB}', '\u{00AC}', '\u{00AD}', '\u{00AE}', '\u{00C6}',
    '\u{00B0}', '\u{00B1}', '\u{00B2}', '\u{00B3}', '\u{00B4}', '\u{00B5}', '\u{00B6}', '\u{00B7}',
    '\u{00F8}', '\u{00B9}', '\u{0157}', '\u{00BB}', '\u{00BC}', '\u{00BD}', '\u{00BE}', '\u{00E6}',
    '\u{0104}', '\u{012E}', '\u{0100}', '\u{0106}', '\u{00C4}', '\u{00C5}', '\u{0118}', '\u{0112}',
    '\u{010C}', '\u{00C9}', '\u{0179}', '\u{0116}', '\u{0122}', '\u{0136}', '\u{012A}', '\u{013B}',
    '\u{0160}', '\u{0143}', '\u{0145}', '\u{00D3}', '\u{014C}', '\u{00D5}', '\u{00D6}', '\u{00D7}',
    '\u{0172}', '\u{0141}', '\u{015A}', '\u{016A}', '\u{00DC}', '\u{017B}', '\u{017D}', '\u{00DF}',
    '\u{0105}', '\u{012F}', '\u{0101}', '\u{0107}', '\u{00E4}', '\u{00E5}', '\u{0119}', '\u{0113}',
    '\u{010D}', '\u{00E9}', '\u{017A}', '\u{0117}', '\u{0123}', '\u{0137}', '\u{012B}', '\u{013C}',
    '\u{0161}', '\u{0144}', '\u{0146}', '\u{00F3}', '\u{014D}', '\u{00F5}', '\u{00F6}', '\u{00F7}',
    '\u{0173}', '\u{0142}', '\u{015B}', '\u{016B}', '\u{00FC}', '\u{017C}', '\u{017E}', '\u{02D9}',
];

/// Text from bytes as an organiser's file holds them: UTF-8 (with or without
/// its mark), else Windows-1257 — what Excel saves as "CSV" on a Lithuanian
/// Windows.
pub fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes
            .iter()
            .map(|&b| {
                if b < 0x80 {
                    char::from(b)
                } else {
                    CP1257_HIGH[usize::from(b - 0x80)]
                }
            })
            .collect(),
    }
}

/// The delimiter a list uses, from its first line: a tab, `;` (Excel in a
/// Lithuanian locale) or `,` — the most of them outside quotes.
fn delimiter(text: &str) -> char {
    // Counted over the first record: up to the first line break outside
    // quotes (a name in quotes can hold one).
    let mut quoted = false;
    let mut counts = [0usize; 3];
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            '\n' if !quoted => break,
            '\t' if !quoted => counts[0] += 1,
            ';' if !quoted => counts[1] += 1,
            ',' if !quoted => counts[2] += 1,
            _ => {}
        }
    }
    let delimiters = ['\t', ';', ','];
    let most = counts.iter().copied().max().unwrap_or(0);
    if most == 0 {
        return ',';
    }
    delimiters[counts.iter().position(|&n| n == most).unwrap_or(2)]
}

/// The records of delimited text: fields split at `delimiter`, quoted
/// fields kept whole (delimiters, line breaks and doubled quotes in them).
fn records(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            c if c == delimiter => record.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                out.push(std::mem::take(&mut record));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        out.push(record);
    }
    out
}

/// Which column a header names: name, title, title2 or id — in English or
/// Lithuanian, as organisers write them.
fn column_of(header: &str) -> Option<usize> {
    let h = header.trim().to_lowercase();
    let h = h.as_str();
    match h {
        "name" | "full name" | "speaker" | "vardas" | "vardas pavardė" | "vardas, pavardė"
        | "pranešėjas" => Some(0),
        "title" | "position" | "role" | "job title" | "pareigos" => Some(1),
        "title2" | "title 2" | "organisation" | "organization" | "company" | "pareigos2"
        | "pareigos 2" | "organizacija" | "įmonė" => Some(2),
        "id" | "nr" | "nr." | "no" => Some(3),
        _ => None,
    }
}

/// The rows of a CSV or TSV list: with a header row naming its columns (in
/// English or Lithuanian), else name, title and second line in that order.
/// Rows without a name are left out.
///
/// # Errors
///
/// When it holds no row with a name.
pub fn rows_from_bytes(bytes: &[u8]) -> Result<Vec<LowerThirdRow>, String> {
    let text = decode(bytes);
    let records = records(&text, delimiter(&text));
    let Some(first) = records.first() else {
        return Err("The list is empty".into());
    };
    let header: Vec<Option<usize>> = first.iter().map(|h| column_of(h)).collect();
    let (columns, body): (Vec<Option<usize>>, &[Vec<String>]) = if header.contains(&Some(0)) {
        (header, &records[1..])
    } else {
        (
            (0..first.len()).map(|i| (i < 3).then_some(i)).collect(),
            &records[..],
        )
    };
    let rows: Vec<LowerThirdRow> = body
        .iter()
        .filter_map(|record| {
            let mut row = LowerThirdRow::default();
            for (value, column) in record.iter().zip(&columns) {
                let value = value.trim().to_string();
                match column {
                    Some(0) => row.name = value,
                    Some(1) => row.title = value,
                    Some(2) => row.title2 = value,
                    Some(3) => row.id = value,
                    _ => {}
                }
            }
            (!row.name.is_empty()).then_some(row)
        })
        .collect();
    if rows.is_empty() {
        return Err("No row in the list has a name".into());
    }
    Ok(rows)
}

/// The letters a Lithuanian name or title needs: a font without them would
/// show boxes on air.
pub const LITHUANIAN_LETTERS: &str = "ąčęėįšųūžĄČĘĖĮŠŲŪŽ";

#[cfg(test)]
mod tests {
    use super::*;

    /// A list saved by Lithuanian Excel — Windows-1257, `;` — reads with
    /// its letters intact and its Lithuanian headers understood.
    #[test]
    fn a_lithuanian_excel_list_reads_with_its_letters() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"Vardas;Pareigos\r\n");
        // "Justina Prunskien\xEB;Seimo nar\xEB" in Windows-1257.
        bytes.extend_from_slice(b"Justina Prunskien\xEB;Seimo nar\xEB\r\n");
        bytes.extend_from_slice(b";be vardo\r\n");
        let rows = rows_from_bytes(&bytes).expect("rows");
        assert_eq!(rows.len(), 1, "the row without a name left out");
        assert_eq!(rows[0].name, "Justina Prunskienė");
        assert_eq!(rows[0].title, "Seimo narė");
    }

    /// UTF-8 with its mark, commas, quoted fields with commas and doubled
    /// quotes, English headers in any order.
    #[test]
    fn a_utf8_csv_reads_with_quotes_and_its_headers() {
        let text =
            "\u{FEFF}Title,Name,Organisation\n\"CEO, founder\",\"Ana \"\"Ana\"\" Ž\",Proconf\n";
        let rows = rows_from_bytes(text.as_bytes()).expect("rows");
        assert_eq!(
            rows,
            vec![LowerThirdRow {
                name: "Ana \"Ana\" Ž".into(),
                title: "CEO, founder".into(),
                title2: "Proconf".into(),
                id: String::new(),
            }]
        );
    }

    /// Without a header row the columns are name, title, second line; a TSV
    /// is told by its tabs.
    #[test]
    fn a_list_without_headers_is_name_title_second_line() {
        let rows =
            rows_from_bytes("Jonas\tModeratorius\tLRT\nOna\tPranešėja\n".as_bytes()).expect("rows");
        assert_eq!(rows[0].title2, "LRT");
        assert_eq!(rows[1].title, "Pranešėja");
    }

    /// A name in quotes holding a line break, first in a `;` list with no
    /// header: the delimiter is told from the whole first record.
    #[test]
    fn a_quoted_line_break_in_the_first_record_keeps_the_delimiter() {
        let rows = rows_from_bytes("\"Ona\nMarija\";Pranešėja\nJonas;Moderatorius\n".as_bytes())
            .expect("rows");
        assert_eq!(rows[0].name, "Ona\nMarija");
        assert_eq!(rows[0].title, "Pranešėja");
        assert_eq!(rows[1].title, "Moderatorius");
    }

    /// Nothing with a name in it is said.
    #[test]
    fn a_list_with_no_names_is_said() {
        assert!(rows_from_bytes(b"").is_err());
        assert!(rows_from_bytes(b"Vardas;Pareigos\n;x\n").is_err());
    }

    /// Stepping stays in the list; normalising puts the cued row and the
    /// look in range.
    #[test]
    fn stepping_and_normalising_stay_in_range() {
        let settings = LowerThirdSettings {
            rows: vec![LowerThirdRow::default(); 3],
            cued: 9,
            look: LowerThirdLook {
                size: f32::NAN,
                plate_opacity: 7.0,
                palette: 40,
                ..LowerThirdLook::default()
            },
            source: None,
            targets: vec!["bus1".into(), String::new(), "bus1".into(), "bus2".into()],
        }
        .normalized();
        assert_eq!(settings.targets, ["bus1", "bus2"], "targets once each, none empty");
        assert_eq!(settings.cued, 2);
        assert_eq!(settings.stepped(1), 2);
        assert_eq!(settings.stepped(-5), 0);
        assert!((settings.look.size - 0.045).abs() < 1e-6);
        assert!((settings.look.plate_opacity - 1.0).abs() < 1e-6);
        assert_eq!(settings.look.palette, 12);
    }

    /// A palette's colours: the plate its background lifted, the name white,
    /// the lines its accent; Custom, the colours set.
    #[test]
    fn a_palette_colours_the_lower_third() {
        let look = LowerThirdLook::default();
        let [plate, bar, name, title] = colours(&look);
        assert_eq!(bar, [0x1E, 0x6F, 0xD9]);
        assert_eq!(name, [255, 255, 255]);
        assert_eq!(title, [0x6F, 0xE3, 0xFF]);
        assert!(plate[2] > 0x2F, "lifted: {plate:?}");
        let custom = LowerThirdLook {
            palette: 0,
            name_colour: [1.0, 0.0, 0.0],
            ..LowerThirdLook::default()
        };
        assert_eq!(colours(&custom)[2], [255, 0, 0]);
    }
}
