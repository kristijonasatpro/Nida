//! Test cards for LED screens: what the LED technician lines the processor up
//! with. Each screen gets its own colour, its name, its size and where it sits
//! in the feed, a one-pixel white edge (a cut edge shows at once), a 64-pixel
//! grid, corner-to-corner diagonals and a circle (a stretched picture shows
//! as an oval).

use std::collections::HashMap;

use crate::engine::value::led::{LedLayout, LedScreen};

/// Grid spacing in LED pixels.
const GRID: u32 = 64;

/// One colour per screen, in list order, repeating.
const COLOURS: [[u8; 3]; 8] = [
    [230, 60, 60],
    [60, 170, 240],
    [90, 210, 90],
    [240, 190, 40],
    [200, 90, 230],
    [40, 210, 200],
    [250, 130, 50],
    [230, 230, 230],
];

/// The colour of the `index`th screen of a feed: on its test card and
/// wherever the screen is drawn in the editor.
pub fn screen_colour(index: usize) -> [u8; 3] {
    COLOURS[index % COLOURS.len()]
}

/// The test card for `screen`, the `index`th of its feed: RGBA, sRGB, rows
/// top to bottom.
pub fn test_card(screen: &LedScreen, index: usize) -> Vec<u8> {
    let [w, h] = screen.size.map(|v| v.max(1));
    let colour = screen_colour(index);
    let dim = colour.map(|c| c / 5);
    let mut card = Card {
        w,
        h,
        px: vec![0; (w * h * 4) as usize],
    };
    for y in 0..h {
        for x in 0..w {
            card.set(x, y, dim);
        }
    }
    let grid = colour.map(|c| c / 2);
    for x in (0..w).step_by(GRID as usize) {
        card.vline(x, grid);
    }
    for y in (0..h).step_by(GRID as usize) {
        card.hline(y, grid);
    }
    card.line([0.0, 0.0], [w as f32 - 1.0, h as f32 - 1.0], colour);
    card.line([w as f32 - 1.0, 0.0], [0.0, h as f32 - 1.0], colour);
    card.circle(
        [w as f32 / 2.0, h as f32 / 2.0],
        w.min(h) as f32 * 0.4,
        colour,
    );
    let lines = [
        screen.name.clone(),
        format!("{} x {}", screen.size[0], screen.size[1]),
        format!("at {}, {}", screen.at[0], screen.at[1]),
    ];
    card.text_block(&lines, [255; 3]);
    // The edge last, so nothing covers it: white outermost, the screen's
    // colour inside it.
    card.hline(0, [255; 3]);
    card.hline(h - 1, [255; 3]);
    card.vline(0, [255; 3]);
    card.vline(w - 1, [255; 3]);
    if w > 2 && h > 2 {
        for x in 1..w - 1 {
            card.set(x, 1, colour);
            card.set(x, h - 2, colour);
        }
        for y in 1..h - 1 {
            card.set(1, y, colour);
            card.set(w - 2, y, colour);
        }
    }
    card.px
}

