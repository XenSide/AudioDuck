#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod audio;
mod control;
mod engine;
mod icon;
mod ui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::{Ducker, Rule, Settings};
pub use icon::{ACCENT, DUCKING, OFF, icon_rgba};
use serde::{Deserialize, Serialize};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::*;

const TICK_MS: u32 = 30;
const REFRESH: Duration = Duration::from_secs(1);

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub settings: Settings,
    pub rules: Vec<Rule>,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true, settings: Settings::default(), rules: vec![] }
    }
}

/// What the engine measured, for the window.
#[derive(Default)]
pub struct Live {
    pub peaks: Vec<(String, f32)>,
    pub gains: HashMap<String, f32>,
    pub volumes: HashMap<String, f32>,
    pub ducking: bool,
}

pub struct Shared {
    pub config: Config,
    pub rev: u64, // bumped on every config change so the engine picks it up
    pub live: Live,
    pub hwnd: isize,
}

pub type SharedRef = Arc<Mutex<Shared>>;

fn app_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("AudioDuck")
}

/// The Python version's data dirs (Store Python virtualises %APPDATA% into its package dir).
fn python_dirs() -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("Packages")) else { return vec![] };
    dir.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("PythonSoftwareFoundation")).map(|e| e.path().join(r"LocalCache\Roaming\AudioDuck")).collect()
}

/// First run takes over the Python version's file (config, or volumes it left ducked).
fn read_with_fallback(file: &str) -> Option<String> {
    std::iter::once(app_dir()).chain(python_dirs()).find_map(|d| std::fs::read_to_string(d.join(file)).ok())
}

