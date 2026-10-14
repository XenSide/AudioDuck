//! Stream Deck (and OpenDeck) side. One key action, one rule per key (settings `{"rule": id, style…}`): a tap turns the rule
//! off and on, the image (see keyimage.rs) shows its state, and the property inspector edits it (plus the shared timings
//! and the key's style) and shows live levels.
use std::collections::{HashMap, HashSet};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use crate::keyimage::{Icons, State, key_image};
use crate::{Config, SharedRef, save_config};

const POLL: Duration = Duration::from_millis(250);
const PRUNE: Duration = Duration::from_secs(5);
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

/// The key's image for its settings (`rule` id and style).
fn image(shared: &SharedRef, icons: &mut Icons, settings: &Value) -> String {
    let id = settings["rule"].as_u64().unwrap_or(0);
    let (state, app, level) = {
        let sh = shared.lock().unwrap();
        match sh.config.rules.iter().find(|r| r.id == id) {
            None => (State::SetUp, String::new(), 0.0),
            Some(r) => {
                let state = if !r.enabled {
                    State::Off
                } else if sh.live.active_rules.contains(&id) {
                    State::Ducking
                } else {
                    State::Listening
                };
                (state, r.sources.first().cloned().unwrap_or_default(), r.level)
            }
        }
    };
    key_image(icons, settings, state, &app, level)
}

fn set_config(shared: &SharedRef, change: impl FnOnce(&mut Config)) {
    let mut sh = shared.lock().unwrap();
    change(&mut sh.config);
    sh.config.ensure_ids(); // rules new from the inspector have no id yet
    sh.rev += 1;
    save_config(&sh.config);
}

/// Adds the rule id of every AudioDuck key in a profile manifest.
fn collect(v: &Value, ids: &mut HashSet<u64>) {
    match v {
        Value::Object(m) => {
            if m.get("UUID").and_then(Value::as_str) == Some(ACTION)
                && let Some(id) = m.get("Settings").and_then(|s| s["rule"].as_u64())
            {
                ids.insert(id);
            }
            m.values().for_each(|x| collect(x, ids));
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, ids)),
        _ => {}
    }
}

/// Rule ids the keys in Stream Deck's saved profiles (every device, page and profile) point at. None when they can't
/// all be read (a profile half-written, or none at all), so nothing gets deleted on a bad read.
fn profile_rules() -> Option<HashSet<u64>> {
    let root = PathBuf::from(std::env::var_os("APPDATA")?).join("Elgato").join("StreamDeck");
    let (mut ids, mut read_any) = (HashSet::new(), false);
    let mut dirs = vec![root.join("ProfilesV3"), root.join("ProfilesV2")];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for path in entries.flatten().map(|e| e.path()) {
            if path.is_dir() {
                dirs.push(path);
            } else if path.file_name().is_some_and(|n| n == "manifest.json") {
                collect(&serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?, &mut ids);
                read_any = true;
            }
        }
    }
    read_any.then_some(ids)
}

/// Whether the host is Elgato's Stream Deck app (whose profiles `profile_rules` reads), not OpenDeck.
fn hosted_by_stream_deck() -> bool {
    let procs = crate::audio::processes();
    procs.get(&std::process::id()).and_then(|(_, parent)| procs.get(parent)).is_some_and(|(name, _)| name == "streamdeck.exe")
}