/// The test card for a whole output, `size` pixels — what its screen is lined
/// up and checked with, in the manner of a media server's: drawn at the
/// output's own pixels, so its grid stays square and its circle round on any
/// screen; a grid of square cells about the centre, corner-to-corner
/// diagonals, a circle as high as the card, a centre cross; the output's name
/// and size in the middle; along the top, colour bars apart from each other;
/// along the foot, grey steps apart and the levels strip — codes 0, 16, 235
/// and 255, each labelled — for checking an output's Levels; a one-pixel
/// white edge with the output's colour inside. RGBA, display codes (sRGB),
/// rows top to bottom.
pub fn output_card(name: &str, size: [u32; 2], index: usize) -> Vec<u8> {
    let [w, h] = size.map(|v| v.max(1));
    let colour = screen_colour(index);
    let mut card = Card {
        w,
        h,
        px: vec![0; (w * h * 4) as usize],
    };
    card.fill(0, 0, w, h, [32, 32, 32]);
    // Square cells about the centre: nine rows on a landscape card.
    let cell = (w.min(h) / 9).max(4);
    let thick = (h / 540).max(1);
    let (cx, cy) = (w / 2, h / 2);
    let grid = [96, 96, 96];
    for k in 0..=(w / cell / 2 + 1) {
        for x in [cx.saturating_add(k * cell), cx.saturating_sub(k * cell)] {
            card.fill(x.saturating_sub(thick / 2), 0, thick, h, grid);
        }
    }
    for k in 0..=(h / cell / 2 + 1) {
        for y in [cy.saturating_add(k * cell), cy.saturating_sub(k * cell)] {
            card.fill(0, y.saturating_sub(thick / 2), w, thick, grid);
        }
    }
    let soft = [170, 170, 170];
    card.line([0.0, 0.0], [w as f32 - 1.0, h as f32 - 1.0], soft);
    card.line([w as f32 - 1.0, 0.0], [0.0, h as f32 - 1.0], soft);
    for t in 0..thick {
        card.circle(
            [w as f32 / 2.0, h as f32 / 2.0],
            h.min(w) as f32 * 0.47 - t as f32,
            [255; 3],
        );
    }
    let arm = cell / 2;
    card.fill(cx.saturating_sub(arm), cy.saturating_sub(thick / 2), arm * 2, thick, [255; 3]);
    card.fill(cx.saturating_sub(thick / 2), cy.saturating_sub(arm), thick, arm * 2, [255; 3]);
    // Bars and patches sit inside the cells of one row each, apart.
    let row_h = cell * 3 / 5;
    let gap = (cell / 8).max(2);
    let band_w = w * 4 / 5;
    let band_x = (w - band_w) / 2;
    let bars: [[u8; 3]; 8] = [
        [255, 255, 255],
        [255, 255, 0],
        [0, 255, 255],
        [0, 255, 0],
        [255, 0, 255],
        [255, 0, 0],
        [0, 0, 255],
        [0, 0, 0],
    ];
    let top = cy.saturating_sub(cell * 3) + (cell - row_h) / 2;
    card.apart((band_x, top), (band_w, row_h), gap, &bars);
    let steps: Vec<[u8; 3]> = (0..=10u32)
        .map(|i| {
            let v = u8::try_from(i * 255 / 10).unwrap_or(255);
            [v, v, v]
        })
        .collect();
    let foot = cy + cell * 2 + (cell - row_h) / 2;
    let steps_w = band_w * 11 / 16;
    card.apart((band_x, foot), (steps_w, row_h), gap, &steps);
    let levels: Vec<[u8; 3]> = LEVEL_PATCHES.iter().map(|&v| [v, v, v]).collect();
    let levels_x = band_x + steps_w + gap * 3;
    let levels_w = band_x + band_w - levels_x;
    card.apart((levels_x, foot), (levels_w, row_h), gap, &levels);
    // Each levels patch says its code, under it.
    let unit = (row_h / 18).max(1);
    let patch_w = (levels_w + gap) / 4;
    for (i, code) in LEVEL_PATCHES.iter().enumerate() {
        let text: Vec<char> = code.to_string().chars().collect();
        let text_w = text.len() as u32 * 6 * unit;
        let x0 = levels_x + patch_w * i as u32 + patch_w.saturating_sub(gap + text_w) / 2;
        for (j, &c) in text.iter().enumerate() {
            card.glyph(c, x0 + j as u32 * 6 * unit, foot + row_h + unit * 2, unit, [220; 3]);
        }
    }
    card.text_block(&[name.to_string(), format!("{w} x {h}")], [255; 3]);
    // The edge last, so nothing covers it: white outermost, the output's
    // colour inside it.
    card.hline(0, [255; 3]);
    card.hline(h - 1, [255; 3]);
    card.vline(0, [255; 3]);
    card.vline(w - 1, [255; 3]);
    if w > 2 && h > 2 {
        card.fill(1, 1, w - 2, thick, colour);
        card.fill(1, h - 1 - thick, w - 2, thick, colour);
        card.fill(1, 1, thick, h - 2, colour);
        card.fill(w - 1 - thick, 1, thick, h - 2, colour);
    }
    card.px
}

/// The levels strip's codes: full black, video black, video white, full white.
pub const LEVEL_PATCHES: [u8; 4] = [0, 16, 235, 255];