fn load_config() -> Config {
    read_with_fallback("config.json").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_config(c: &Config) {
    let _ = std::fs::create_dir_all(app_dir());
    let _ = std::fs::write(app_dir().join("config.json"), serde_json::to_string_pretty(c).unwrap_or_default());
}

fn tray_icon(color: [u8; 3]) -> Option<tray_icon::Icon> {
    tray_icon::Icon::from_rgba(icon_rgba(color, 32), 32, 32).ok()
}

/// On screen: visible and not minimized (Windows counts minimized windows as visible).
pub fn window_shown(hwnd: isize) -> bool {
    unsafe { IsWindowVisible(HWND(hwnd as _)).as_bool() && !IsIconic(HWND(hwnd as _)).as_bool() }
}

pub fn show_window(hwnd: isize) {
    unsafe {
        let h = HWND(hwnd as _);
        let _ = ShowWindow(h, SW_SHOW);
        let _ = ShowWindow(h, SW_RESTORE);
        let _ = SetForegroundWindow(h);
    }
}

/// Audio + tray thread. Owns every COM object; talks to the window only through `Shared`.
fn engine_thread(shared: SharedRef, ctx: eframe::egui::Context) {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let state = app_dir().join("ducked_state.json");
    if !state.exists() && let Some(s) = read_with_fallback("ducked_state.json") {
        let _ = std::fs::create_dir_all(app_dir());
        let _ = std::fs::write(&state, s);
    }
    let mut mixer = audio::Mixer::new(state);
    let mut cfg = shared.lock().unwrap().config.clone();

    let open = MenuItem::new("Apri AudioDuck", true, None);
    let toggle = CheckMenuItem::new("Ducking attivo", true, cfg.enabled, None);
    let quit = MenuItem::new("Esci", true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&open, &toggle, &PredefinedMenuItem::separator(), &quit]);
    // tray-icon needs a message loop on its thread: the wait below pumps it.
    let tray = TrayIconBuilder::new().with_menu(Box::new(menu)).with_menu_on_left_click(false).with_tooltip("AudioDuck").build().ok();

    let show = |shared: &SharedRef| {
        // Copy the handle and release the lock first: ShowWindow waits for the UI thread, which may be waiting for this lock.
        let hwnd = shared.lock().unwrap().hwnd;
        show_window(hwnd);
        ctx.request_repaint();
    };
    let set_enabled = |f: &dyn Fn(bool) -> bool| {
        let mut sh = shared.lock().unwrap();
        sh.config.enabled = f(sh.config.enabled);
        sh.rev += 1;
        save_config(&sh.config);
        ctx.request_repaint();
    };
    control::create_window();
    let mut ducker = Ducker::default();
    let mut rev = u64::MAX;
    let (mut last_tick, mut last_refresh) = (Instant::now(), Instant::now() - REFRESH);
    let mut was = (false, None::<bool>); // (enabled, ducking) last shown in tray
    let mut was_visible = false;

    loop {
        unsafe {
            MsgWaitForMultipleObjects(None, false, TICK_MS, QS_ALLINPUT);
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        while let Ok(e) = TrayIconEvent::receiver().try_recv() {
            if matches!(e, TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }) {
                show(&shared);
            }
        }
        while let Ok(e) = MenuEvent::receiver().try_recv() {
            if e.id == *open.id() {
                show(&shared);
            } else if e.id == *toggle.id() {
                set_enabled(&|_| toggle.is_checked());
            } else if e.id == *quit.id() {
                mixer.restore_all();
                std::process::exit(0);
            }
        }
        match control::REQUEST.swap(control::QUERY, std::sync::atomic::Ordering::SeqCst) {
            control::TOGGLE => set_enabled(&|on| !on),
            control::OPEN => show(&shared),
            _ => {}
        }

        let now = Instant::now();
        let dt = (now - last_tick).as_secs_f64().min(0.2) * 1000.0;
        last_tick = now;
        let hwnd = {
            let sh = shared.lock().unwrap();
            if sh.rev != rev {
                rev = sh.rev;
                if sh.config.rules != cfg.rules {
                    ducker.rules_changed();
                    last_refresh -= REFRESH; // measure new sources right away
                }
                cfg = sh.config.clone();
            }
            sh.hwnd
        };
        let visible = window_shown(hwnd);
        if visible != was_visible {
            was_visible = visible;
            last_refresh -= REFRESH;
        }
        if now - last_refresh >= REFRESH {
            last_refresh = now;
            // Hidden: only rule sources need accurate levels. Visible: every app, for the meters.
            mixer.refresh(|app| visible || cfg.rules.iter().any(|r| r.enabled && r.sources.iter().any(|s| s == app)));
        }
        let peaks = mixer.peaks();
        if cfg.enabled {
            mixer.apply(ducker.step(&cfg.rules, &cfg.settings, &peaks, dt));
        } else if was.0 {
            mixer.restore_all();
            ducker.gains.clear();
        }
        let ducking = cfg.enabled && ducker.gains.values().any(|&g| g < 0.999);
        if (cfg.enabled, Some(ducking)) != was {
            was = (cfg.enabled, Some(ducking));
            if let Some(t) = &tray {
                let _ = t.set_icon(tray_icon(if ducking { DUCKING } else if cfg.enabled { ACCENT } else { OFF }));
                let _ = t.set_tooltip(Some(format!("AudioDuck — {}", if ducking { "sta abbassando" } else if cfg.enabled { "attivo" } else { "in pausa" })));
            }
            toggle.set_checked(cfg.enabled);
            control::STATE.store(if ducking { control::DUCKING } else if cfg.enabled { control::ACTIVE } else { control::PAUSED }, std::sync::atomic::Ordering::SeqCst);
        }
        if visible {
            let mut list: Vec<_> = peaks.into_iter().collect();
            list.sort_by(|a, b| a.0.cmp(&b.0));
            let volumes = mixer.volumes();
            let mut sh = shared.lock().unwrap();
            sh.live = Live { peaks: list, gains: ducker.gains.clone(), volumes, ducking };
        }
    }
}

fn main() -> eframe::Result {
    // Two instances would each take the other's ducked volume as the user's and push it down for good.
    let _single = unsafe { CreateMutexW(None, false, w!("AudioDuck.SingleInstance")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if let Ok(h) = unsafe { FindWindowW(w!("Window Class"), w!("AudioDuck")) } {
            show_window(h.0 as isize);
        }
        return Ok(());
    }
    // Lets the Stream Deck plugin start AudioDuck when it isn't running.
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::create_dir_all(app_dir());
        let _ = std::fs::write(app_dir().join("exe_path"), exe.to_string_lossy().as_bytes());
    }
    let start_hidden = std::env::args().any(|a| a == "--tray");
    let shared: SharedRef = Arc::new(Mutex::new(Shared { config: load_config(), rev: 0, live: Live::default(), hwnd: 0 }));
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("AudioDuck")
            .with_inner_size([640.0, 780.0])
            .with_min_inner_size([480.0, 420.0])
            .with_visible(!start_hidden)
            .with_icon(eframe::egui::IconData { rgba: icon_rgba(ACCENT, 64), width: 64, height: 64 }),
        ..Default::default()
    };
    eframe::run_native(
        "AudioDuck",
        options,
        Box::new(move |cc| {
            let app = ui::App::new(cc, shared.clone(), start_hidden);
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || engine_thread(shared, ctx));
            Ok(Box::new(app))
        }),
    )
}
