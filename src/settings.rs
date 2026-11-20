//! App settings (settings.json) and "start with Windows" (HKCU Run key).

use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Quranify";
/// Command-line flag added to the Run entry so we know we were started with Windows.
pub const AUTOSTART_ARG: &str = "--autostart";

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// When started with Windows, open minimized to the taskbar.
    pub start_minimized: bool,
    /// "site" or "offline": which screen opens first.
    pub start_page: String,
    /// GPU acceleration (uses more memory; takes effect after restart).
    pub gpu: bool,
    /// Custom download folder; `None` means the default under %LOCALAPPDATA%.
    pub download_dir: Option<String>,
    /// Closing the window keeps the app (and audio) running in the tray.
    pub close_to_tray: bool,
    /// Global keyboard shortcuts (Ctrl+Alt+Space etc.).
    pub hotkeys: bool,
    /// Pause when the PC is locked (Win+L).
    pub pause_on_lock: bool,
    /// Pause when the audio output changes (headphones unplugged).
    pub pause_on_output: bool,
    /// Pause while another app uses the microphone (calls, meetings).
    pub pause_on_call: bool,
    /// Resume after unlocking / after the call, if we paused it.
    pub auto_resume: bool,
    /// Daily khatma reminder (a Windows notification) when today's portion isn't done.
    pub khatma_reminder: bool,
    /// "HH:MM", local time.
    pub reminder_time: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start_minimized: true,
            start_page: "offline".into(),
            gpu: false,
            download_dir: None,
            close_to_tray: true,
            hotkeys: true,
            pause_on_lock: true,
            pause_on_output: true,
            pause_on_call: true,
            auto_resume: true,
            khatma_reminder: true,
            reminder_time: "20:00".into(),
        }
    }
}

impl Settings {
    pub fn load(path: &PathBuf) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &PathBuf) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, text);
        }
    }
}

fn run_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" {AUTOSTART_ARG}", exe.display()))
}

pub fn autostart_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(RUN_VALUE))
        .is_ok()
}

pub fn set_autostart(enable: bool) -> std::io::Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY)?;
    if enable {
        let cmd = run_command().ok_or_else(|| std::io::Error::other("exe path unknown"))?;
        key.set_value(RUN_VALUE, &cmd)
    } else {
        match key.delete_value(RUN_VALUE) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// If the exe was moved or rebuilt elsewhere, point the Run entry at the current path.
pub fn refresh_autostart_path() {
    if autostart_enabled() {
        let current = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(RUN_KEY)
            .and_then(|k| k.get_value::<String, _>(RUN_VALUE))
            .ok();
        if current != run_command() {
            let _ = set_autostart(true);
        }
    }
}
