//! `Quranify.exe --uninstall` (run from Windows Settings → Installed apps).

use std::{
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    ptr::null_mut,
    thread,
    time::Duration,
};

use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_RIGHT,
    MB_RTLREADING, MB_YESNO, MessageBoxW,
};
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

use crate::settings::{self, Settings};

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Quranify";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn message(text: &str, flags: u32) -> i32 {
    let (t, c) = (wide(text), wide("إزالة Quranify"));
    unsafe { MessageBoxW(null_mut(), t.as_ptr(), c.as_ptr(), flags | MB_RTLREADING | MB_RIGHT) }
}

fn installed_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join("Quranify")
}

/// Delete a folder after this process has exited (it may contain our own exe).
fn delete_later(dir: &Path) {
    // raw_arg: cmd.exe needs the quotes exactly as written (Rust would escape them).
    let cmd = format!("/C ping 127.0.0.1 -n 3 > nul & rmdir /s /q \"{}\"", dir.display());
    let _ = Command::new("cmd").raw_arg(cmd).creation_flags(CREATE_NO_WINDOW).spawn();
}

pub fn run(data_dir: &Path) {
    let quiet = std::env::args().any(|a| a == "--quiet");
    if !quiet
        && message("إزالة Quranify من الجهاز؟", MB_YESNO | MB_ICONQUESTION) != IDYES
    {
        return;
    }

    // Close a running copy first.
    if crate::single::request_quit() {
        thread::sleep(Duration::from_secs(3));
    }

    let wipe_data = !quiet
        && message(
            "تمسح كمان السور المحمّلة ونص المصحف والإعدادات وتسجيل الدخول؟\n\n\
             اختار \"لا\" لو هتثبّته تاني وعايز تحتفظ بيهم.",
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES;

    let _ = settings::set_autostart(false);
    let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(UNINSTALL_KEY);
    crate::shortcuts::remove();

    if wipe_data {
        // A custom download folder is always our own "Quranify Offline" folder.
        if let Some(custom) = Settings::load(&data_dir.join("settings.json")).download_dir {
            let p = PathBuf::from(custom);
            if p.ends_with("Quranify Offline") {
                let _ = fs::remove_dir_all(&p);
            }
        }
        // WebView2 may still be releasing files for a moment.
        for _ in 0..10 {
            if fs::remove_dir_all(data_dir).is_ok() || !data_dir.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    // Only ever delete the installed copy's folder, never a folder we were run from elsewhere.
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    if exe_dir.as_deref() == Some(installed_dir().as_path()) {
        delete_later(&installed_dir());
    }

    if !quiet {
        message("اتشال Quranify من الجهاز.", MB_OK | MB_ICONINFORMATION);
    }
}
