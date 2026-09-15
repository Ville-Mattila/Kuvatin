//! Classic per-user Explorer context-menu registration: a cascading "Kuvatin"
//! verb on image files (one `SystemFileAssociations\.<ext>` entry per accepted
//! extension), on folders, and on a folder's background. The submenu lists
//! every preset in the user's store — in store order, then a separator and
//! the fixed actions — and is rewritten whenever presets change.
//!
//! Static verbs are invoked once per selected item; the app folds those
//! processes into one batch at runtime (see `crate::rendezvous`), so a
//! multi-selection — or a folder, or a mix — converts as a single run.

use anyhow::{Context, Result};
use kuvatin_core::format::INPUT_EXTENSIONS;
use kuvatin_core::menu::{command_line, frame_items, menu_items, MenuItem};
use kuvatin_core::preset::PresetStore;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Console::{
    AttachConsole, GetConsoleWindow, GetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK, MB_TOPMOST};

/// Every key below hangs off this one classes root. `super::verbs` strips it
/// back off to name the same keys inside another user's hive, so the absolute
/// paths here stay the only spelling of them.
///
/// `HKCU\Software\Classes` is itself a registry symbolic link to the user's
/// `HKEY_USERS\<SID>_Classes` — see `user_classes_root`, which is the one
/// place that follows it.
pub(super) const CLASSES_ROOT: &str = r"Software\Classes";

/// The verb root that carries the registration sentinels (`Icon` = exe path,
/// `Schema`) — the `.png` entry, which every install has.
const ROOT: &str = r"Software\Classes\SystemFileAssociations\.png\shell\Kuvatin";
/// Schema ≤ 3 attached the verb to Windows' perceived-type group instead.
/// That group includes `.jfif/.jpe/.dib/.ico/.wmf/.emf` (which the engine
/// rejected → "no image files") and excludes `.exr` (no perceived type → no
/// sequence item on a frame). Removed on every (un)register.
pub(super) const LEGACY_ROOT: &str = r"Software\Classes\SystemFileAssociations\image\shell\Kuvatin";
/// The same verb on folders (right-click a folder → converts its images).
pub(super) const FOLDER_ROOT: &str = r"Software\Classes\Directory\shell\Kuvatin";
/// …and on a folder's background (right-click inside an open folder).
pub(super) const BACKGROUND_ROOT: &str = r"Software\Classes\Directory\Background\shell\Kuvatin";

/// Command stores the cascading verbs point at (`ExtendedSubCommandsKey`).
/// Two stores because the item token differs: `%1` is the selected file or
/// folder, while a background verb only has `%V`, the folder itself.
pub(super) const STORE_ITEM: &str = "Kuvatin.CommandStore";
pub(super) const STORE_BACKGROUND: &str = "Kuvatin.CommandStore.Background";
/// Sequence-only frame formats (`.exr`): the image presets can't read them,
/// so their submenu holds just the sequence render.
pub(super) const STORE_FRAMES: &str = "Kuvatin.CommandStore.Frames";
/// The stores as one list: what `super::verbs` deletes, and what
/// `every_root_points_at_a_store_the_uninstall_removes` holds equal to the
/// stores the registered verb roots actually point at.
pub(super) const STORES: &[&str] = &[STORE_ITEM, STORE_BACKGROUND, STORE_FRAMES];

/// Bump when the registered keys or the command lines they hold change, so
/// existing installs (whose `Icon` sentinel already matches the exe)
/// re-register at next launch.
/// 2 = folder + background verbs, MultiSelectModel; 3 = sequence-to-MP4 item;
/// 4 = per-extension roots, presets from the store; 5 = `--` before the path,
/// so a file named like a flag is not parsed as one.
const SCHEMA: &str = "5";

/// Frame formats that are NOT image inputs — they get the verb with the
/// sequence-only store.
const FRAME_ONLY_EXTENSIONS: &[&str] = &["exr"];

/// Every extension the verb attaches to (image inputs, then the sequence-only
/// frame formats), for the sparse-package build via `--print-extensions`.
pub fn menu_extensions() -> Vec<&'static str> {
    INPUT_EXTENSIONS
        .iter()
        .chain(FRAME_ONLY_EXTENSIONS.iter())
        .copied()
        .collect()
}