/// Talks to Stream Deck until it disconnects.
pub fn run(a: Args, shared: &SharedRef) {
    let Ok(stream) = TcpStream::connect(("127.0.0.1", a.port)) else { return };
    let _ = stream.set_read_timeout(Some(POLL)); // the read timeout is the poll tick
    let Ok((mut ws, _)) = tungstenite::client(format!("ws://127.0.0.1:{}", a.port), stream) else { return };
    send(&mut ws, json!({ "event": a.event, "uuid": a.uuid }));

    let mut keys: HashMap<String, (Value, String)> = HashMap::new(); // visible keys -> settings, image shown
    let mut icons = Icons::default();
    let mut inspector: Option<(String, String)> = None; // (action, context) of the open property inspector
    let mut sent_rev = None; // config revision the open inspector has
    // The keys are the rules: a rule no key points at is deleted (after two checks in a row, so a key just set up
    // in the inspector isn't caught before Stream Deck saves its profile).
    let prune = hosted_by_stream_deck();
    let (mut last_prune, mut unused_before) = (Instant::now(), HashSet::new());
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                let ctx = v["context"].as_str().unwrap_or_default().to_string();
                let action = v["action"].as_str().unwrap_or_default().to_string();
                match v["event"].as_str().unwrap_or_default() {
                    "willAppear" | "didReceiveSettings" => {
                        // Earlier versions titled the key; the image carries everything now.
                        send(&mut ws, json!({ "event": "setTitle", "context": ctx, "payload": { "title": "", "target": 0 } }));
                        keys.insert(ctx, (v["payload"]["settings"].clone(), String::new()));
                    }
                    "willDisappear" => {
                        keys.remove(&ctx);
                    }
                    "keyUp" => {
                        let id = keys.get(&ctx).and_then(|k| k.0["rule"].as_u64()).unwrap_or(0);
                        let mut found = false;
                        set_config(shared, |c| {
                            if let Some(r) = c.rules.iter_mut().find(|r| r.id == id) {
                                r.enabled = !r.enabled;
                                found = true;
                            }
                        });
                        if !found {
                            send(&mut ws, json!({ "event": "showAlert", "context": ctx })); // not set up yet
                        }
                    }
                    "propertyInspectorDidAppear" => {
                        shared.lock().unwrap().inspector_open = true;
                        inspector = Some((action, ctx));
                        sent_rev = None;
                    }
                    "propertyInspectorDidDisappear" => {
                        shared.lock().unwrap().inspector_open = false;
                        inspector = None;
                    }
                    "sendToPlugin" => {
                        // The inspector sends the whole config on every change, or {} when it loads and wants it.
                        if let Ok(config) = serde_json::from_value::<Config>(v["payload"]["config"].clone()) {
                            set_config(shared, |c| *c = config);
                        } else {
                            sent_rev = None;
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
        for (ctx, (settings, shown)) in keys.iter_mut() {
            let now = image(shared, &mut icons, settings);
            if now != *shown {
                send(&mut ws, json!({ "event": "setImage", "context": ctx, "payload": { "image": now, "target": 0 } }));
                *shown = now;
            }
        }
        if prune && last_prune.elapsed() >= PRUNE {
            last_prune = Instant::now();
            if let Some(mut used) = profile_rules() {
                used.extend(keys.values().filter_map(|k| k.0["rule"].as_u64()));
                let unused: HashSet<u64> = shared.lock().unwrap().config.rules.iter().map(|r| r.id).filter(|id| !used.contains(id)).collect();
                if unused.iter().any(|id| unused_before.contains(id)) {
                    set_config(shared, |c| c.rules.retain(|r| !(unused.contains(&r.id) && unused_before.contains(&r.id))));
                }
                unused_before = unused;
            }
        }
        if let Some((action, ctx)) = &inspector {
            let (action, ctx) = (action.clone(), ctx.clone());
            let mut payload = json!({});
            {
                let sh = shared.lock().unwrap();
                payload["live"] = serde_json::to_value(&sh.live).unwrap_or_default();
                if sent_rev != Some(sh.rev) {
                    sent_rev = Some(sh.rev); // config changed (here, by a key, or by the inspector itself): resend it
                    payload["config"] = serde_json::to_value(&sh.config).unwrap_or_default();
                }
            }
            send(&mut ws, json!({ "event": "sendToPropertyInspector", "action": action, "context": ctx, "payload": payload }));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn collect_finds_rule_keys_at_any_depth() {
        let page = serde_json::json!({ "Controllers": [{ "Actions": {
            "0,0": { "UUID": "com.xenside.audioduck.rule", "Settings": { "rule": 3 } },
            "1,0": { "UUID": "com.xenside.audioduck.rule", "Settings": {} },
            "2,0": { "UUID": "com.elgato.discord.mute", "Settings": { "rule": 9 } },
            "3,0": { "UUID": "com.elgato.streamdeck.multiactions", "Settings": {}, "Actions": [[{ "UUID": "com.xenside.audioduck.rule", "Settings": { "rule": 1 } }]] }
        } }] });
        let mut ids = std::collections::HashSet::new();
        super::collect(&page, &mut ids);
        assert_eq!(ids, [1, 3].into());
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(super::base64(i.as_bytes()), o);
        }
    }
}