struct Card {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Card {
    fn set(&mut self, x: u32, y: u32, rgb: [u8; 3]) {
        if x < self.w && y < self.h {
            let i = ((y * self.w + x) * 4) as usize;
            self.px[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }

    /// A filled rectangle, clipped to the card.
    fn fill(&mut self, left: u32, top: u32, width: u32, height: u32, rgb: [u8; 3]) {
        for row in top..top.saturating_add(height).min(self.h) {
            for column in left..left.saturating_add(width).min(self.w) {
                self.set(column, row, rgb);
            }
        }
    }

    /// `colours` side by side across `width` from `left`, each a block of its
    /// own with `gap` between — bars apart, not a gradient.
    fn apart(&mut self, (left, top): (u32, u32), (width, height): (u32, u32), gap: u32, colours: &[[u8; 3]]) {
        let count = u32::try_from(colours.len()).unwrap_or(1).max(1);
        let each = (width.saturating_sub(gap * (count - 1)) / count).max(1);
        for (at, &rgb) in colours.iter().enumerate() {
            let at = u32::try_from(at).unwrap_or(0);
            self.fill(left + at * (each + gap), top, each, height, rgb);
        }
    }

    fn hline(&mut self, y: u32, rgb: [u8; 3]) {
        for x in 0..self.w {
            self.set(x, y, rgb);
        }
    }

    fn vline(&mut self, x: u32, rgb: [u8; 3]) {
        for y in 0..self.h {
            self.set(x, y, rgb);
        }
    }

    fn line(&mut self, from: [f32; 2], to: [f32; 2], rgb: [u8; 3]) {
        let steps = (to[0] - from[0])
            .abs()
            .max((to[1] - from[1]).abs())
            .ceil()
            .max(1.0) as u32;
        for i in 0..=steps {
            let along = i as f32 / steps as f32;
            let x = from[0] + (to[0] - from[0]) * along;
            let y = from[1] + (to[1] - from[1]) * along;
            self.set(x.round() as u32, y.round() as u32, rgb);
        }
    }

    fn circle(&mut self, centre: [f32; 2], radius: f32, rgb: [u8; 3]) {
        let steps = (radius * 8.0).ceil().max(8.0) as u32;
        for i in 0..steps {
            let angle = i as f32 / steps as f32 * std::f32::consts::TAU;
            let (x, y) = (
                centre[0] + radius * angle.cos(),
                centre[1] + radius * angle.sin(),
            );
            if x >= 0.0 && y >= 0.0 {
                self.set(x.round() as u32, y.round() as u32, rgb);
            }
        }
    }

    /// `lines` centred on the card, the first twice the size of the rest,
    /// each on a dark band so it reads over the grid.
    fn text_block(&mut self, lines: &[String], rgb: [u8; 3]) {
        let longest = lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(1)
            .max(1) as u32;
        // Glyphs are 5×7 in cells of 6×9; the name gets double size.
        let unit = (self.w * 7 / 10 / (longest * 6 * 2))
            .min(self.h * 6 / 10 / (9 * 4))
            .max(1);
        let sizes: Vec<u32> = (0..lines.len())
            .map(|i| if i == 0 { unit * 2 } else { unit })
            .collect();
        let total_h: u32 = sizes.iter().map(|s| s * 9).sum();
        if total_h + 4 > self.h || longest * 6 * unit * 2 + 4 > self.w {
            return; // Too small a screen to write on.
        }
        let mut y = self.h.saturating_sub(total_h) / 2;
        for (line, &s) in lines.iter().zip(&sizes) {
            let chars: Vec<char> = line.chars().collect();
            let line_w = chars.len() as u32 * 6 * s;
            let x0 = self.w.saturating_sub(line_w) / 2;
            for yy in y..(y + 9 * s).min(self.h) {
                for xx in x0.saturating_sub(s)..(x0 + line_w + s).min(self.w) {
                    self.set(xx, yy, [0, 0, 0]);
                }
            }
            for (i, &c) in chars.iter().enumerate() {
                self.glyph(c, x0 + i as u32 * 6 * s, y + s, s, rgb);
            }
            y += 9 * s;
        }
    }

    fn glyph(&mut self, c: char, x: u32, y: u32, s: u32, rgb: [u8; 3]) {
        let rows = glyph_rows(c);
        for (ry, row) in rows.iter().enumerate() {
            for rx in 0..5u32 {
                if row & (0b10000 >> rx) != 0 {
                    for dy in 0..s {
                        for dx in 0..s {
                            self.set(x + rx * s + dx, y + ry as u32 * s + dy, rgb);
                        }
                    }
                }
            }
        }
    }
}

/// A 5×7 bitmap of `c`, one row per entry, high bit on the left. Letters
/// with diacritics are drawn as their base letter; what has no glyph is `?`.
fn glyph_rows(c: char) -> [u8; 7] {
    let c = match c.to_ascii_uppercase() {
        'Ą' | 'ą' | 'Ä' | 'ä' | 'Á' | 'á' | 'À' | 'à' => 'A',
        'Č' | 'č' | 'Ç' | 'ç' => 'C',
        'Ę' | 'ę' | 'Ė' | 'ė' | 'É' | 'é' | 'È' | 'è' => 'E',
        'Į' | 'į' | 'Í' | 'í' => 'I',
        'Š' | 'š' => 'S',
        'Ų' | 'ų' | 'Ū' | 'ū' | 'Ü' | 'ü' | 'Ú' | 'ú' => 'U',
        'Ž' | 'ž' => 'Z',
        'Ö' | 'ö' | 'Ó' | 'ó' => 'O',
        '×' => 'X',
        other => other.to_ascii_uppercase(),
    };
    match c {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 2, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 17, 25, 21, 19, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [31, 2, 4, 2, 1, 17, 14],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 30, 1, 1, 17, 14],
        '6' => [6, 8, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 2, 12],
        ' ' => [0; 7],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ',' => [0, 0, 0, 0, 12, 4, 8],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '/' => [1, 1, 2, 4, 8, 16, 16],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '#' => [10, 10, 31, 10, 31, 10, 10],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

/// The test cards of every feed that shows them, as GPU textures, made again
/// only when a screen's name, size or place changes.
#[derive(Default)]
pub struct LedCards {
    cards: HashMap<String, Made>,
}

struct Made {
    key: (String, [u32; 2], [i32; 2], usize),
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl LedCards {
    /// Have a card for every screen of every `layouts` entry showing test
    /// cards, and drop the rest.
    pub fn prepare<'a>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layouts: impl Iterator<Item = &'a LedLayout>,
    ) {
        let mut wanted = std::collections::HashSet::new();
        for layout in layouts.filter(|l| l.test_card) {
            for (index, screen) in layout.screens.iter().enumerate() {
                let key = (screen.name.clone(), screen.size, screen.at, index);
                wanted.insert(screen.uuid.clone());
                if self.cards.get(&screen.uuid).is_some_and(|m| m.key == key) {
                    continue;
                }
                let [w, h] = screen.size.map(|v| v.max(1));
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("LED test card"),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &test_card(screen, index),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * 4),
                        rows_per_image: Some(h),
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                self.cards.insert(
                    screen.uuid.clone(),
                    Made {
                        key,
                        _texture: texture,
                        view,
                    },
                );
            }
        }
        self.cards.retain(|uuid, _| wanted.contains(uuid));
    }

    /// The card of the screen `uuid`, once prepared.
    pub fn view(&self, uuid: &str) -> Option<&wgpu::TextureView> {
        self.cards.get(uuid).map(|m| &m.view)
    }
}

/// The test cards of outputs showing theirs ([`output_card`]), as GPU
/// textures, made again only when an output's name or size changes.
#[derive(Default)]
pub struct OutputCards {
    cards: HashMap<String, MadeCard>,
}

struct MadeCard {
    key: (String, [u32; 2], usize),
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// An output wanting its card: its uuid, name, size in pixels and place in
/// the outputs list (its colour).
pub struct WantedCard {
    pub uuid: String,
    pub name: String,
    pub size: [u32; 2],
    pub index: usize,
}

impl OutputCards {
    /// Have a card for every output `wanted`, and drop the rest.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        wanted: impl Iterator<Item = WantedCard>,
    ) {
        let mut kept = std::collections::HashSet::new();
        for card in wanted {
            let key = (card.name.clone(), card.size, card.index);
            kept.insert(card.uuid.clone());
            if self.cards.get(&card.uuid).is_some_and(|m| m.key == key) {
                continue;
            }
            let [w, h] = card.size.map(|v| v.max(1));
            let (texture, view) = card_texture(
                device,
                queue,
                [w, h],
                &output_card(&card.name, [w, h], card.index),
            );
            self.cards.insert(
                card.uuid,
                MadeCard {
                    key,
                    _texture: texture,
                    view,
                },
            );
        }
        self.cards.retain(|uuid, _| kept.contains(uuid));
    }

    /// The card of the output `uuid`, once prepared.
    pub fn view(&self, uuid: &str) -> Option<&wgpu::TextureView> {
        self.cards.get(uuid).map(|m| &m.view)
    }
}

/// A card's pixels as a texture to draw from: display codes, read as such.
fn card_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    [w, h]: [u32; 2],
    px: &[u8],
) -> (wgpu::Texture, wgpu::TextureView) {
    let size = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Output test card"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        px,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        size,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::value::render::OutputSource;

    fn pixel(card: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [card[i], card[i + 1], card[i + 2]]
    }

    #[test]
    fn a_card_has_a_white_edge_its_colour_inside_it_and_its_name() {
        let screen = LedScreen::new("Cube L", [192, 128], [0, 0], OutputSource::Master);
        let card = test_card(&screen, 0);
        assert_eq!(card.len(), 192 * 128 * 4);
        for (x, y) in [(0, 0), (191, 0), (0, 127), (191, 127), (96, 0), (0, 64)] {
            assert_eq!(pixel(&card, 192, x, y), [255; 3], "edge at {x},{y}");
        }
        assert_eq!(
            pixel(&card, 192, 96, 1),
            COLOURS[0],
            "the colour just inside"
        );
        // The text: white glyph pixels on a black band, away from the edge.
        let inner: Vec<[u8; 3]> = (20..108)
            .flat_map(|y| (10..182).map(move |x| (x, y)))
            .map(|(x, y)| pixel(&card, 192, x, y))
            .collect();
        assert!(inner.contains(&[0, 0, 0]), "a band");
        assert!(
            inner.iter().filter(|p| **p == [255; 3]).count() > 50,
            "glyphs"
        );
    }

    /// An output's card is made at its pixels: at 1920×1080 its cells are
    /// square — 120 pixels both ways — and its circle round, on a 32:9 one
    /// too; never a square card stretched.
    #[test]
    fn an_outputs_card_has_square_cells_and_a_round_circle_at_its_own_size() {
        for (w, h) in [(1920u32, 1080u32), (3840, 1080)] {
            let card = output_card("TV", [w, h], 0);
            let (cx, cy) = (w / 2, h / 2);
            let cell = h / 9;
            let grid = [96, 96, 96];
            // Grid lines whole cells from the centre, across and down, where
            // nothing is drawn over them: right of the name, above it.
            let open_y = cy - cell - cell / 2;
            assert_eq!(pixel(&card, w, cx + 3 * cell, open_y), grid, "{w}×{h} a column");
            assert_eq!(pixel(&card, w, cx + 4 * cell + cell / 2, cy - cell), grid, "{w}×{h} a row");
            assert_eq!(
                pixel(&card, w, cx + 3 * cell + cell / 2, open_y),
                [32, 32, 32],
                "{w}×{h} inside a cell"
            );
            // The circle as far out sideways as up.
            let r = (h as f32 * 0.47).round() as u32;
            assert_eq!(pixel(&card, w, cx + r, cy), [255; 3], "{w}×{h} side");
            assert_eq!(pixel(&card, w, cx, cy - r), [255; 3], "{w}×{h} top");
        }
    }

    /// Its bars are apart: background between each colour; and its levels
    /// strip carries codes 0, 16, 235 and 255.
    #[test]
    fn an_outputs_card_has_bars_apart_and_the_levels_strip() {
        let (w, h) = (1920u32, 1080u32);
        let card = output_card("TV", [w, h], 0);
        let cell = h / 9;
        let row_h = cell * 3 / 5;
        let gap = cell / 8;
        let band_w = w * 4 / 5;
        let band_x = (w - band_w) / 2;
        let top = h / 2 - cell * 3 + (cell - row_h) / 2 + row_h / 2;
        let each = (band_w - gap * 7) / 8;
        assert_eq!(pixel(&card, w, band_x + each / 2, top), [255, 255, 255], "white bar");
        assert_eq!(pixel(&card, w, band_x + each + gap / 2, top), [32, 32, 32], "a gap");
        assert_eq!(pixel(&card, w, band_x + each + gap + each / 2, top), [255, 255, 0], "yellow");
        let foot = h / 2 + cell * 2 + (cell - row_h) / 2 + row_h / 2;
        let steps_w = band_w * 11 / 16;
        let levels_x = band_x + steps_w + gap * 3;
        let levels_w = band_x + band_w - levels_x;
        let patch = (levels_w - gap * 3) / 4;
        let codes: Vec<u8> = (0..4)
            .map(|i| pixel(&card, w, levels_x + i * (patch + gap) + patch / 2, foot)[0])
            .collect();
        assert_eq!(codes, LEVEL_PATCHES);
    }

    #[test]
    fn every_screen_of_a_feed_gets_its_own_colour() {
        let screen = LedScreen::new("A", [8, 8], [0, 0], OutputSource::Master);
        assert_ne!(
            pixel(&test_card(&screen, 0), 8, 4, 1),
            pixel(&test_card(&screen, 1), 8, 4, 1)
        );
    }

    #[test]
    fn letters_without_a_glyph_fall_back_sensibly() {
        assert_eq!(glyph_rows('š'), glyph_rows('S'));
        assert_eq!(glyph_rows('ų'), glyph_rows('U'));
        assert_eq!(glyph_rows('×'), glyph_rows('X'));
        assert_eq!(glyph_rows('€'), glyph_rows('?'));
    }
}