/// Every extension that carries the verb, with the store its submenu shows.
pub(super) fn extension_roots() -> Vec<(String, &'static str)> {
    let root = |e: &str| format!(r"{CLASSES_ROOT}\SystemFileAssociations\.{e}\shell\Kuvatin");
    INPUT_EXTENSIONS
        .iter()
        .map(|e| (root(e), STORE_ITEM))
        .chain(
            FRAME_ONLY_EXTENSIONS
                .iter()
                .map(|e| (root(e), STORE_FRAMES)),
        )
        .collect()
}

/// The user's presets, or the built-ins if the store can't be read.
fn load_store() -> PresetStore {
    PresetStore::default_path()
        .and_then(|p| PresetStore::load_or_init(&p).ok())
        .unwrap_or_else(PresetStore::builtin)
}

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

fn close_key(hkey: HKEY) {
    unsafe {
        let _ = RegCloseKey(hkey);
    }
}

fn set_string(hkey: HKEY, name: Option<&str>, value: &str) -> Result<()> {
    let wname = name.map(wide);
    let wval = wide(value);
    let bytes = unsafe { std::slice::from_raw_parts(wval.as_ptr() as *const u8, wval.len() * 2) };
    let status = unsafe {
        RegSetValueExW(
            hkey,
            wname
                .as_ref()
                .map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr())),
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

fn set_dword(hkey: HKEY, name: &str, value: u32) -> Result<()> {
    let wname = wide(name);
    let status = unsafe {
        RegSetValueExW(
            hkey,
            PCWSTR(wname.as_ptr()),
            0,
            REG_DWORD,
            Some(&value.to_le_bytes()),
        )
    };
    if status != ERROR_SUCCESS {
        anyhow::bail!("RegSetValueExW (DWORD) failed: {status:?}");
    }
    Ok(())
}

fn delete_tree(path: &str) {
    let wpath = wide(path);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(wpath.as_ptr()));
    }
}

fn exe_path() -> Result<String> {
    Ok(env::current_exe()
        .context("current_exe")?
        .to_string_lossy()
        .into_owned())
}

/// (Re)write one command store (`Software\Classes\<store>\shell\<item>\command`).
/// The old item tree is deleted first so presets removed in the GUI vanish
/// from the menu instead of lingering as "unknown preset" entries.
fn write_store(store: &str, exe: &str, token: &str, items: &[MenuItem]) -> Result<()> {
    let class_key = format!(r"{CLASSES_ROOT}\{store}");
    delete_tree(&format!(r"{class_key}\shell"));
    close_key(create_key(&class_key)?);
    for item in items {
        let item_key = format!(r"{class_key}\shell\{}", item.id);
        let k = create_key(&item_key)?;
        set_string(k, None, &item.label)?;
        set_string(k, Some("MultiSelectModel"), "Player")?;
        if item.separator_before {
            set_dword(k, "CommandFlags", 0x20)?;
        }
        close_key(k);

        let c = create_key(&format!(r"{item_key}\command"))?;
        set_string(c, None, &command_line(exe, &item.action, token))?;
        close_key(c);
    }
    Ok(())
}

