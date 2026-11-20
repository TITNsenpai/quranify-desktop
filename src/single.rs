//! One running copy only: a second launch asks the first one to show its window.

use std::{ptr::null, thread};

use tao::event_loop::EventLoopProxy;
use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
    System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
        WaitForSingleObject,
    },
    UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow},
};

use crate::offline::{UserEvent, post};

const MUTEX_NAME: &str = "Local\\QuranifyDesktop.Instance";
const SHOW_EVENT: &str = "Local\\QuranifyDesktop.Show";
const QUIT_EVENT: &str = "Local\\QuranifyDesktop.Quit";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Returns `true` if this is the only running copy. Otherwise it signals the running
/// copy to show itself (unless `quiet`) and returns `false`.
pub fn acquire(quiet: bool) -> bool {
    unsafe {
        let name = wide(MUTEX_NAME);
        let mutex = CreateMutexW(null(), 0, name.as_ptr());
        if GetLastError() != ERROR_ALREADY_EXISTS {
            // Keep the mutex for the life of the process.
            let _ = mutex;
            return true;
        }
        if !quiet {
            // Let the running copy take keyboard focus.
            AllowSetForegroundWindow(ASFW_ANY);
            let ev_name = wide(SHOW_EVENT);
            let ev = OpenEventW(EVENT_MODIFY_STATE, 0, ev_name.as_ptr());
            if !ev.is_null() {
                SetEvent(ev);
            }
        }
        false
    }
}

/// Ask a running copy to close cleanly (`Quranify.exe --quit`). Returns true if one was running.
pub fn request_quit() -> bool {
    unsafe {
        let name = wide(QUIT_EVENT);
        let ev = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if ev.is_null() {
            return false;
        }
        SetEvent(ev);
        true
    }
}

/// Wait for "show yourself" and "quit" requests from later launches.
pub fn listen(proxy: EventLoopProxy<UserEvent>) {
    for (name, quit) in [(SHOW_EVENT, false), (QUIT_EVENT, true)] {
        let ev = unsafe {
            let n = wide(name);
            CreateEventW(null(), 0, 0, n.as_ptr())
        };
        if ev.is_null() {
            continue;
        }
        let handle = ev as usize;
        let proxy = proxy.clone();
        thread::spawn(move || {
            loop {
                unsafe { WaitForSingleObject(handle as _, INFINITE) };
                let event = if quit { UserEvent::Tray("quit".into()) } else { UserEvent::ShowMain };
                post(&proxy, event);
            }
        });
    }
}
