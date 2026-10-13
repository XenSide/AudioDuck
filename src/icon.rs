//! App icon, shared with the Stream Deck plugin (`src/streamdeck.rs`) and `examples/icons.rs`.

pub const ACCENT: [u8; 3] = [124, 156, 255];
pub const DUCKING: [u8; 3] = [255, 180, 84];
pub const OFF: [u8; 3] = [90, 95, 110];

/// Rounded square with three equalizer bars, RGBA.
pub fn icon_rgba(color: [u8; 3], size: u32) -> Vec<u8> {
    let s = size as f32;
    let inside_rrect = |x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        let dx = (x0 + r - x).max(x - (x1 - r)).max(0.0);
        let dy = (y0 + r - y).max(y - (y1 - r)).max(0.0);
        (r + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0) // antialiased coverage
    };
    let bars = [(0.22, 0.31), (0.41, 0.56), (0.59, 0.41)]; // (x, height) as fractions of size
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let bg = inside_rrect(x, y, 0.0, 0.0, s, s, s * 0.25);
            let bar = bars.iter().map(|&(bx, h)| inside_rrect(x, y, bx * s, (0.5 - h / 2.0) * s, (bx + 0.13) * s, (0.5 + h / 2.0) * s, s * 0.065)).fold(0.0, f32::max);
            let mix = |c: u8| (c as f32 * (1.0 - bar) + 21.0 * bar) as u8;
            out.extend([mix(color[0]), mix(color[1]), mix(color[2]), (bg * 255.0) as u8]);
        }
    }
    out
}

/// PNG file bytes, for the plugin's key images and manifest icons.
#[allow(dead_code)] // not used by the app itself
pub fn png(color: [u8; 3], size: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = png::Encoder::new(&mut buf, size, size);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut w| w.write_image_data(&icon_rgba(color, size))).expect("png encode");
    buf
}
