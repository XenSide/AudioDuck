//! Windows audio sessions on all active output devices: measure each app's real output, set per-app volume, restore.
//! All COM objects live on the engine thread (MTA).
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, IAgileObject, IAgileObject_Impl};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows::Win32::System::Threading::CreateEventW;
use windows::core::{GUID, IUnknown, Interface, PCWSTR, Ref, Result, implement};

/// Session peak meters can lie (Discord's mirrors the whole device mix), so each app's level comes from
/// process loopback: a capture of only that process tree's output (Windows 10 2004+).
pub struct ProcessMeter {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Activated(mpsc::SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _: Ref<IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}
impl IAgileObject_Impl for Activated_Impl {}

#[repr(C)]
struct PropVariantBlob {
    vt: u16,
    reserved: [u16; 3],
    size: u32,
    data: *const u8,
}

impl ProcessMeter {
    pub fn new(pid: u32) -> Result<Self> {
        unsafe {
            let params = AUDIOCLIENT_ACTIVATION_PARAMS {
                ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
                Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                    ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                        TargetProcessId: pid,
                        ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                    },
                },
            };
            let pv = PropVariantBlob {
                vt: 65, // VT_BLOB
                reserved: [0; 3],
                size: size_of_val(&params) as u32,
                data: &params as *const _ as *const u8,
            };
            let (tx, rx) = mpsc::sync_channel(1);
            let handler: IActivateAudioInterfaceCompletionHandler = Activated(tx).into();
            let op = ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(&pv as *const _ as *const _),
                &handler,
            )?;
            rx.recv_timeout(Duration::from_secs(2)).map_err(|_| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
            let (mut hr, mut unk) = (windows::core::HRESULT(0), None::<IUnknown>);
            op.GetActivateResult(&mut hr, &mut unk)?;
            hr.ok()?;
            let client: IAudioClient = unk.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))?.cast()?;
            // Process loopback has no mix format; ask for s16 stereo at the usual mix rate (48k) so Windows needn't resample.
            let fmt = WAVEFORMATEX { wFormatTag: 1, nChannels: 2, nSamplesPerSec: 48000, nAvgBytesPerSec: 48000 * 4, nBlockAlign: 4, wBitsPerSample: 16, cbSize: 0 };
            let flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM;
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &fmt, None)?;
            let event = CreateEventW(None, false, false, PCWSTR::null())?;
            client.SetEventHandle(event)?; // required in this mode even though we poll
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok(Self { client, capture, event })
        }
    }

    /// Peak (0..1) of everything captured since the last call.
    pub fn peak(&self) -> Result<f32> {
        let mut peak = 0i32;
        unsafe {
            while self.capture.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                if frames > 0 && flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 == 0 {
                    let samples = std::slice::from_raw_parts(data as *const i16, frames as usize * 2);
                    peak = samples.iter().fold(peak, |m, &s| m.max((s as i32).abs()));
                }
                self.capture.ReleaseBuffer(frames)?;
            }
        }
        Ok((peak as f32 / 32768.0).min(1.0))
    }
}

