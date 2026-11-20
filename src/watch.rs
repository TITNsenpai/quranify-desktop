//! Watches the system so playback can pause by itself:
//! - the PC is locked (Win+L) or goes to sleep
//! - the audio output changes (e.g. headphones unplugged → it would play from the speakers)
//! - another app starts using the microphone (a call or meeting)
//!
//! Lock/sleep arrive as window messages (no polling). The device and microphone checks run
//! every 2 seconds, and only while something is playing or was paused by us.

use std::{
    ptr::null,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use tao::event_loop::EventLoopProxy;
use windows::Win32::{
    Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender},
    System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::{
        LibraryLoader::GetModuleHandleW,
        RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification},
    },
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PBT_APMSUSPEND,
        RegisterClassW, TranslateMessage, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WNDCLASSW,
        WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    },
};
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

use crate::offline::UserEvent;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sys {
    Lock,
    Unlock,
    Suspend,
    OutputChanged,
    CallStarted,
    CallEnded,
}

/// Set by the event loop: is something playing, or did we pause it and may resume?
pub static ACTIVE: AtomicBool = AtomicBool::new(false);

static PROXY: Mutex<Option<EventLoopProxy<UserEvent>>> = Mutex::new(None);

fn send(s: Sys) {
    if let Some(p) = PROXY.lock().unwrap().as_ref() {
        let _ = p.send_event(UserEvent::System(s));
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_WTSSESSION_CHANGE => match wparam as u32 {
            WTS_SESSION_LOCK => send(Sys::Lock),
            WTS_SESSION_UNLOCK => send(Sys::Unlock),
            _ => {}
        },
        WM_POWERBROADCAST if wparam as u32 == PBT_APMSUSPEND => send(Sys::Suspend),
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// A hidden window that receives lock/unlock and sleep notifications.
fn session_window() {
    unsafe {
        let class = wide("QuranifyWatch");
        let hinst = GetModuleHandleW(null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let title = wide("");
        // A normal (never shown) top-level window: message-only windows miss broadcasts.
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), hinst, null());
        if hwnd.is_null() {
            return;
        }
        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Id of the current default audio output, e.g. headphones or speakers.
fn default_output(enumerator: &IMMDeviceEnumerator) -> Option<String> {
    unsafe {
        let dev = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let id = dev.GetId().ok()?;
        let s = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as _));
        s
    }
}

/// Is another app using the microphone right now? Windows records this for its privacy
/// indicator: an app whose `LastUsedTimeStop` is 0 is using it at this moment.
fn mic_in_use_by_others() -> bool {
    const BASE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(root) = hkcu.open_subkey(BASE) else { return false };
    let in_use = |k: &RegKey| -> bool {
        let start: u64 = k.get_value("LastUsedTimeStart").unwrap_or(0);
        let stop: u64 = k.get_value("LastUsedTimeStop").unwrap_or(1);
        start > 0 && stop == 0
    };
    let mine = |name: &str| {
        let n = name.to_ascii_lowercase();
        n.contains("quranify") || n.contains("msedgewebview2")
    };
    for name in root.enum_keys().flatten() {
        if name == "NonPackaged" {
            if let Ok(np) = root.open_subkey(&name) {
                for app in np.enum_keys().flatten() {
                    if !mine(&app) && np.open_subkey(&app).is_ok_and(|k| in_use(&k)) {
                        return true;
                    }
                }
            }
        } else if !mine(&name) && root.open_subkey(&name).is_ok_and(|k| in_use(&k)) {
            return true;
        }
    }
    false
}

fn poll_loop() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let enumerator: Option<IMMDeviceEnumerator> =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() };
    let mut last_output = enumerator.as_ref().and_then(default_output);
    let mut in_call = false;
    loop {
        thread::sleep(Duration::from_secs(2));
        if !ACTIVE.load(Ordering::Relaxed) {
            // Nothing to protect: keep the baseline fresh and do no other work.
            if let Some(e) = &enumerator {
                last_output = default_output(e);
            }
            in_call = false;
            continue;
        }
        if let Some(e) = &enumerator {
            let now = default_output(e);
            if now.is_some() && now != last_output {
                last_output = now;
                send(Sys::OutputChanged);
            }
        }
        let call = mic_in_use_by_others();
        if call != in_call {
            in_call = call;
            send(if call { Sys::CallStarted } else { Sys::CallEnded });
        }
    }
}

/// `Quranify.exe --diag`: what the auto-pause checks see right now.
pub fn diagnostics() -> String {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let enumerator: Option<IMMDeviceEnumerator> =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() };
    format!(
        "default audio output: {}
another app using the microphone: {}
",
        enumerator.as_ref().and_then(default_output).unwrap_or_else(|| "(unknown)".into()),
        mic_in_use_by_others()
    )
}

pub fn start(proxy: EventLoopProxy<UserEvent>) {
    *PROXY.lock().unwrap() = Some(proxy);
    thread::spawn(session_window);
    thread::spawn(poll_loop);
}
