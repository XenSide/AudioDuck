//! Control for the Stream Deck plugin. In-process via `handle`; from a second AudioDuck (launched by Stream Deck while
//! another one already runs) via a message-only window on the engine thread: `MSG` with the command in wparam, state in reply.

use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering::SeqCst};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const CLASS: PCWSTR = w!("AudioDuck.Control");
const MSG: u32 = WM_APP + 0x4d;

// Commands (wparam).
pub const QUERY: usize = 0;
pub const TOGGLE: usize = 1;
pub const OPEN: usize = 2;
pub const QUIT: usize = 3;

// Replies. 0 is never sent: it's what a failed or timed-out SendMessage gives back.
pub const PAUSED: isize = 1;
pub const ACTIVE: isize = 2;
pub const DUCKING: isize = 3;

/// Set by the engine loop.
pub static STATE: AtomicIsize = AtomicIsize::new(ACTIVE);
/// Last command received, consumed by the engine loop.
pub static REQUEST: AtomicUsize = AtomicUsize::new(QUERY);

/// Queues a command for the engine loop and returns the state.
pub fn handle(cmd: usize) -> isize {
    if matches!(cmd, TOGGLE | OPEN | QUIT) {
        REQUEST.store(cmd, SeqCst);
    }
    STATE.load(SeqCst)
}

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if m == MSG { LRESULT(handle(if w.0 == QUIT { QUERY } else { w.0 })) } else { unsafe { DefWindowProcW(h, m, w, l) } }
}

/// Call on a thread that pumps messages; the window lives as long as that thread.
pub fn create_window() {
    unsafe {
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), lpszClassName: CLASS, ..Default::default() };
        RegisterClassW(&wc);
        let _ = CreateWindowExW(Default::default(), CLASS, PCWSTR::null(), Default::default(), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None);
    }
}

/// From another process. 0 if AudioDuck isn't running or doesn't answer in time.
pub fn send(cmd: usize) -> isize {
    send_opt(cmd).unwrap_or(0)
}

fn send_opt(cmd: usize) -> Option<isize> {
    unsafe {
        let h = FindWindowExW(Some(HWND_MESSAGE), None, CLASS, PCWSTR::null()).ok()?;
        let mut reply = 0usize;
        // Timeout, never a plain SendMessage: the engine thread can sit in a slow COM call.
        let ok = SendMessageTimeoutW(h, MSG, WPARAM(cmd), LPARAM(0), SMTO_ABORTIFHUNG, 300, Some(&mut reply));
        (ok.0 != 0 && reply != 0).then_some(reply as isize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_replies_without_queueing_and_commands_queue() {
        STATE.store(DUCKING, SeqCst);
        assert_eq!(handle(QUERY), DUCKING);
        assert_eq!(REQUEST.load(SeqCst), QUERY);
        assert_eq!(handle(TOGGLE), DUCKING);
        assert_eq!(REQUEST.swap(QUERY, SeqCst), TOGGLE);
        handle(99); // unknown: ignored
        assert_eq!(REQUEST.load(SeqCst), QUERY);
    }
}