impl Drop for ProcessMeter {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Event context on our volume writes, so session-event listeners can tell AudioDuck from other apps.
const CTX: GUID = GUID::from_u128(0x41d1_0d0c_7a3e_4b5f_9c2d_a0d1_0c4e_5f60);

struct Session {
    name: String,
    sid: String, // GetSessionIdentifier: app + device, survives the session being recreated
    root: u32,
    vol: ISimpleAudioVolume,
    meter: IAudioMeterInformation,
    base: f32,             // user's own volume, restored when ducking ends
    last_set: Option<f32>, // what we last wrote; None = we're not touching this session
}

pub struct Mixer {
    sessions: HashMap<String, Session>,
    // root pid -> meter; None = loopback failed, use the session meter; absent = app not measured (reads as 0)
    loopbacks: HashMap<u32, Option<ProcessMeter>>,
    state_path: PathBuf,
    // Windows remembers per-app volume, so sessions that vanish while ducked (or all of them, if we died) get the
    // user's volume back when they reappear. Keyed by session identifier.
    pending_restore: HashMap<String, f32>,
    saved: HashMap<String, f32>,
}

/// pid -> (lowercase exe name, parent pid)
fn processes() -> HashMap<u32, (String, u32)> {
    let mut out = HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.insert(e.th32ProcessID, (String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase(), e.th32ParentProcessID));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Topmost ancestor with the same exe name, so the loopback covers the whole app (renderers, utilities).
fn root_pid(procs: &HashMap<u32, (String, u32)>, pid: u32, name: &str) -> u32 {
    let mut cur = pid;
    for _ in 0..32 {
        match procs.get(&cur).and_then(|(_, ppid)| procs.get(ppid).map(|(n, _)| (n, *ppid))) {
            Some((n, ppid)) if n == name && ppid != cur => cur = ppid,
            _ => break,
        }
    }
    cur
}

impl Mixer {
    pub fn new(state_path: PathBuf) -> Self {
        let pending: HashMap<String, f32> = std::fs::read_to_string(&state_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Self { sessions: HashMap::new(), loopbacks: HashMap::new(), state_path, saved: pending.clone(), pending_restore: pending }
    }

    /// `measure(app)`: whether that app needs an accurate level (each loopback costs a capture pipeline).
    pub fn refresh(&mut self, measure: impl Fn(&str) -> bool) {
        let mut procs = None; // process snapshot, only taken if a new session shows up
        let me = std::process::id();
        let mut found = HashMap::new();
        let mut restored = HashSet::new();
        unsafe {
            let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return };
            let Ok(devices) = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return };
            for i in 0..devices.GetCount().unwrap_or(0) {
                let Ok(sessions) = devices.Item(i).and_then(|d| d.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)).and_then(|m| m.GetSessionEnumerator()) else { continue };
                for j in 0..sessions.GetCount().unwrap_or(0) {
                    let Ok(ctl) = sessions.GetSession(j).and_then(|c| c.cast::<IAudioSessionControl2>()) else { continue };
                    let pid = ctl.GetProcessId().unwrap_or(0);
                    if pid == 0 || pid == me || ctl.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                        continue; // pid 0 = system sounds
                    }
                    let Ok(id) = ctl.GetSessionInstanceIdentifier() else { continue };
                    let key = id.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(id.0 as _));
                    if let Some(s) = self.sessions.remove(&key) {
                        found.insert(key, s);
                        continue;
                    }
                    let procs = procs.get_or_insert_with(processes);
                    let Some((name, _)) = procs.get(&pid) else { continue };
                    let (Ok(vol), Ok(meter)) = (ctl.cast::<ISimpleAudioVolume>(), ctl.cast::<IAudioMeterInformation>()) else { continue };
                    let Ok(sid) = ctl.GetSessionIdentifier() else { continue };
                    let sid_str = sid.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(sid.0 as _));
                    // A session recreated while we had it ducked starts at the ducked volume (Windows persists it):
                    // put the user's volume back first, or the ducked level would become the new baseline.
                    if let Some(v) = self.pending_restore.remove(&sid_str) {
                        let _ = vol.SetMasterVolume(v, &CTX);
                    } else if let Some(&v) = self.pending_restore.get(name) {
                        let _ = vol.SetMasterVolume(v, &CTX); // old state files were keyed by app name
                        restored.insert(name.clone()); // removed after the loop: the app may have a session per device
                    }
                    let root = root_pid(procs, pid, name);
                    found.insert(key, Session { name: name.clone(), sid: sid_str, root, vol, meter, base: 1.0, last_set: None });
                }
            }
        }
        self.pending_restore.retain(|name, _| !restored.contains(name));
        for s in self.sessions.values_mut() {
            if s.last_set.is_some() {
                self.pending_restore.insert(s.sid.clone(), s.base); // in case it comes back with the ducked volume
            }
            restore(s); // best effort: the session is usually gone already
        }
        self.sessions = found;

        let roots: HashSet<u32> = self.sessions.values().filter(|s| measure(&s.name)).map(|s| s.root).collect();
        self.loopbacks.retain(|r, _| roots.contains(r));
        for r in roots {
            self.loopbacks.entry(r).or_insert_with(|| ProcessMeter::new(r).ok());
        }
    }

    pub fn peaks(&self) -> HashMap<String, f32> {
        let mut out: HashMap<String, f32> = HashMap::new();
        let mut read = HashSet::new();
        for s in self.sessions.values() {
            let peak = match self.loopbacks.get(&s.root) {
                Some(Some(m)) if read.insert(s.root) => m.peak().unwrap_or(0.0),
                Some(Some(_)) => 0.0, // already drained this app's loopback this call
                Some(None) => unsafe { s.meter.GetPeakValue().unwrap_or(0.0) },
                None => 0.0,
            };
            let e = out.entry(s.name.clone()).or_default();
            *e = e.max(peak);
        }
        out
    }

    pub fn apply(&mut self, gains: &HashMap<String, f32>) {
        for s in self.sessions.values_mut() {
            let g = gains.get(&s.name).copied().unwrap_or(1.0);
            if s.last_set.is_none() && g >= 1.0 {
                continue; // not ducked and not about to be: no COM calls
            }
            // The user's volume is read once, when ducking starts. Never re-derive it from the current volume while
            // ducked: other apps write it too (Discord's attenuation), and treating their writes as the user's
            // made the baseline ratchet down a bit more on every duck.
            if s.last_set.is_none() {
                let Ok(cur) = (unsafe { s.vol.GetMasterVolume() }) else { continue };
                s.base = cur;
            }
            if g >= 1.0 {
                restore(s);
                continue;
            }
            let v = s.base * g;
            if s.last_set.is_none_or(|last| (v - last).abs() > 0.001) && unsafe { s.vol.SetMasterVolume(v, &CTX) }.is_ok() {
                s.last_set = Some(v);
            }
        }
        self.save_state();
    }

    pub fn restore_all(&mut self) {
        self.sessions.values_mut().for_each(restore);
        self.save_state();
    }

    /// User-facing volume of each app (its pre-duck level if ducked).
    pub fn volumes(&self) -> HashMap<String, f32> {
        self.sessions
            .values()
            .filter_map(|s| Some((s.name.clone(), if s.last_set.is_some() { s.base } else { unsafe { s.vol.GetMasterVolume() }.ok()? })))
            .collect()
    }

    fn save_state(&mut self) {
        let mut ducked: HashMap<String, f32> = self.sessions.values().filter(|s| s.last_set.is_some()).map(|s| (s.sid.clone(), s.base)).collect();
        ducked.extend(self.pending_restore.iter().map(|(k, v)| (k.clone(), *v)));
        if ducked != self.saved {
            let _ = std::fs::write(&self.state_path, serde_json::to_string(&ducked).unwrap_or_default());
            self.saved = ducked;
        }
    }
}

fn restore(s: &mut Session) {
    if s.last_set.is_some() {
        let _ = unsafe { s.vol.SetMasterVolume(s.base, &CTX) };
    }
    s.last_set = None;
}
