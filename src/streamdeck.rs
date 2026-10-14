//! Stream Deck (and OpenDeck) side. One key action; a key's settings are what it does (`engine::Rule` fields plus its
//! style). A tap turns the key off and on, the image (see keyimage.rs) shows its state, and the property inspector edits
//! it (plus the shared timings and the key's style) and shows live levels.
use std::collections::HashMap;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::Storage::FileSystem::{FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindFirstChangeNotificationW, FindNextChangeNotification};
use windows::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};
use windows::core::HSTRING;

use crate::keyimage::{Icons, State, key_image};
use crate::engine::Rule;
use crate::{Config, SharedRef, save_config};

const POLL: Duration = Duration::from_millis(250);
const ACTION: &str = "com.xenside.audioduck.rule";

type Ws = WebSocket<TcpStream>;

/// What Stream Deck passes on the command line.
pub struct Args {
    port: u16,
    uuid: String,
    event: String,
}

impl Args {
    pub fn parse() -> Option<Self> {
        let args: Vec<String> = std::env::args().collect();
        let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
        Some(Self { port: arg("-port")?.parse().ok()?, uuid: arg("-pluginUUID")?, event: arg("-registerEvent")? })
    }
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn send(ws: &mut Ws, v: Value) {
    let _ = ws.send(Message::text(v.to_string()));
}

/// What a key does, from its settings.
fn setup(settings: &Value) -> Rule {
    serde_json::from_value(settings.clone()).unwrap_or_default()
}

/// Whether a key has been given something to do (until then it shows "Set up" and does nothing).
fn is_set_up(r: &Rule) -> bool {
    !r.sources.is_empty() && (r.all_others || !r.targets.is_empty())
}

/// The key's image for its settings.
fn image(shared: &SharedRef, icons: &mut Icons, settings: &Value) -> String {
    let r = setup(settings);
    let state = if !is_set_up(&r) {
        State::SetUp
    } else if !r.enabled {
        State::Off
    } else if shared.lock().unwrap().live.active.contains(&r) {
        State::Ducking // identical keys do the same thing, so they show the same state
    } else {
        State::Listening
    };
    key_image(icons, settings, state, r.sources.first().map_or("", |s| s), r.level)
}

fn set_config(shared: &SharedRef, change: impl FnOnce(&mut Config)) {
    let mut sh = shared.lock().unwrap();
    change(&mut sh.config);
    sh.rev += 1;
    save_config(&sh.config);
}

/// Adds the settings of every AudioDuck key in a profile manifest.
fn collect(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            if m.get("UUID").and_then(Value::as_str) == Some(ACTION) {
                out.push(m.get("Settings").cloned().unwrap_or_default());
            }
            m.values().for_each(|x| collect(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        _ => {}
    }
}

fn profile_dirs() -> Vec<PathBuf> {
    let root = PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("Elgato").join("StreamDeck");
    vec![root.join("ProfilesV3"), root.join("ProfilesV2")]
}

/// Sleeps until Windows reports Stream Deck writing a profile (right after a key is added, changed or deleted), then
/// sends a note. Runs until the plugin side is gone.
fn watch_profiles(changed: mpsc::Sender<()>) {
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE;
    let handles: Vec<_> = profile_dirs().iter().filter_map(|d| unsafe { FindFirstChangeNotificationW(&HSTRING::from(d.as_path()), true, filter) }.ok()).collect();
    if handles.is_empty() {
        return;
    }
    loop {
        let i = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) }.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if i >= handles.len() || changed.send(()).is_err() {
            return;
        }
        let _ = unsafe { FindNextChangeNotification(handles[i]) };
    }
}

/// Settings of every AudioDuck key in Stream Deck's saved profiles (every device, page and profile). None when they
/// can't all be read (a profile half-written, or none at all), so a bad read never silences ducking.
fn saved_keys() -> Option<Vec<Value>> {
    let (mut out, mut read_any) = (vec![], false);
    let mut dirs = profile_dirs();
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for path in entries.flatten().map(|e| e.path()) {
            if path.is_dir() {
                dirs.push(path);
            } else if path.file_name().is_some_and(|n| n == "manifest.json") {
                collect(&serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?, &mut out);
                read_any = true;
            }
        }
    }
    read_any.then_some(out)
}

