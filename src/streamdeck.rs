//! Stream Deck (and OpenDeck) plugin: one key that pauses/resumes AudioDuck and shows its state.
//! Tap: toggle ducking. Hold: open the AudioDuck window. AudioDuck not running: tap starts it.
#![windows_subsystem = "windows"]
#[path = "control.rs"]
mod control;
#[path = "icon.rs"]
mod icon;

use std::collections::HashMap;
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

const POLL: Duration = Duration::from_millis(250);
const HOLD: Duration = Duration::from_millis(500);
const OFFLINE: [u8; 3] = [45, 48, 58];

type Ws = WebSocket<TcpStream>;

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

/// Key image and title for a state (0 = AudioDuck not running).
fn look(state: isize) -> (String, &'static str) {
    let (color, title) = match state {
        control::PAUSED => (icon::OFF, "Pausa"),
        control::ACTIVE => (icon::ACCENT, "Attivo"),
        control::DUCKING => (icon::DUCKING, "Abbassa"),
        _ => (OFFLINE, "Spento"),
    };
    (format!("data:image/png;base64,{}", base64(&icon::png(color, 144))), title)
}

fn send(ws: &mut Ws, v: Value) {
    let _ = ws.send(Message::text(v.to_string()));
}

fn show(ws: &mut Ws, ctx: &str, state: isize) {
    let (image, title) = look(state);
    send(ws, json!({ "event": "setImage", "context": ctx, "payload": { "image": image, "target": 0 } }));
    send(ws, json!({ "event": "setTitle", "context": ctx, "payload": { "title": title, "target": 0 } }));
}

/// AudioDuck writes its path at every start.
fn launch() -> bool {
    let Some(appdata) = std::env::var_os("APPDATA") else { return false };
    let Ok(exe) = std::fs::read_to_string(PathBuf::from(appdata).join(r"AudioDuck\exe_path")) else { return false };
    use std::os::windows::process::CommandExt;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let cmd = || {
        let mut c = std::process::Command::new(exe.trim());
        c.arg("--tray");
        c
    };
    // Break out of the host's job object so AudioDuck (and its volume restore on exit) outlives Stream Deck.
    cmd().creation_flags(CREATE_BREAKAWAY_FROM_JOB).spawn().or_else(|_| cmd().spawn()).is_ok()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
    let Ok(port) = arg("-port").parse::<u16>() else { return };
    let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) else { return };
    let _ = stream.set_read_timeout(Some(POLL)); // the read timeout is the poll tick
    let Ok((mut ws, _)) = tungstenite::client(format!("ws://127.0.0.1:{port}"), stream) else { return };
    send(&mut ws, json!({ "event": arg("-registerEvent"), "uuid": arg("-pluginUUID") }));

    let mut keys: HashMap<String, Option<Instant>> = HashMap::new(); // visible keys -> when pressed
    let mut state = control::send(control::QUERY).unwrap_or(0);
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                let ctx = v["context"].as_str().unwrap_or_default().to_string();
                match v["event"].as_str().unwrap_or_default() {
                    "willAppear" => {
                        show(&mut ws, &ctx, state);
                        keys.insert(ctx, None);
                    }
                    "willDisappear" => {
                        keys.remove(&ctx);
                    }
                    "keyDown" => {
                        keys.insert(ctx, Some(Instant::now()));
                    }
                    "keyUp" => {
                        let held = keys.get_mut(&ctx).and_then(Option::take).is_some_and(|t| t.elapsed() >= HOLD);
                        let ok = match control::send(if held { control::OPEN } else { control::TOGGLE }) {
                            Some(_) => true,
                            None => launch(),
                        };
                        if !ok {
                            send(&mut ws, json!({ "event": "showAlert", "context": ctx }));
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
        let now = control::send(control::QUERY).unwrap_or(0);
        if now != state {
            state = now;
            for ctx in keys.keys().cloned().collect::<Vec<_>>() {
                show(&mut ws, &ctx, state);
            }
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
