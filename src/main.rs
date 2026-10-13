#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! AudioDuck runs as a Stream Deck / OpenDeck plugin: no window, no tray. Each key is one rule, set up in that key's
//! property inspector (streamdeck/…/ui/rule.html); tapping the key turns its rule off and on.
mod audio;
mod engine;
mod icon;
mod streamdeck;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::{Ducker, Rule, Settings};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

const TICK: Duration = Duration::from_millis(30);
const REFRESH: Duration = Duration::from_secs(1);

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    pub rules: Vec<Rule>,
}

impl Config {
    /// Gives every rule without one a fresh id.
    pub fn ensure_ids(&mut self) {
        let mut next = self.rules.iter().map(|r| r.id).max().unwrap_or(0);
        for r in self.rules.iter_mut().filter(|r| r.id == 0) {
            next += 1;
            r.id = next;
        }
    }
}


/// What the engine measured. Per-app values are only filled while a property inspector is open.
#[derive(Default, Serialize)]
pub struct Live {
    pub peaks: Vec<(String, f32)>,
    pub gains: HashMap<String, f32>,
    pub volumes: HashMap<String, f32>,
    /// Ids of the rules ducking right now.
    pub active_rules: Vec<u64>,
}

pub struct Shared {
    pub config: Config,
    pub rev: u64, // bumped on every config change so the engine picks it up
    pub live: Live,
    pub inspector_open: bool,
    pub quit: bool,
}

pub type SharedRef = Arc<Mutex<Shared>>;

fn app_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("AudioDuck")
}

fn load_config() -> Config {
    std::fs::read_to_string(app_dir().join("config.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_config(c: &Config) {
    let _ = std::fs::create_dir_all(app_dir());
    let _ = std::fs::write(app_dir().join("config.json"), serde_json::to_string_pretty(c).unwrap_or_default());
}

/// Audio loop. Owns every COM object; talks to the plugin side only through `Shared`.
fn engine(shared: SharedRef) {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let mut mixer = audio::Mixer::new(app_dir().join("ducked_state.json"));
    let mut cfg = shared.lock().unwrap().config.clone();
    let mut ducker = Ducker::default();
    let mut rev = u64::MAX;
    let (mut last_tick, mut last_refresh) = (Instant::now(), Instant::now() - REFRESH);
    let mut was_open = false;

    loop {
        std::thread::sleep(TICK);
        let now = Instant::now();
        let dt = (now - last_tick).as_secs_f64().min(0.2) * 1000.0;
        last_tick = now;
        let open = {
            let sh = shared.lock().unwrap();
            if sh.quit {
                mixer.restore_all();
                return;
            }
            if sh.rev != rev {
                rev = sh.rev;
                if sh.config.rules != cfg.rules {
                    ducker.rules_changed();
                    last_refresh -= REFRESH; // measure new sources right away
                }
                cfg = sh.config.clone();
            }
            sh.inspector_open
        };
        if open != was_open {
            was_open = open;
            last_refresh -= REFRESH;
        }
        if now - last_refresh >= REFRESH {
            last_refresh = now;
            // Inspector closed: only rule sources need accurate levels. Open: every app, for the meters.
            mixer.refresh(|app| open || cfg.rules.iter().any(|r| r.enabled && r.sources.iter().any(|s| s == app)));
        }
        let peaks = mixer.peaks();
        mixer.apply(ducker.step(&cfg.rules, &cfg.settings, &peaks, dt));
        let active_rules = cfg.rules.iter().zip(&ducker.active).filter(|&(_, &a)| a).map(|(r, _)| r.id).collect();
        let mut sh = shared.lock().unwrap();
        sh.live.active_rules = active_rules;
        if open {
            let mut list: Vec<_> = peaks.into_iter().collect();
            list.sort_by(|a, b| a.0.cmp(&b.0));
            sh.live.peaks = list;
            sh.live.gains = ducker.gains.clone();
            sh.live.volumes = mixer.volumes();
        }
    }
}

fn main() {
    let Some(plugin) = streamdeck::Args::parse() else { return }; // only runs when started by Stream Deck / OpenDeck
    // Two instances would each take the other's ducked volume as the user's and push it down for good.
    let _single = unsafe { CreateMutexW(None, false, w!("AudioDuck.SingleInstance")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }
    let mut config = load_config();
    config.ensure_ids();
    let shared: SharedRef = Arc::new(Mutex::new(Shared { config, rev: 0, live: Live::default(), inspector_open: false, quit: false }));
    let sd = shared.clone();
    std::thread::spawn(move || {
        streamdeck::run(plugin, &sd);
        sd.lock().unwrap().quit = true; // Stream Deck is gone: restore volumes and exit with it
    });
    engine(shared);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_ids_keeps_existing_and_numbers_new_ones_after_the_highest() {
        let mut c = Config { rules: vec![Rule { id: 5, ..Default::default() }, Rule::default(), Rule::default()], ..Default::default() };
        c.ensure_ids();
        assert_eq!(c.rules.iter().map(|r| r.id).collect::<Vec<_>>(), [5, 6, 7]);
    }
}