/// Every key's settings: the saved ones, with each on-screen key's live settings (`current`) in place of its saved copy
/// (`on_disk`, which may lag behind a tap or an edit until Stream Deck saves). Once that copy is gone, Stream Deck has
/// saved the change and `on_disk` catches up. A leftover identical copy is harmless: identical keys do the same thing.
fn merge<'a>(mut saved: Vec<Value>, keys: impl IntoIterator<Item = (&'a mut Value, &'a Value)>) -> Vec<Value> {
    let mut out = vec![];
    for (on_disk, current) in keys {
        if let Some(i) = saved.iter().position(|s| s == on_disk) {
            saved.swap_remove(i);
        } else {
            *on_disk = current.clone();
            if let Some(i) = saved.iter().position(|s| s == current) {
                saved.swap_remove(i);
            }
        }
        out.push(current.clone());
    }
    out.extend(saved);
    out
}

/// Whether the host is Elgato's Stream Deck app (whose profiles `profile_rules` reads), not OpenDeck.
fn hosted_by_stream_deck() -> bool {
    let procs = crate::audio::processes();
    procs.get(&std::process::id()).and_then(|(_, parent)| procs.get(parent)).is_some_and(|(name, _)| name == "streamdeck.exe")
}

/// An on-screen key.
struct Key {
    settings: Value,
    on_disk: Value, // its settings as of when it appeared, until Stream Deck has saved a change (see `merge`)
    shown: String,  // image last sent
}

