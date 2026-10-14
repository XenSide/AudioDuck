//! Key images in Foreground Boost's design: flat themed tile ("default" Stream Deck look or "duotone" Duotone Essentials
//! colours), the trigger app's icon on top, and a pill at the bottom with the ducked volume. Drawn as SVG data URIs.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::Shell::SHDefExtractIconW;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::{HSTRING, PWSTR};

use crate::streamdeck::base64;

const ICON: u32 = 64;
const RETRY: Duration = Duration::from_secs(3);

/// Same palettes as Foreground Boost / Volume Controller NEO: (background, secondary, accent).
const PRESETS: [(&str, [&str; 3]); 4] = [
    ("blue", ["#3b4c66", "#4c70a3", "#67a4ff"]),
    ("orange", ["#6a5542", "#a67b50", "#ffb367"]),
    ("purple", ["#4e3d66", "#774ea3", "#b367ff"]),
    ("red", ["#623d46", "#a14e5d", "#ff6783"]),
];

pub struct Theme {
    duo: bool,
    bg: String,
    fg: String,
    sec: String, // in "default": the muted grey
}

fn rgb(h: &str) -> [u8; 3] {
    let n = u32::from_str_radix(h.trim_start_matches('#'), 16).unwrap_or(0);
    [(n >> 16) as u8, (n >> 8) as u8, n as u8]
}

