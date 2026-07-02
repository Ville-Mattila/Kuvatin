//! Classic per-user Explorer context-menu registration for image files.

use anyhow::{Context, Result};
use std::env;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
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

/// Self-healing registration for GUI startup: cheaply verify that the
/// per-user context-menu registration exists *and* points at this exe, and
/// re-run the full registration when it is missing or stale.
///
/// Context-menu registration lives in HKCU, but the MSI only runs
/// `--register` as the installing user — other Windows users on the same
/// machine (and anyone whose install path changed on upgrade) would otherwise
/// have no / dead menu entries. Calling this once at GUI startup heals both
/// cases (see `crates/kuvatin/wix/README.md`, "Registration scope").
///
/// Idempotent and best-effort by design: the fast path is a single registry
/// read (the `Icon` value that `register()` writes holds the exe path, so it
/// doubles as a "registered and current?" sentinel), and a failure to
/// (re)register must never block app startup, so errors are swallowed.
pub fn ensure_registered() {
    let Ok(exe) = exe_path() else { return };
    if registered_exe_path().as_deref() == Some(exe.as_str()) {
        return; // already registered for this user and pointing at us
    }
    let _ = register();
}

/// Read back the `Icon` value under ROOT that `register()` writes (it is set
/// to the absolute exe path). `None` when unregistered or unreadable.
fn registered_exe_path() -> Option<String> {
    let wpath = wide(ROOT);
    let wname = wide("Icon");
    // MAX_PATH-with-headroom; a long-path exe simply fails the read and takes
    // the (idempotent) re-register path.
    let mut buf = [0u16; 1024];
    let mut cb = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(wpath.as_ptr()),
            PCWSTR(wname.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut cb),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let units = (cb as usize / 2).min(buf.len());
    let value = &buf[..units];
    let value = &value[..value.iter().position(|&u| u == 0).unwrap_or(value.len())];
    Some(String::from_utf16_lossy(value))
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
