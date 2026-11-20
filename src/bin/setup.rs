//! Quranify-Setup.exe: installs the app for the current user (no admin rights needed).
//!
//! - copies Quranify.exe to %LOCALAPPDATA%\Programs\Quranify
//! - adds Start Menu and Desktop shortcuts
//! - registers it in "Installed apps" so it can be removed from Windows Settings
//!
//! The app itself is embedded at build time from dist/Quranify.exe (see build-release.ps1).
#![windows_subsystem = "windows"]

#[path = "../shortcuts.rs"]
mod shortcuts;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    ptr::null_mut,
    thread,
    time::Duration,
};

use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent},
    UI::WindowsAndMessaging::{
        IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_RIGHT, MB_RTLREADING,
        MB_YESNO, MessageBoxW,
    },
};
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

const APP: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/Quranify.exe"));
const VERSION: &str = env!("CARGO_PKG_VERSION");
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Quranify";
const QUIT_EVENT: &str = "Local\\QuranifyDesktop.Quit";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn message(text: &str, flags: u32) -> i32 {
    let (t, c) = (wide(text), wide("تثبيت Quranify"));
    unsafe { MessageBoxW(null_mut(), t.as_ptr(), c.as_ptr(), flags | MB_RTLREADING | MB_RIGHT) }
}

/// Close a running copy cleanly and wait for it to exit.
fn close_running_app() {
    unsafe {
        let name = wide(QUIT_EVENT);
        let ev = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if ev.is_null() {
            return;
        }
        SetEvent(ev);
        CloseHandle(ev);
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(200));
            let still = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
            if still.is_null() {
                break;
            }
            CloseHandle(still);
        }
    }
    thread::sleep(Duration::from_millis(500));
}

fn register(exe: &Path, dir: &Path) -> std::io::Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(UNINSTALL_KEY)?;
    let exe_s = exe.display().to_string();
    key.set_value("DisplayName", &"Quranify")?;
    key.set_value("DisplayVersion", &VERSION)?;
    key.set_value("Publisher", &"Quranify Desktop (personal build)")?;
    key.set_value("DisplayIcon", &format!("{exe_s},0"))?;
    key.set_value("InstallLocation", &dir.display().to_string())?;
    key.set_value("UninstallString", &format!("\"{exe_s}\" --uninstall"))?;
    key.set_value("QuietUninstallString", &format!("\"{exe_s}\" --uninstall --quiet"))?;
    key.set_value("EstimatedSize", &((APP.len() / 1024) as u32))?;
    key.set_value("NoModify", &1u32)?;
    key.set_value("NoRepair", &1u32)?;
    Ok(())
}

fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join("Quranify")
}

fn main() {
    let quiet = std::env::args().any(|a| a == "--quiet");
    let dir = install_dir();
    let exe = dir.join("Quranify.exe");
    let update = exe.exists();

    if !quiet {
        let q = if update {
            format!("Quranify متثبّت على الجهاز. تحدّثه للإصدار {VERSION}؟\n\nتحميلاتك وإعداداتك وتسجيل الدخول هيفضلوا زي ما هم.")
        } else {
            format!("تثبيت Quranify {VERSION} على الجهاز؟\n\nهيتحط في قائمة ابدأ وعلى سطح المكتب، ومش محتاج صلاحيات أدمن.")
        };
        if message(&q, MB_YESNO | MB_ICONQUESTION) != IDYES {
            return;
        }
    }

    close_running_app();

    let result = (|| -> std::io::Result<()> {
        fs::create_dir_all(&dir)?;
        // Write next to the target first so a failed copy never leaves a broken app.
        let tmp = dir.join("Quranify.exe.new");
        fs::write(&tmp, APP)?;
        let mut tries = 0;
        loop {
            match fs::rename(&tmp, &exe) {
                Ok(()) => break,
                Err(e) if tries < 20 => {
                    let _ = e;
                    tries += 1;
                    thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(e),
            }
        }
        register(&exe, &dir)?;
        Ok(())
    })();

    if let Err(e) = result {
        message(&format!("تعذّر التثبيت:\n{e}"), MB_OK | MB_ICONERROR);
        return;
    }
    shortcuts::create(&exe, &dir);

    let open = quiet
        || message(
            "تم التثبيت ✓\n\nتلاقي Quranify في قائمة ابدأ وعلى سطح المكتب. تفتحه دلوقتي؟",
            MB_YESNO | MB_ICONINFORMATION,
        ) == IDYES;
    if open {
        let _ = Command::new(&exe).current_dir(&dir).spawn();
    }
}
