//! Measures how fast the engine notices a source app playing: `cargo run --release --example latency -- <source.exe> [secs]`.
//! Runs the same mixer/ducker loop as the plugin with one rule (source -> a dummy target) and prints, as Unix ms,
//! when the rule turns active and inactive, plus any loop iteration or refresh slower than expected.
#![allow(dead_code)]
#[path = "../src/audio.rs"]
mod audio;
#[path = "../src/engine.rs"]
mod engine;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let source = args.get(1).cloned().unwrap_or("powershell.exe".into());
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15);
    let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
    let state = std::env::temp_dir().join("audioduck-latency-state.json");
    let mut mixer = audio::Mixer::new(state);
    let rules = vec![engine::Rule { id: 1, sources: vec![source.clone()], targets: vec!["nonexistent-target.exe".into()], ..Default::default() }];
    let settings = engine::Settings::default();
    let mut ducker = engine::Ducker::default();
    let (tick, refresh_every) = (Duration::from_millis(30), Duration::from_secs(1));
    let start = Instant::now();
    let (mut last, mut last_refresh, mut was) = (Instant::now(), Instant::now() - refresh_every, false);
    println!("ready {} source={source}", now_ms());
    while start.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(tick);
        let now = Instant::now();
        let dt = (now - last).as_secs_f64() * 1000.0;
        last = now;
        if dt > 60.0 {
            println!("slow tick {dt:.0} ms at {}", now_ms());
        }
        if now - last_refresh >= refresh_every {
            last_refresh = now;
            let t = Instant::now();
            mixer.refresh(|app| app == source, |app| app == source);
            let took = t.elapsed().as_millis();
            if took > 20 {
                println!("refresh took {took} ms at {}", now_ms());
            }
        }
        let peaks = mixer.peaks();
        ducker.step(&rules, &settings, &peaks, dt.min(200.0));
        if ducker.active[0] != was {
            was = ducker.active[0];
            println!("{} {} (peak {:.3})", if was { "ACTIVE" } else { "inactive" }, now_ms(), peaks.get(&source).copied().unwrap_or(0.0));
        }
    }
}
