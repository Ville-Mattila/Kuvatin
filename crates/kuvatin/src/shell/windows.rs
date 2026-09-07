//! Classic per-user Explorer context-menu registration: a cascading "Kuvatin"
//! verb on image files, on folders, and on a folder's background.
//!
//! Static verbs are invoked once per selected item; the app folds those
//! processes into one batch at runtime (see `crate::rendezvous`), so a
//! multi-selection — or a folder, or a mix — converts as a single run.

use anyhow::{Context, Result};
use std::env;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Console::GetConsoleWindow;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
};

/// The cascading verb on image files. Also carries the registration sentinels
/// (`Icon` = exe path, `Schema`) that `ensure_registered` checks.
const ROOT: &str = r"Software\Classes\SystemFileAssociations\image\shell\Kuvatin";
/// The same verb on folders (right-click a folder → converts its images).
const FOLDER_ROOT: &str = r"Software\Classes\Directory\shell\Kuvatin";
/// …and on a folder's background (right-click inside an open folder).
const BACKGROUND_ROOT: &str = r"Software\Classes\Directory\Background\shell\Kuvatin";

/// Command stores the cascading verbs point at (`ExtendedSubCommandsKey`).
/// Two stores because the item token differs: `%1` is the selected file or
/// folder, while a background verb only has `%V`, the folder itself.
const STORE_ITEM: &str = "Kuvatin.CommandStore";
const STORE_BACKGROUND: &str = "Kuvatin.CommandStore.Background";

/// Bump when the set of registry keys changes, so existing installs (whose
/// `Icon` sentinel already matches the exe) re-register at next launch.
/// 2 = folder + background verbs, MultiSelectModel.
const SCHEMA: &str = "2";

/// (command id under a store, menu label, preset name or empty for GUI)
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

/// The command line a store item runs. `token` is Explorer's placeholder for
/// the clicked item: `%1` (file/folder) or `%V` (background folder).
fn command_line(exe: &str, preset: &str, token: &str) -> String {
    if preset.is_empty() {
        format!("\"{exe}\" \"{token}\"")
    } else {
        format!("\"{exe}\" --preset \"{preset}\" \"{token}\"")
    }
}

/// Write one command store (`Software\Classes\<store>\shell\<item>\command`).
fn write_store(store: &str, exe: &str, token: &str) -> Result<()> {
    let class_key = format!(r"Software\Classes\{store}");
    let storeroot = create_key(&class_key)?;
    unsafe {
        let _ = RegCloseKey(storeroot);
    };
    for (id, label, preset) in ITEMS {
        let item_key = format!(r"{class_key}\shell\{id}");
        let k = create_key(&item_key)?;
        set_string(k, None, label)?;
        set_string(k, Some("MultiSelectModel"), "Player")?;
        unsafe {
            let _ = RegCloseKey(k);
        };

        let c = create_key(&format!(r"{item_key}\command"))?;
        set_string(c, None, &command_line(exe, preset, token))?;
        unsafe {
            let _ = RegCloseKey(c);
        };
    }
    Ok(())
}

pub fn register() -> Result<()> {
    let exe = exe_path()?;

    // The cascading "Kuvatin" verb, attached in three places.
    for (root, store) in [
        (ROOT, STORE_ITEM),
        (FOLDER_ROOT, STORE_ITEM),
        (BACKGROUND_ROOT, STORE_BACKGROUND),
    ] {
        let k = create_key(root)?;
        set_string(k, Some("MUIVerb"), "Kuvatin")?;
        set_string(k, Some("ExtendedSubCommandsKey"), store)?;
        set_string(k, Some("Icon"), &exe)?;
        // Without this Explorer hides the verb once more than 15 items are
        // selected ("Player" = any number of items).
        set_string(k, Some("MultiSelectModel"), "Player")?;
        if root == ROOT {
            set_string(k, Some("Schema"), SCHEMA)?;
        }
        unsafe {
            let _ = RegCloseKey(k);
        };
    }

    write_store(STORE_ITEM, &exe, "%1")?;
    write_store(STORE_BACKGROUND, &exe, "%V")?;

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
    if read_root_value("Icon").as_deref() == Some(exe.as_str())
        && read_root_value("Schema").as_deref() == Some(SCHEMA)
    {
        return; // registered for this user, pointing at us, with the current key set
    }
    let _ = register();
}

/// Read back a string value under ROOT that `register()` writes (`Icon` holds
/// the absolute exe path, `Schema` the key-set version). `None` when
/// unregistered or unreadable.
fn read_root_value(name: &str) -> Option<String> {
    let wpath = wide(ROOT);
    let wname = wide(name);
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

/// True when the process has an attached console window. The debug build is a
/// console subsystem (run from a terminal); the release build is windowed and
/// has none, so its stdout/stderr go nowhere.
fn has_console() -> bool {
    unsafe { !GetConsoleWindow().0.is_null() }
}

/// Surface an error to the user for the headless context-menu quick-run path.
///
/// When there's a console, the caller has already printed the detail there, so
/// this is a no-op. When there isn't (the windowed release build launched from
/// Explorer's right-click menu), a failure would otherwise be completely
/// silent — so pop a modal error box instead.
pub fn notify_error(title: &str, text: &str) {
    if has_console() {
        return;
    }
    let wtitle = wide(title);
    let wtext = wide(text);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(wtext.as_ptr()),
            PCWSTR(wtitle.as_ptr()),
            MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

pub fn unregister() -> Result<()> {
    let stores = [
        format!(r"Software\Classes\{STORE_ITEM}"),
        format!(r"Software\Classes\{STORE_BACKGROUND}"),
    ];
    for path in [ROOT, FOLDER_ROOT, BACKGROUND_ROOT]
        .into_iter()
        .chain(stores.iter().map(String::as_str))
    {
        let wpath = wide(path);
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(wpath.as_ptr()));
        }
    }
    println!("Kuvatin context menu removed.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_quote_the_exe_and_pass_the_item_token() {
        assert_eq!(
            command_line(r"C:\Program Files\Kuvatin\kuvatin.exe", "Convert to WebP", "%1"),
            r#""C:\Program Files\Kuvatin\kuvatin.exe" --preset "Convert to WebP" "%1""#
        );
        // The GUI item has no preset; background verbs get the folder via %V.
        assert_eq!(
            command_line(r"C:\k\kuvatin.exe", "", "%V"),
            r#""C:\k\kuvatin.exe" "%V""#
        );
    }
}