fn is_hex(h: &str) -> bool {
    h.len() == 7 && h.starts_with('#') && h[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Key settings: `style` ("default" | "duotone"), `duoPreset` (a preset name or "custom"), `duoBg`, `duoAccent`.
pub fn theme(s: &Value) -> Theme {
    let get = |k: &str| s[k].as_str().unwrap_or_default();
    if get("style") != "duotone" {
        return Theme { duo: false, bg: "#000000".into(), fg: "#ffffff".into(), sec: "#8a8a8a".into() };
    }
    let (bg, accent, sec) = if get("duoPreset") == "custom" && is_hex(get("duoBg")) && is_hex(get("duoAccent")) {
        // Secondary: accent at 40% over the background.
        let (b, a) = (rgb(get("duoBg")), rgb(get("duoAccent")));
        let mix: Vec<String> = (0..3).map(|i| format!("{:02x}", (b[i] as f32 + (a[i] as f32 - b[i] as f32) * 0.4).round() as u8)).collect();
        (get("duoBg").to_string(), get("duoAccent").to_string(), format!("#{}", mix.concat()))
    } else {
        let p = PRESETS.iter().find(|(n, _)| *n == get("duoPreset")).unwrap_or(&PRESETS[0]).1;
        (p[0].into(), p[2].into(), p[1].into())
    };
    Theme { duo: true, bg, fg: accent, sec }
}

/// Duotone Essentials look: split the icon into its two best-separated luminance groups (Otsu) and paint them in the
/// secondary and accent tones. Port of Foreground Boost's `duoIconRgba`.
fn duo_icon(rgba: &mut [u8], sec: [u8; 3], pri: [u8; 3]) {
    let lum = |p: &[u8]| (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) / 255.0;
    let solid = || rgba.chunks(4).filter(|p| p[3] > 128);
    let (lo, hi) = solid().fold((1f32, 0f32), |(lo, hi), p| (lo.min(lum(p)), hi.max(lum(p))));
    let span = (hi - lo).max(0.05);
    let norm = |p: &[u8]| (lum(p) - lo) / span;
    let mut hist = [0u32; 64];
    let mut n = 0;
    for p in solid() {
        hist[((64.0 * norm(p)) as usize).min(63)] += 1;
        n += 1;
    }
    let tot: f64 = hist.iter().enumerate().map(|(k, &c)| c as f64 * k as f64).sum();
    let (mut best, mut cut, mut w0, mut s0) = (0f64, 0.5f32, 0u32, 0f64);
    for (k, &h) in hist.iter().enumerate().take(63) {
        w0 += h;
        s0 += h as f64 * k as f64;
        let w1 = n - w0;
        if w0 == 0 || w1 == 0 {
            continue;
        }
        let v = w0 as f64 * w1 as f64 * (s0 / w0 as f64 - (tot - s0) / w1 as f64).powi(2);
        if v > best {
            best = v;
            cut = (k + 1) as f32 / 64.0;
        }
    }
    for p in rgba.chunks_mut(4) {
        let t = ((norm(p) - cut) / 0.08 + 0.5).clamp(0.0, 1.0); // soft edge at the cut
        for k in 0..3 {
            p[k] = (sec[k] as f32 + (pri[k] as f32 - sec[k] as f32) * t).round() as u8;
        }
    }
}

fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// ICON×ICON RGBA of the exe's main icon.
fn exe_icon(path: &str) -> Option<Vec<u8>> {
    unsafe {
        let mut icon = HICON::default();
        SHDefExtractIconW(&HSTRING::from(path), 0, 0, Some(&mut icon), None, ICON).ok().ok()?;
        let mut info = ICONINFO::default();
        let got = GetIconInfo(icon, &mut info);
        let _ = DestroyIcon(icon);
        got.ok()?;
        let dc = CreateCompatibleDC(None);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: ICON as i32,
                biHeight: -(ICON as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (ICON * ICON * 4) as usize];
        let rows = GetDIBits(dc, info.hbmColor, 0, ICON, Some(px.as_mut_ptr() as _), &mut bmi, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        if rows == 0 {
            return None;
        }
        let has_alpha = px.chunks(4).any(|p| p[3] != 0);
        for p in px.chunks_mut(4) {
            p.swap(0, 2); // BGRA -> RGBA
            if !has_alpha {
                p[3] = 255; // shortcut: old icons without alpha come out as an opaque square
            }
        }
        Some(px)
    }
}

/// App icons by exe name, looked up from a running process (an app that isn't running yet is retried every RETRY).
#[derive(Default)]
pub struct Icons {
    rgba: HashMap<String, Option<Vec<u8>>>,
    tried: HashMap<String, Instant>,
    png: HashMap<(String, bool, String), String>, // (exe, duo, colours) -> base64 PNG
}

impl Icons {
    fn png_b64(&mut self, exe: &str, t: &Theme) -> Option<String> {
        let key = (exe.to_string(), t.duo, format!("{}{}", t.sec, t.fg));
        if let Some(p) = self.png.get(&key) {
            return Some(p.clone());
        }
        if !self.rgba.contains_key(exe) {
            if self.tried.get(exe).is_some_and(|t| t.elapsed() < RETRY) {
                return None;
            }
            self.tried.insert(exe.to_string(), Instant::now());
            let pid = crate::audio::processes().into_iter().find(|(_, (n, _))| n == exe)?.0;
            self.rgba.insert(exe.to_string(), image_path(pid).and_then(|p| exe_icon(&p)));
        }
        let mut px = self.rgba.get(exe)?.clone()?;
        if t.duo {
            duo_icon(&mut px, rgb(&t.sec), rgb(&t.fg));
        }
        let b = base64(&crate::icon::encode(&px, ICON));
        self.png.insert(key, b.clone());
        Some(b)
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    SetUp, // no rule on this key yet
    Off,
    Listening,
    Ducking,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

const FONT: &str = r#"font-family="Segoe UI,Arial,sans-serif" font-weight="700""#;

/// `app`: the rule's (first) trigger app, `level`: its ducked volume (0..1).
pub fn key_image(icons: &mut Icons, settings: &Value, state: State, app: &str, level: f32) -> String {
    let t = theme(settings);
    let lit = matches!(state, State::Listening | State::Ducking);
    let opacity = if lit { "1" } else { "0.45" };
    let mut p = vec![r#"<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">"#.to_string()];
    p.push(format!(r#"<rect width="144" height="144" fill="{}"/>"#, t.bg));
    match (state, icons.png_b64(app, &t)) {
        (State::SetUp, _) => {
            // AudioDuck's equalizer glyph.
            let bar = if t.duo { &t.sec } else { &t.fg };
            for (x, h) in [(46.0, 22.0), (66.0, 40.0), (86.0, 30.0)] {
                p.push(format!(r#"<rect x="{x}" y="{}" width="12" height="{h}" rx="6" fill="{bar}" opacity="0.45"/>"#, 46.0 - h / 2.0));
            }
        }
        (_, Some(png)) => p.push(format!(r#"<image x="40" y="14" width="64" height="64" opacity="{opacity}" href="data:image/png;base64,{png}"/>"#)),
        (_, None) => {
            // App not seen running yet: its name instead of the icon.
            let mut chars = app.trim_end_matches(".exe").chars().take(9);
            let name: String = chars.next().into_iter().flat_map(char::to_uppercase).chain(chars).collect();
            p.push(format!(r#"<text x="72" y="54" text-anchor="middle" {FONT} font-size="20" fill="{}" opacity="{opacity}">{}</text>"#, t.fg, esc(&name)));
        }
    }
    // Fixed-width pill like Foreground Boost: Stream Deck draws Segoe UI wider than browsers, so never size to the text.
    let ink = match state {
        State::Ducking => &t.bg,
        State::Listening => &t.fg,
        _ => &t.sec,
    };
    if state == State::Ducking {
        p.push(format!(r#"<rect x="12" y="95" width="120" height="36" rx="18" fill="{}"/>"#, t.fg));
    }
    let label = match state {
        State::SetUp => "Set up".to_string(),
        State::Off => "Off".to_string(),
        _ => {
            p.push(format!(r#"<path d="M26 111l6 7 6-7" fill="none" stroke="{ink}" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round"/>"#));
            format!("{}%", (level * 100.0).round())
        }
    };
    let x = if matches!(state, State::SetUp | State::Off) { 72 } else { 84 };
    p.push(format!(r#"<text x="{x}" y="121" text-anchor="middle" {FONT} font-size="22" fill="{ink}">{label}</text>"#));
    p.push("</svg>".into());
    format!("data:image/svg+xml;base64,{}", base64(p.concat().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn themes() {
        assert_eq!(theme(&json!({})).bg, "#000000");
        let t = theme(&json!({ "style": "duotone", "duoPreset": "red" }));
        assert_eq!((t.bg.as_str(), t.sec.as_str(), t.fg.as_str()), ("#623d46", "#a14e5d", "#ff6783"));
        let t = theme(&json!({ "style": "duotone", "duoPreset": "custom", "duoBg": "#000000", "duoAccent": "#ffffff" }));
        assert_eq!(t.sec, "#666666");
        assert_eq!(theme(&json!({ "style": "duotone", "duoPreset": "custom", "duoBg": "bad" })).bg, "#3b4c66"); // falls back to blue
    }

    #[test]
    fn duo_icon_maps_dark_and_light_to_the_two_tones() {
        let mut px = [0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0, 255, 255, 255, 128].to_vec();
        duo_icon(&mut px, [1, 2, 3], [9, 8, 7]);
        // Light -> accent; dark -> mostly secondary (the soft edge at the cut blends it a little, as in Foreground Boost).
        assert_eq!(&px[4..8], [9, 8, 7, 255]);
        assert!(px[0] < 5 && px[3] == 255 && px[11] == 0 && px[15] == 128, "{px:?}");
    }
}