/// Talks to Stream Deck until it disconnects.
pub fn run(a: Args, shared: &SharedRef) {
    let Ok(stream) = TcpStream::connect(("127.0.0.1", a.port)) else { return };
    let _ = stream.set_read_timeout(Some(POLL)); // the read timeout is the poll tick
    let Ok((mut ws, _)) = tungstenite::client(format!("ws://127.0.0.1:{}", a.port), stream) else { return };
    send(&mut ws, json!({ "event": a.event, "uuid": a.uuid }));

    let mut keys: HashMap<String, Key> = HashMap::new();
    let mut icons = Icons::default();
    let mut inspector: Option<(String, String)> = None; // (action, context) of the open property inspector
    let mut sent_rev = None; // config revision the open inspector has
    let mut sent_settings = Value::Null; // key settings the open inspector has
    // Keys on other pages and profiles come from Stream Deck's saved profiles, re-read whenever Windows reports Stream
    // Deck saving one (a key added, changed or deleted). Elgato's app only: elsewhere, on-screen keys only.
    let from_profiles = hosted_by_stream_deck();
    let (changed_tx, changed) = mpsc::channel();
    if from_profiles {
        std::thread::spawn(move || watch_profiles(changed_tx));
    }
    let mut dirty = true; // the list of what every key does needs rebuilding
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                let ctx = v["context"].as_str().unwrap_or_default().to_string();
                let action = v["action"].as_str().unwrap_or_default().to_string();
                let settings = v["payload"]["settings"].clone();
                match v["event"].as_str().unwrap_or_default() {
                    "willAppear" => {
                        // Earlier versions titled the key; the image carries everything now.
                        send(&mut ws, json!({ "event": "setTitle", "context": ctx, "payload": { "title": "", "target": 0 } }));
                        keys.insert(ctx, Key { on_disk: settings.clone(), settings, shown: String::new() });
                        dirty = true;
                    }
                    "didReceiveSettings" => {
                        if let Some(k) = keys.get_mut(&ctx) {
                            k.settings = settings;
                            dirty = true;
                        }
                    }
                    "willDisappear" => {
                        keys.remove(&ctx);
                        dirty = true;
                    }
                    "keyUp" => match keys.get_mut(&ctx) {
                        Some(k) if is_set_up(&setup(&k.settings)) => {
                            let on = setup(&k.settings).enabled;
                            k.settings["enabled"] = (!on).into();
                            let settings = k.settings.clone();
                            send(&mut ws, json!({ "event": "setSettings", "context": ctx, "payload": settings }));
                            dirty = true;
                        }
                        _ => send(&mut ws, json!({ "event": "showAlert", "context": ctx })), // not set up yet
                    },
                    "propertyInspectorDidAppear" => {
                        shared.lock().unwrap().inspector_open = true;
                        inspector = Some((action, ctx));
                        sent_rev = None;
                        sent_settings = Value::Null;
                    }
                    "propertyInspectorDidDisappear" => {
                        shared.lock().unwrap().inspector_open = false;
                        inspector = None;
                    }
                    "sendToPlugin" => {
                        // The inspector sends the shared timings on every change, or {} when it loads and wants them.
                        if let Ok(config) = serde_json::from_value::<Config>(v["payload"]["config"].clone()) {
                            set_config(shared, |c| *c = config);
                        } else {
                            sent_rev = None;
                            sent_settings = Value::Null;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return, // Stream Deck closed the connection
        }
        if changed.try_iter().count() > 0 {
            dirty = true;
        }
        if dirty {
            let all = if from_profiles {
                saved_keys().map(|saved| merge(saved, keys.values_mut().map(|k| (&mut k.on_disk, &k.settings))))
            } else {
                Some(keys.values().map(|k| k.settings.clone()).collect())
            };
            // A failed read (Stream Deck mid-write) keeps the last list and `dirty`: retried next tick.
            if let Some(all) = all {
                dirty = false;
                let rules: Vec<Rule> = all.iter().map(setup).filter(is_set_up).collect();
                let mut sh = shared.lock().unwrap();
                if sh.rules != rules {
                    sh.rules = rules;
                    sh.rev += 1;
                }
            }
        }
        for (ctx, k) in keys.iter_mut() {
            let now = image(shared, &mut icons, &k.settings);
            if now != k.shown {
                send(&mut ws, json!({ "event": "setImage", "context": ctx, "payload": { "image": now, "target": 0 } }));
                k.shown = now;
            }
        }
        if let Some((action, ctx)) = &inspector {
            let (action, ctx) = (action.clone(), ctx.clone());
            let mut payload = json!({});
            {
                let sh = shared.lock().unwrap();
                payload["live"] = serde_json::to_value(&sh.live).unwrap_or_default();
                if sent_rev != Some(sh.rev) {
                    sent_rev = Some(sh.rev); // config changed (here or by the inspector itself): resend it
                    payload["config"] = serde_json::to_value(&sh.config).unwrap_or_default();
                }
                if let Some(k) = keys.get(&ctx) {
                    let r = setup(&k.settings);
                    payload["ducking"] = (r.enabled && is_set_up(&r) && sh.live.active.contains(&r)).into();
                    if k.settings != sent_settings {
                        sent_settings = k.settings.clone(); // e.g. a tap turned it off: the inspector must not undo that
                        payload["settings"] = sent_settings.clone();
                    }
                }
            }
            send(&mut ws, json!({ "event": "sendToPropertyInspector", "action": action, "context": ctx, "payload": payload }));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn collect_finds_keys_at_any_depth() {
        let page = json!({ "Controllers": [{ "Actions": {
            "0,0": { "UUID": "com.xenside.audioduck.rule", "Settings": { "sources": ["discord.exe"] } },
            "1,0": { "UUID": "com.elgato.discord.mute", "Settings": { "sources": ["x.exe"] } },
            "2,0": { "UUID": "com.elgato.streamdeck.multiactions", "Settings": {}, "Actions": [[{ "UUID": "com.xenside.audioduck.rule", "Settings": { "level": 0.5 } }]] }
        } }] });
        let mut out = vec![];
        super::collect(&page, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&json!({ "sources": ["discord.exe"] })) && out.contains(&json!({ "level": 0.5 })));
    }

    #[test]
    fn merge_prefers_live_settings_and_catches_up_once_saved() {
        let (old, new, other) = (json!({ "enabled": true }), json!({ "enabled": false }), json!({ "level": 0.5 }));
        // Tapped off, not saved yet: the stale saved copy is replaced, the hidden key stays.
        let mut on_disk = old.clone();
        assert_eq!(super::merge(vec![old.clone(), other.clone()], [(&mut on_disk, &new)]), [new.clone(), other.clone()]);
        assert_eq!(on_disk, old);
        // Saved: on_disk catches up and the saved copy isn't counted twice.
        assert_eq!(super::merge(vec![new.clone(), other.clone()], [(&mut on_disk, &new)]), [new.clone(), other.clone()]);
        assert_eq!(on_disk, new);
        // Deleted: gone from the profile and off screen.
        assert_eq!(super::merge(vec![other.clone()], []), [other]);
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(super::base64(i.as_bytes()), o);
        }
    }
}
