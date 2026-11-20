//! Daily khatma reminder: a Windows notification from the tray icon at the time you pick,
//! only on days when today's portion isn't done yet. Clicking it opens the khatma page.
//!
//! Cost: one clock check per minute. user.json is only read once the reminder time has
//! passed, and not again after today's reminder was shown.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicIsize, AtomicU32, Ordering},
    },
    thread,
    time::Duration,
};

use tao::event_loop::EventLoopProxy;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, SystemInformation::GetLocalTime},
    UI::{
        Shell::{
            DefSubclassProc, NIF_INFO, NIIF_LARGE_ICON, NIIF_NONE, NIIF_RESPECT_QUIET_TIME,
            NIIF_USER, NIM_MODIFY, NOTIFYICONDATAW, SetWindowSubclass, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{IMAGE_ICON, LR_DEFAULTSIZE, LoadImageW},
    },
};

use crate::{offline::UserEvent, settings::Settings};

/// tray-icon's WM_USER_TRAYICON (pinned crate version, see Cargo.lock).
const TRAY_CALLBACK_MSG: u32 = 6002;
const NIN_BALLOONUSERCLICK: u32 = 0x0405;

static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static TRAY_UID: AtomicU32 = AtomicU32::new(0);
static PROXY: Mutex<Option<EventLoopProxy<UserEvent>>> = Mutex::new(None);

fn wide_into(dst: &mut [u16], s: &str) {
    let n = dst.len() - 1;
    for (i, c) in s.encode_utf16().take(n).enumerate() {
        dst[i] = c;
    }
}

/// Local date as a day number (days since 1970-01-01) and minutes since midnight.
/// Same numbering as the page: `Date.UTC(y, m - 1, d) / 864e5`.
fn now_local() -> (i64, u32) {
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    let (y, m, d) = (t.wYear as i64, t.wMonth as i64, t.wDay as i64);
    // days_from_civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe - 719468, t.wHour as u32 * 60 + t.wMinute as u32)
}

fn parse_time(s: &str) -> u32 {
    let mut it = s.split(':').map(|x| x.trim().parse::<u32>().unwrap_or(0));
    let h = it.next().unwrap_or(20).min(23);
    let m = it.next().unwrap_or(0).min(59);
    h * 60 + m
}

/// Title and text for today's reminder, or None if there's nothing due.
fn due_message(data_dir: &Path, today: i64) -> Option<(String, String)> {
    let text = fs::read_to_string(data_dir.join("user.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let k = v.get("khatma")?;
    let start = k.get("startDay")?.as_i64()?;
    let days = k.get("days")?.as_i64()?;
    if days <= 0 {
        return None;
    }
    let done = |i: i64| k["done"].get(i.to_string()).is_some_and(|x| !x.is_null());
    let today_idx = (today - start).clamp(0, days - 1);
    let next = (0..days).find(|&i| !done(i))?;
    if next > today_idx {
        return None;
    }
    let owed = (next..=today_idx).filter(|&i| !done(i)).count();
    let label = k["labels"].get(next as usize).and_then(|x| x.as_str()).unwrap_or("");
    let listen = k["mode"].as_str() == Some("listen");
    let title = if owed > 1 {
        format!("وردك النهارده (عليك {owed} أوراد)")
    } else {
        "وردك النهارده".to_string()
    };
    let verb = if listen { "تسمعه" } else { "تقراه" };
    Some((title, format!("{label}\nاضغط هنا علشان {verb}.")))
}

/// Show a notification from the tray icon (Windows turns it into a toast).
/// tray-icon's id for the icon is internal, so try the small ids it hands out: changing a
/// notify icon that doesn't exist just fails and has no other effect.
pub fn notify(title: &str, body: &str) -> bool {
    let hwnd = TRAY_HWND.load(Ordering::Relaxed) as HWND;
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uFlags = NIF_INFO;
        wide_into(&mut nid.szInfoTitle, title);
        wide_into(&mut nid.szInfo, body);
        let icon = LoadImageW(GetModuleHandleW(std::ptr::null()), 1 as _, IMAGE_ICON, 0, 0, LR_DEFAULTSIZE);
        nid.dwInfoFlags = NIIF_RESPECT_QUIET_TIME
            | if icon.is_null() { NIIF_NONE } else { NIIF_USER | NIIF_LARGE_ICON };
        nid.hBalloonIcon = icon as _;
        let known = TRAY_UID.load(Ordering::Relaxed);
        let order = std::iter::once(known).filter(|&u| u != 0).chain(1..=32);
        for uid in order {
            nid.uID = uid;
            if Shell_NotifyIconW(NIM_MODIFY, &nid) != 0 {
                TRAY_UID.store(uid, Ordering::Relaxed);
                return true;
            }
        }
        false
    }
}

/// "Try it" from the settings: show what today's reminder would say.
pub fn test(data_dir: &Path) -> bool {
    let (today, _) = now_local();
    match due_message(data_dir, today) {
        Some((t, b)) => notify(&t, &b),
        None => notify("تذكير الختمة شغّال", "هيظهرلك كده كل يوم في المعاد اللي اخترته، لو لسه ما خلّصتش وردك."),
    }
}

unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if msg == TRAY_CALLBACK_MSG && (lparam as u32 & 0xFFFF) == NIN_BALLOONUSERCLICK {
        if let Some(p) = PROXY.lock().unwrap().as_ref() {
            let _ = p.send_event(UserEvent::Tray("khatma".into()));
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn check_loop(data_dir: PathBuf, settings_path: PathBuf) {
    let mark = data_dir.join("reminded.txt");
    let mut reminded: i64 = fs::read_to_string(&mark).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    loop {
        let (today, minutes) = now_local();
        if reminded != today {
            let st = Settings::load(&settings_path);
            if st.khatma_reminder && minutes >= parse_time(&st.reminder_time) {
                if let Some((title, body)) = due_message(&data_dir, today) {
                    if notify(&title, &body) {
                        reminded = today;
                        let _ = fs::write(&mark, today.to_string());
                    }
                }
            }
        }
        thread::sleep(Duration::from_secs(60));
    }
}

/// Must be called on the thread that owns the tray icon (the main thread).
pub fn start(proxy: EventLoopProxy<UserEvent>, tray_hwnd: HWND, data_dir: PathBuf, settings_path: PathBuf) {
    if tray_hwnd.is_null() {
        return;
    }
    TRAY_HWND.store(tray_hwnd as isize, Ordering::Relaxed);
    *PROXY.lock().unwrap() = Some(proxy);
    unsafe {
        SetWindowSubclass(tray_hwnd, Some(subclass_proc), 0x5155_524E, 0);
    }
    thread::spawn(move || check_loop(data_dir, settings_path));
}
