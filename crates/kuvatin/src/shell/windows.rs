//! Classic per-user Explorer context-menu registration for image files.

use anyhow::{Context, Result};
use std::env;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const ROOT: &str = r"Software\Classes\SystemFileAssociations\image\shell\Kuvatin";
const STORE: &str = r"Software\Classes\Kuvatin.CommandStore\shell";

/// (command id under CommandStore, menu label, preset name or empty for GUI)
const ITEMS: &[(&str, &str, &str)] = &[
    ("Kuvatin.Webp", "Convert to WebP", "Convert to WebP"),
    ("Kuvatin.1080p", "Resize to 1080p", "Resize to 1080p"),
    ("Kuvatin.Half", "Resize to 50%", "Resize to 50%"),
    ("Kuvatin.Open", "Open in Kuvatin…", ""),
];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn create_key(path: &str) -> Result<HKEY> {
    let mut hkey = HKEY::default();
    let wpath = wide(path);
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(wpath.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if status != ERROR_SUCCESS {
        anyhow::bail!("RegCreateKeyExW failed for {path}: {status:?}");
    }
    Ok(hkey)
}

fn set_string(hkey: HKEY, name: Option<&str>, value: &str) -> Result<()> {
    let wname = name.map(wide);
    let wval = wide(value);
    let bytes = unsafe { std::slice::from_raw_parts(wval.as_ptr() as *const u8, wval.len() * 2) };
    let status = unsafe {
        RegSetValueExW(
            hkey,
            wname.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr())),
            0,
            REG_SZ,
            Some(bytes),
        )
    };
    if status != ERROR_SUCCESS {
        anyhow::bail!("RegSetValueExW failed: {status:?}");
    }
    Ok(())
}

fn exe_path() -> Result<String> {
    Ok(env::current_exe()
        .context("current_exe")?
        .to_string_lossy()
        .into_owned())
}

pub fn register() -> Result<()> {
    let exe = exe_path()?;

    let root = create_key(ROOT)?;
    set_string(root, Some("MUIVerb"), "Kuvatin")?;
    set_string(root, Some("ExtendedSubCommandsKey"), r"Kuvatin.CommandStore")?;
    set_string(root, Some("Icon"), &exe)?;
    unsafe {
        let _ = RegCloseKey(root);
    };

    let storeroot = create_key(r"Software\Classes\Kuvatin.CommandStore")?;
    unsafe {
        let _ = RegCloseKey(storeroot);
    };

    for (id, label, preset) in ITEMS {
        let item_key = format!(r"{STORE}\{id}");
        let k = create_key(&item_key)?;
        set_string(k, None, label)?;
        unsafe {
            let _ = RegCloseKey(k);
        };

        let cmd_key = format!(r"{item_key}\command");
        let c = create_key(&cmd_key)?;
        let command = if preset.is_empty() {
            format!("\"{exe}\" \"%1\"")
        } else {
            format!("\"{exe}\" --preset \"{preset}\" \"%1\"")
        };
        set_string(c, None, &command)?;
        unsafe {
            let _ = RegCloseKey(c);
        };
    }

    println!("Kuvatin context menu registered.");
    Ok(())
}

pub fn unregister() -> Result<()> {
    for path in [ROOT, r"Software\Classes\Kuvatin.CommandStore"] {
        let wpath = wide(path);
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(wpath.as_ptr()));
        }
    }
    println!("Kuvatin context menu removed.");
    Ok(())
}
