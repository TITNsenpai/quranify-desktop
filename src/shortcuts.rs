//! Start Menu and Desktop shortcuts, created with the Windows Shell API.
//!
//! WScript.Shell (the usual PowerShell way) can't save shortcuts whose paths have characters
//! outside the system code page, e.g. an Arabic user name on an English Windows. IShellLinkW
//! is fully Unicode, and doesn't need a hidden PowerShell window.

use std::path::{Path, PathBuf};

use windows::{
    Win32::{
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, IPersistFile,
        },
        UI::Shell::{FOLDERID_Desktop, FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink},
    },
    core::{HSTRING, Interface},
};

const NAME: &str = "Quranify.lnk";

/// The user's Start Menu "Programs" folder and Desktop.
fn folders() -> Vec<PathBuf> {
    [FOLDERID_Programs, FOLDERID_Desktop]
        .iter()
        .filter_map(|id| unsafe {
            let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as _));
            s.map(PathBuf::from)
        })
        .collect()
}

/// Returns false if any shortcut couldn't be created.
#[allow(dead_code)]
pub fn create(exe: &Path, dir: &Path) -> bool {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let mut ok = true;
    for folder in folders() {
        let made: windows::core::Result<()> = unsafe {
            (|| {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                link.SetPath(&HSTRING::from(exe))?;
                link.SetWorkingDirectory(&HSTRING::from(dir))?;
                link.SetIconLocation(&HSTRING::from(exe), 0)?;
                link.SetDescription(&HSTRING::from("Quranify"))?;
                let file: IPersistFile = link.cast()?;
                file.Save(&HSTRING::from(folder.join(NAME).as_path()), true)
            })()
        };
        ok &= made.is_ok();
    }
    ok
}

#[allow(dead_code)]
pub fn remove() {
    for folder in folders() {
        let _ = std::fs::remove_file(folder.join(NAME));
    }
}