/// Every command store registration writes, with the token its command lines
/// substitute and the items its submenu lists.
///
/// The other half of `classic_roots_with_stores`: that names the stores the
/// verbs point *at*, this names the stores actually written, and
/// `every_root_points_at_a_store_the_uninstall_removes` holds both equal to
/// `STORES`. A store written here and listed nowhere else would be created on
/// every machine and uninstalled from none.
fn command_stores<'a>(
    items: &'a [MenuItem],
    frames: &'a [MenuItem],
) -> [(&'static str, &'static str, &'a [MenuItem]); 3] {
    [
        // `%1` is the selected file or folder; a background verb is invoked on
        // no item at all, so its commands take `%V`, the folder it happened in.
        (STORE_ITEM, "%1", items),
        (STORE_BACKGROUND, "%V", items),
        (STORE_FRAMES, "%1", frames),
    ]
}

/// Write the whole registration for the running exe, silently.
fn register_quiet() -> Result<()> {
    let exe = exe_path()?;
    let items = menu_items(&load_store());

    delete_tree(LEGACY_ROOT);
    // The cascading "Kuvatin" verb: per extension, on folders, on backgrounds.
    for (root, store) in classic_roots_with_stores() {
        let k = create_key(&root)?;
        set_string(k, Some("MUIVerb"), "Kuvatin")?;
        set_string(k, Some("ExtendedSubCommandsKey"), store)?;
        set_string(k, Some("Icon"), &exe)?;
        // Without this Explorer hides the verb once more than 15 items are
        // selected ("Player" = any number of items).
        set_string(k, Some("MultiSelectModel"), "Player")?;
        if root == ROOT {
            set_string(k, Some("Schema"), SCHEMA)?;
        }
        close_key(k);
    }

    let frames = frame_items(&items);
    for (store, token, listed) in command_stores(&items, &frames) {
        write_store(store, &exe, token, listed)?;
    }
    Ok(())
}

pub fn register() -> Result<()> {
    register_quiet()?;
    println!("Kuvatin context menu registered.");
    let (package_active, line) = register_package();
    set_classic_verbs_hidden(package_active);
    println!("{line}");
    Ok(())
}

/// Every classic verb root — per extension, folders, folder backgrounds — with
/// the command store its submenu comes from.
///
/// The one list of what the classic menu *is*: `register_quiet` writes exactly
/// these, `set_classic_verbs_hidden` hides exactly these, and `super::verbs`
/// deletes exactly these. Spelled out twice, a root added to the registration
/// alone would be written to every machine and then uninstalled from none —
/// and never hidden behind the Windows 11 menu either, so it would show up as
/// a second, identical "Kuvatin".
pub(super) fn classic_roots_with_stores() -> Vec<(String, &'static str)> {
    let mut roots = extension_roots();
    roots.push((FOLDER_ROOT.to_string(), STORE_ITEM));
    roots.push((BACKGROUND_ROOT.to_string(), STORE_BACKGROUND));
    roots
}

/// The same roots without the stores, for the callers that only need naming.
pub(super) fn classic_roots() -> Vec<String> {
    classic_roots_with_stores()
        .into_iter()
        .map(|(root, _)| root)
        .collect()
}

/// Windows 11 lists a packaged handler in BOTH its new menu and the classic
/// "Show more options" menu, so with the package registered the registry
/// verbs would show up as a second, identical "Kuvatin". `ProgrammaticAccessOnly`
/// keeps a verb off the menu while its keys (and our `Icon`/`Schema`
/// sentinels) stay in place; cleared again when the package is not active
/// (Windows 10, or a failed registration), so the classic menu takes over.
fn set_classic_verbs_hidden(hidden: bool) {
    let name = wide("ProgrammaticAccessOnly");
    for root in classic_roots() {
        let Ok(k) = create_key(&root) else { continue };
        if hidden {
            let _ = set_string(k, Some("ProgrammaticAccessOnly"), "");
        } else {
            unsafe {
                let _ = RegDeleteValueW(k, PCWSTR(name.as_ptr()));
            }
        }
        close_key(k);
    }
}

/// The install directory: where the exe, the handler DLL and the package live.
fn install_dir() -> Result<std::path::PathBuf> {
    let exe = env::current_exe().context("current_exe")?;
    exe.parent()
        .map(|p| p.to_path_buf())
        .context("exe has no parent directory")
}

/// Register the Windows 11 sparse package (see `super::package`), never
/// failing the classic registration over it. Returns whether the package is
/// now active (so the classic verbs must hide) and the line to report.
fn register_package() -> (bool, String) {
    use super::package::Outcome;
    let attempt = install_dir().and_then(|dir| super::package::register(&dir));
    let (active, line) = match attempt {
        Ok(Outcome::Registered) => (true, "Windows 11 menu: registered.".to_string()),
        Ok(Outcome::AlreadyRegistered) => {
            (true, "Windows 11 menu: already registered.".to_string())
        }
        Ok(Outcome::Unsupported) => (
            false,
            "Windows 11 menu: not available on this Windows version (classic menu only)."
                .to_string(),
        ),
        Err(e) => (
            false,
            format!("Windows 11 menu: not registered ({e:#}); the classic menu still works."),
        ),
    };
    crate::applog::log(&line);
    (active, line)
}

/// Rewrite the submenu after the preset store changed (save/delete in the
/// GUI). Only when the menu is OURS — a copy that doesn't own the menu must
/// not take it over just because its user edited a preset. Best-effort.
pub fn sync_menu() {
    let Ok(exe) = exe_path() else { return };
    if read_root_value("Icon").as_deref() == Some(exe.as_str()) {
        let _ = register_quiet();
    }
}

/// Self-healing registration for GUI startup.
///
/// Context-menu registration lives in HKCU, but the MSI only runs
/// `--register` as the installing user — other Windows users on the same
/// machine (and anyone whose install path changed on upgrade) would otherwise
/// have no / dead menu entries. Calling this once at GUI startup heals those
/// cases (see `crates/kuvatin/wix/README.md`, "Registration scope").
///
/// It must NOT hijack: an earlier version re-pointed the menu at whatever exe
/// was running, so `cargo run`, a portable copy or a test build silently
/// rewrote the installed menu (and deleting `target` left dead verbs). Now:
/// - debug builds never touch the menu;
/// - a menu owned by another Kuvatin that still exists is left alone;
/// - only "not registered", "registered exe gone" (moved install) and "ours
///   but an older key set" trigger a (re)registration.
///
/// Idempotent and best-effort: two registry reads on the fast path, and a
/// failure to (re)register never blocks startup, so errors are swallowed.
pub fn ensure_registered() {
    if cfg!(debug_assertions) {
        return;
    }
    let Ok(exe) = exe_path() else { return };
    // The Windows 11 package, per user like the registry menu: another user
    // on the machine, or a moved install, gets it on first launch. Off the UI
    // thread, because enumerating packages takes a moment.
    if super::package::os_supports_package() {
        std::thread::spawn(|| {
            let (active, _) = register_package();
            set_classic_verbs_hidden(active);
        });
    }
    let registered = read_root_value("Icon");
    let schema_current = read_root_value("Schema").as_deref() == Some(SCHEMA);
    match registered.as_deref() {
        Some(owner) if owner == exe && schema_current => {}
        Some(owner) if owner == exe => {
            let _ = register_quiet();
        }
        Some(owner) if std::path::Path::new(owner).exists() => {}
        _ => {
            let _ = register_quiet();
        }
    }
}

/// Read a string value under HKLM (`windows_build()` uses it). `None` when
/// absent or unreadable.
pub(super) fn read_hklm_string(key: &str, name: &str) -> Option<String> {
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    let wkey = wide(key);
    let wname = wide(name);
    let mut buf = [0u16; 256];
    let mut cb = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(wkey.as_ptr()),
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
    let len = (cb as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
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

/// When set (`--quiet`), `notify_error` never pops a dialog. The installer's
/// custom actions run with it: a message box from a deferred action would
/// block the install.
static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

/// The windowed release exe has no console, so a `--register` from a terminal
/// printed nothing. Attaching to the parent process' console (when there is
/// one) makes println!/eprintln! land in that terminal; from Explorer there
/// is no parent console and this is a harmless no-op.
pub fn attach_parent_console() {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// True when there is somewhere textual to report to: a console window (the
/// debug build, or a release exe that joined its terminal's console), or a
/// stderr handle at all — a script that launched the windowed exe with
/// redirected output gets the text instead of a dialog it can't dismiss.
/// Launched from Explorer, both are absent.
fn has_console() -> bool {
    unsafe {
        if !GetConsoleWindow().0.is_null() {
            return true;
        }
        matches!(GetStdHandle(STD_ERROR_HANDLE), Ok(h) if !h.is_invalid() && !h.0.is_null())
    }
}

/// Surface an error to the user for the headless paths.
///
/// With a console (the debug build, or a release exe run from a terminal) it
/// goes to stderr. Under `--quiet` it is dropped. Otherwise (the windowed
/// release build launched from Explorer's right-click menu) a failure would be
/// completely silent — so pop a modal error box. Topmost so it can't hide
/// behind the window the user is looking at, but it does not steal focus.
pub fn notify_error(title: &str, text: &str) {
    if has_console() {
        eprintln!("{title}\n{text}");
        return;
    }
    if QUIET.load(Ordering::Relaxed) {
        return;
    }
    let wtitle = wide(title);
    let wtext = wide(text);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(wtext.as_ptr()),
            PCWSTR(wtitle.as_ptr()),
            MB_OK | MB_ICONERROR | MB_TOPMOST,
        );
    }
}

/// This user's classes hive, open and ready to be worked relative to.
///
/// `HKCU\Software\Classes` is a registry symbolic link to the user's
/// `HKEY_USERS\<SID>_Classes`, and this is the one place that deliberately
/// follows it: the link is Windows' own and its target is exactly the hive we
/// came for. Every deletion then happens *below* the handle it returns, never
/// by walking that path again — `regutil` refuses to pass through a link, so a
/// path-walking delete named `Software\Classes\…` would refuse every verb key
/// and leave the whole menu in place (`a_verb_key_in_the_user_classes_hive_deletes`
/// pins that down). It is also the shape the all-users uninstall works in,
/// where the root is a mounted hive instead.
fn user_classes_root() -> Option<super::regutil::OwnedKey> {
    super::regutil::open_owned(HKEY_CURRENT_USER, CLASSES_ROOT)
}

/// Delete every classic verb key this user has, from the one shared list in
/// `super::verbs` — so the keys an uninstall removes can never drift from the
/// keys a registration writes.
///
/// The deleting itself is `verbs::remove_verbs_under`, which the all-users
/// uninstall runs too against a mounted hive; all that differs here is where
/// the answer goes. Reports it to the log rather than to the user:
/// `--unregister` runs from the installer, where a line about one key that
/// would not go is for whoever reads the log afterwards, and there is nothing
/// the person uninstalling could do with it anyway.
fn remove_classic_verbs() {
    let Some(classes) = user_classes_root() else {
        crate::applog::log(&format!(
            r"Context menu: HKCU\{CLASSES_ROOT} would not open; no keys removed"
        ));
        return;
    };
    let sweep = super::verbs::remove_verbs_under(classes.get());
    for line in &sweep.lines {
        // Every kind of line names a key relative to the hive it swept — the
        // enumeration trouble as much as a note or a refusal — so every kind
        // is logged with that hive in front of it.
        let text = match line {
            super::verbs::SweepLine::Trouble(text)
            | super::verbs::SweepLine::Note(text)
            | super::verbs::SweepLine::Refused(text) => text,
        };
        crate::applog::log(&format!(r"Context menu: HKCU\{CLASSES_ROOT}\{text}"));
    }
    // Told apart, because they mean different things: keys already gone is an
    // ordinary second uninstall, keys refused is the menu still on the machine.
    crate::applog::log(&format!(
        "Context menu: {} key trees removed, {} already gone, {} would not go",
        sweep.removed, sweep.absent, sweep.refused
    ));
}

pub fn unregister() -> Result<()> {
    remove_classic_verbs();
    match super::package::unregister() {
        Ok(true) => crate::applog::log("Windows 11 menu: package removed"),
        Ok(false) => {}
        Err(e) => crate::applog::log(&format!("Windows 11 menu: package removal failed ({e:#})")),
    }
    println!("Kuvatin context menu removed.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::regutil::{
        delete_tree_under, is_reg_link, open_owned_no_links, DeleteOutcome,
    };
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A scratch key in the user's real classes hive — the same place the verb
    /// keys live, because that is the hive under test — removed when the test
    /// ends, pass, fail or panic.
    struct ClassesScratch {
        name: String,
    }

    impl ClassesScratch {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            // The clock ticks every 100 ns here, which two tests starting
            // together can share; the counter is what keeps them apart.
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let me = ClassesScratch {
                name: format!(
                    "Kuvatin-classes-test-{}-{nanos}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ),
            };
            // Shaped like a real verb root: values on the key, a subkey under
            // it. An empty key would prove less than the ones we delete.
            let verb = create_key(&me.absolute(r"shell\Kuvatin")).expect("create a scratch verb");
            set_string(verb, Some("MUIVerb"), "Kuvatin").expect("MUIVerb");
            set_string(verb, Some("Icon"), "kuvatin.exe").expect("Icon");
            close_key(verb);
            let command =
                create_key(&me.absolute(r"shell\Kuvatin\command")).expect("create the command key");
            set_string(command, None, "kuvatin.exe --convert").expect("command line");
            close_key(command);
            me
        }

        /// `Software\Classes\Kuvatin-classes-test-…\<rest>`.
        fn absolute(&self, rest: &str) -> String {
            format!(r"{CLASSES_ROOT}\{}\{rest}", self.name)
        }
    }

    impl Drop for ClassesScratch {
        fn drop(&mut self) {
            let Some(classes) = user_classes_root() else {
                eprintln!("could not open the classes root to clean up {}", self.name);
                return;
            };
            // Say so loudly rather than leaving a key in the live classes hive.
            match delete_tree_under(classes.get(), &self.name) {
                DeleteOutcome::Deleted { .. } | DeleteOutcome::Absent => {}
                DeleteOutcome::Refused { why, .. } => eprintln!(
                    "scratch key HKCU\\{}\\{} survived cleanup ({why}); remove it by hand",
                    CLASSES_ROOT, self.name
                ),
            }
        }
    }

    /// The per-user unregister works *below* an open classes root, never by
    /// walking `HKCU\Software\Classes\…` as a path. It has to: that key is a
    /// registry symbolic link, and `regutil` refuses to step through one, so a
    /// path-walking delete would refuse all seventeen verb keys and silently
    /// leave the menu behind.
    #[test]
    fn a_verb_key_in_the_user_classes_hive_deletes() {
        let scratch = ClassesScratch::new();
        let verb = format!(r"{}\shell\Kuvatin", scratch.name);

        // Why the relative form is not a style choice. Conditional because it
        // is Windows' behaviour being reported, not ours: where the classes
        // root is not a link there is nothing to refuse.
        if is_reg_link(HKEY_CURRENT_USER, CLASSES_ROOT) {
            let why = open_owned_no_links(
                HKEY_CURRENT_USER,
                &scratch.absolute(r"shell\Kuvatin"),
                KEY_WRITE,
            )
            .expect_err("a path through the classes link should be refused");
            assert!(why.contains("SymbolicLinkValue"), "{why}");
        }

        let classes = user_classes_root().expect("open the user's classes root");
        let outcome = delete_tree_under(classes.get(), &verb);
        assert!(
            matches!(outcome, DeleteOutcome::Deleted { .. }),
            "a verb key in the live classes hive should go: {outcome:?}"
        );
        // Reading may follow the link; only deleting must not.
        assert!(
            super::super::regutil::open_owned(
                HKEY_CURRENT_USER,
                &scratch.absolute(r"shell\Kuvatin")
            )
            .is_none(),
            "the verb key should be gone"
        );
    }

    /// The sparse-package build script carries a static copy of the extension
    /// list (it cannot run the exe on CI); this keeps it equal to the engine's.
    #[test]
    fn msix_build_script_lists_the_same_extensions() {
        let script = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("msix/build-msix.ps1"),
        )
        .expect("read build-msix.ps1");
        let line = script
            .lines()
            .find(|l| l.trim_start().starts_with("$extensions = "))
            .expect("$extensions line");
        let listed: Vec<String> = line
            .split('\'')
            .skip(1)
            .step_by(2)
            .map(|s| s.trim_start_matches('.').to_string())
            .collect();
        assert_eq!(listed, menu_extensions(), "update build-msix.ps1");
    }

    /// The token each store's command lines substitute, pinned per store.
    ///
    /// A background verb is invoked on no item at all: it has only `%V`, the
    /// folder the right-click happened in. Give that store `%1` and every
    /// background conversion would run with an empty path — a menu that looks
    /// right and does nothing. The other two are invoked on the item itself.
    #[test]
    fn each_store_takes_the_token_its_verbs_can_supply() {
        for (store, token, _) in command_stores(&[], &[]) {
            let want = if store == STORE_BACKGROUND {
                "%V"
            } else {
                "%1"
            };
            assert_eq!(token, want, "{store} substitutes {want}");
        }
    }

    /// Registration points every verb root at a command store and writes every
    /// store, and the uninstall deletes the stores in `STORES`. A fourth store
    /// written or pointed at, but never added to that list, would be created on
    /// every machine and removed from none.
    #[test]
    fn every_root_points_at_a_store_the_uninstall_removes() {
        let mut listed: Vec<&str> = STORES.to_vec();
        listed.sort_unstable();

        let mut referenced: Vec<&str> = classic_roots_with_stores()
            .into_iter()
            .map(|(_, store)| store)
            .collect();
        referenced.sort_unstable();
        referenced.dedup();
        assert_eq!(referenced, listed, "a verb points at an unlisted store");

        // The same on the writing side: the submenus are empty here because
        // only the names are under test.
        let mut written: Vec<&str> = command_stores(&[], &[])
            .iter()
            .map(|(store, _, _)| *store)
            .collect();
        written.sort_unstable();
        written.dedup();
        assert_eq!(written, listed, "a store is written but never removed");
    }

    /// Per-extension roots: the aliases Windows' perceived-type group carried
    /// (.jfif) are in with the full store, EXR is in (it had no perceived
    /// type) with the sequence-only store, the old group is not used.
    #[test]
    fn verb_roots_follow_the_canonical_extension_list() {
        let roots = extension_roots();
        let store_of = |ext: &str| {
            roots
                .iter()
                .find(|(r, _)| r.contains(&format!(r"\.{ext}\")))
                .map(|(_, s)| *s)
        };
        assert_eq!(store_of("jfif"), Some(STORE_ITEM));
        assert_eq!(store_of("tif"), Some(STORE_ITEM));
        assert_eq!(store_of("exr"), Some(STORE_FRAMES));
        assert!(roots.iter().any(|(r, _)| r == ROOT));
        assert!(!roots.iter().any(|(r, _)| r.contains(r"\image\")));
    }
}
