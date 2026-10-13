//! Stream Deck (and OpenDeck) side. Two key actions:
//! - toggle: pauses/resumes all ducking; its property inspector edits the whole config and shows live levels.
//! - rule: enables/disables one rule, picked in its property inspector (settings `{"rule": id}`).
//! The key image shows the state either way.
use std::collections::HashMap;
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use crate::{Config, SharedRef, icon, save_config};

const POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, PartialEq)]
enum State {
    Paused,
    Active,
    Ducking,
}

/// What a visible key controls: None = all ducking, Some(id) = one rule (0 = none picked yet).
type Key = Option<u64>;

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

fn base64(data: &[u8]) -> String {
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

fn show(ws: &mut Ws, ctx: &str, (state, title): &(State, String)) {
    let color = match state {
        State::Paused => icon::OFF,
        State::Active => icon::ACCENT,
        State::Ducking => icon::DUCKING,
    };
    let image = format!("data:image/png;base64,{}", base64(&icon::png(color, 144)));
    send(ws, json!({ "event": "setImage", "context": ctx, "payload": { "image": image, "target": 0 } }));
    send(ws, json!({ "event": "setTitle", "context": ctx, "payload": { "title": title, "target": 0 } }));
}

fn pretty(app: &str) -> String {
    let s = app.strip_suffix(".exe").unwrap_or(app);
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// State and title of a key.
fn look(shared: &SharedRef, key: Key) -> (State, String) {
    let sh = shared.lock().unwrap();
    let Some(id) = key else {
        return match (sh.config.enabled, sh.live.ducking) {
            (false, _) => (State::Paused, "Pausa".into()),
            (true, true) => (State::Ducking, "Abbassa".into()),
            (true, false) => (State::Active, "Attivo".into()),
        };
    };
    let Some(rule) = sh.config.rules.iter().find(|r| r.id == id) else { return (State::Paused, "Regola?".into()) };
    let title = rule.sources.iter().map(|a| pretty(a)).collect::<Vec<_>>().join("\n"); // the rule's trigger apps
    let state = if !sh.config.enabled || !rule.enabled {
        State::Paused
    } else if sh.live.active_rules.contains(&id) {
        State::Ducking
    } else {
        State::Active
    };
    (state, title)
}

fn set_config(shared: &SharedRef, change: impl FnOnce(&mut Config)) {
    let mut sh = shared.lock().unwrap();
    change(&mut sh.config);
    sh.config.ensure_ids(); // rules new from the inspector have no id yet
    sh.rev += 1;
    save_config(&sh.config);
}

/// Talks to Stream Deck until it disconnects.
pub fn run(a: Args, shared: &SharedRef) {
    let Ok(stream) = TcpStream::connect(("127.0.0.1", a.port)) else { return };
    let _ = stream.set_read_timeout(Some(POLL)); // the read timeout is the poll tick
    let Ok((mut ws, _)) = tungstenite::client(format!("ws://127.0.0.1:{}", a.port), stream) else { return };
    send(&mut ws, json!({ "event": a.event, "uuid": a.uuid }));

    let mut keys: HashMap<String, (Key, (State, String))> = HashMap::new(); // visible keys -> what they control, what they show
    let mut inspector: Option<(String, String)> = None; // (action, context) of the open property inspector
    let mut sent_rev = None; // config revision the open inspector has
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                let ctx = v["context"].as_str().unwrap_or_default().to_string();
                let action = v["action"].as_str().unwrap_or_default().to_string();
                match v["event"].as_str().unwrap_or_default() {
                    "willAppear" | "didReceiveSettings" => {
                        let key = action.ends_with(".rule").then(|| v["payload"]["settings"]["rule"].as_u64().unwrap_or(0));
                        let shown = look(shared, key);
                        show(&mut ws, &ctx, &shown);
                        keys.insert(ctx, (key, shown));
                    }
                    "willDisappear" => {
                        keys.remove(&ctx);
                    }
                    "keyUp" => match keys.get(&ctx).map(|k| k.0) {
                        Some(None) => set_config(shared, |c| c.enabled = !c.enabled),
                        Some(Some(id)) => {
                            let mut found = false;
                            set_config(shared, |c| {
                                if let Some(r) = c.rules.iter_mut().find(|r| r.id == id) {
                                    r.enabled = !r.enabled;
                                    found = true;
                                }
                            });
                            if !found {
                                send(&mut ws, json!({ "event": "showAlert", "context": ctx }));
                            }
                        }
                        None => {}
                    },
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
        for (ctx, (key, shown)) in keys.iter_mut() {
            let now = look(shared, *key);
            if now != *shown {
                show(&mut ws, ctx, &now);
                *shown = now;
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
    fn base64_matches_rfc4648() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(super::base64(i.as_bytes()), o);
        }
    }
}
