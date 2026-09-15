# Clean uninstall for every account (pk-uninst) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make uninstall remove Kuvatin's Explorer menu, its Windows 11 sparse package and its per-user files for *every* account on the machine, not just the uninstalling one.

**Architecture:** A new `kuvatin.exe --unregister-all-users` mode runs as SYSTEM from a deferred WiX custom action (`WixQuietExec64`, `Impersonate='no'`, `Return='ignore'`), after the existing impersonated `KuvatinUnregister` and before `KuvatinUntrustCert`, and only when not a major upgrade. It walks every real user profile in `HKLM\...\ProfileList`, removes the classic verbs from each profile's classes hive (the loaded `HKEY_USERS\<SID>_Classes` when present, otherwise `RegLoadKeyW` of `UsrClass.dat`), removes the sparse package for all users via the `PackageManager` API with `RemovalOptions::RemoveForAllUsers`, and deletes each profile's Kuvatin logs, `%TEMP%\kuvatin` and `AppData\Local\Packages\VilleMattila.Kuvatin_*` with a junction-safe walk. Presets and settings stay. The registry key list becomes one shared source of truth used by both the existing per-user unregister and the new all-users path.

**Tech Stack:** Rust (`windows` 0.58 crate — `Win32_System_Registry`, `Win32_Security`, `Win32_System_Threading`, `Management_Deployment`, `Foundation`), WiX Toolset v3 (`WixUtilExtension` / `WixCA`), GitHub Actions (`windows-latest`, PowerShell 5.1 + pwsh 7).

---

## Orientation for the implementer (read once)

**Where the work happens:** the worktree `C:\Työt\Koodaus\Kuvatin\.claude\worktrees\uninstall`, on branch `uninstall-every-account`, which starts from the current tip of `after-undo`. **Run every command from that worktree.** Never work in the main checkout `C:\Työt\Koodaus\Kuvatin` — other work is going on there. Build on the worktree's current files; the line numbers cited below come from that tree and may have shifted by a line or two, so search for the quoted code rather than trusting the number blindly.

**What exists today (cite before you change):**
- Per-user registration/unregistration: `crates/kuvatin/src/shell/windows.rs`. Registry writes all go to `HKEY_CURRENT_USER` via `create_key` (`windows.rs:97-117`), `delete_tree` (`windows.rs:163-168`), `wide` (`windows.rs:93-95`). The verb key roots: `ROOT`/`LEGACY_ROOT`/`FOLDER_ROOT`/`BACKGROUND_ROOT` (`windows.rs:30-39`), the stores (`windows.rs:44-48`), `extension_roots()` (`windows.rs:73-84`), `menu_extensions()` (`windows.rs:64-70`). Today's `unregister()` is `windows.rs:474-491`.
- The Windows 11 sparse package: `crates/kuvatin/src/shell/package.rs`. `PACKAGE_NAME = "VilleMattila.Kuvatin"` (`package.rs:23`), `os_supports_package()` (`package.rs:51-53`), `init_com()` (`package.rs:55-61`), `registered()`/`unregister()`/`remove()` (`package.rs:85-166`).
- The module surface: `crates/kuvatin/src/shell/mod.rs` (`pub use` list at `mod.rs:6-10`; non-windows stubs below it).
- CLI: `crates/kuvatin/src/cli.rs` (`Cli` struct `cli.rs:11-50`, `Mode` `cli.rs:52-70`, `into_mode()` `cli.rs:72-101`, tests `cli.rs:118+`).
- Dispatch: `crates/kuvatin/src/main.rs:142-263`. Before the `match cli.into_mode()` (`main.rs:157`) it calls `shell::attach_parent_console()` (`main.rs:145`), `applog::install_panic_hook()` (`main.rs:146`), `configure_bundled_gstreamer()` (`main.rs:147`), `configure_batch_memory()` (`main.rs:148`), then parses args (`main.rs:149-156`).
- Logs live at `%LOCALAPPDATA%\Kuvatin\kuvatin.log` / `.log.1` / `crash.log` (`crates/kuvatin/src/applog.rs:19-35`). Presets at `%APPDATA%\Kuvatin\presets.toml` (`crates/kuvatin-core/src/preset.rs:241-243`). Settings at `%APPDATA%\Kuvatin\settings.toml` (`crates/kuvatin/src/settings.rs:23-25`). Rendezvous at `%TEMP%\kuvatin\rendezvous` (`crates/kuvatin/src/rendezvous.rs:106`). Frame cache at `%TEMP%\kuvatin\seq-cache` (`crates/kuvatin-video/src/sequence.rs:229`).
- The WiX source: `crates/kuvatin/wix/main.wxs`. `xmlns:util` at line 60, `<util:CloseApplication>` at `main.wxs:111-116` (proves `WixUtilExtension`/`WixCA` are already linked by cargo-wix — do not add a new `-ext`). The custom actions block `main.wxs:270-303`, the `InstallExecuteSequence` `main.wxs:305-315`.
- CI: `.github/workflows/release.yml`. Build MSI + signing step (~343-432), Install test (~437-507), env `PACKAGE`/`SIGNED`.

**Rules for this whole plan:**
- The SYSTEM path must **never** call `crate::applog::*` — as SYSTEM those functions resolve `%LOCALAPPDATA%` to `C:\Windows\System32\config\systemprofile\AppData\Local\Kuvatin`, which would be a brand-new leftover. Everything in the all-users path prints to **stdout** only.
- The SYSTEM path must **never** call `load_store()`, `PresetStore::load_or_init`, `register_quiet()` or anything that creates presets — those would seed files in the SYSTEM profile.
- **Never** delete a whole `AppData\Local\Kuvatin` or `AppData\Roaming\Kuvatin` folder outright: the local folder can hold the user's signing keys, and the roaming folder holds presets/settings we keep. Delete named files, then remove the parent only if it is empty.
- Keep the existing impersonated `KuvatinUnregister` action. The all-users action is additive.
- **No new crate targets.** The `kuvatin` package stays a binary with no `lib.rs` and no `tests/` directory; every new test is an in-crate `#[cfg(test)]` test. `Cargo.toml` is not edited by this plan.

**Toolchain facts (already checked against the installed crates):**
- `windows` 0.58 exposes everything used here: `RegLoadKeyW`, `RegUnLoadKeyW`, `RegOpenKeyExW`, `RegEnumKeyExW`, `RegQueryValueExW`, `RegDeleteTreeW`, `RegCreateKeyExW`, `RegCloseKey`, `HKEY_USERS`, `KEY_ALL_ACCESS`, `KEY_READ`, `KEY_WRITE`, `REG_LINK`, `REG_OPTION_OPEN_LINK`, `REG_OPTION_NON_VOLATILE`, `REG_VALUE_TYPE`, `RRF_RT_REG_SZ`, `RRF_RT_REG_EXPAND_SZ`, `RRF_NOEXPAND` (all under `Win32_System_Registry`); `OpenProcessToken`, `GetCurrentProcess` (`Win32_System_Threading`); `AdjustTokenPrivileges`, `LookupPrivilegeValueW`, `GetTokenInformation`, `SE_BACKUP_NAME`, `SE_RESTORE_NAME`, `SE_PRIVILEGE_ENABLED`, `TOKEN_ADJUST_PRIVILEGES`, `TOKEN_QUERY`, `TOKEN_PRIVILEGES`, `LUID_AND_ATTRIBUTES`, `TokenElevation`, `TOKEN_ELEVATION` (`Win32_Security`); `PackageManager::FindPackages`, `FindUsers`, `RemovePackageWithOptionsAsync`, `DeprovisionPackageForAllUsersAsync`, `FindProvisionedPackages`, `RemovalOptions::RemoveForAllUsers`, `PackageInstallState`, `PackageUserInformation::{UserSecurityId, InstallState}` (`Management_Deployment`); `AsyncOperationWithProgressCompletedHandler::new(FnMut(Option<&IAsyncOperationWithProgress<..>>, AsyncStatus) -> Result<()> + Send + 'static)` and `IAsyncOperationWithProgress::get()` (`Foundation`). The crate features are already enabled in `crates/kuvatin/Cargo.toml:29-48`; **no `Cargo.toml` change is required**.
- `menu_extensions()` returns image inputs then `exr` (`windows.rs:64-70`). Today the list is `png,jpg,jpeg,jpe,jfif,webp,bmp,tiff,tif,gif` + `exr` = 11 extension roots. With the `image` legacy root, `Directory`, `Directory\Background` and the three stores, that is 17 classic keys per profile — the same 17 the probe counted.

**Test running (so nothing passes by skipping):**
- Every test in this plan is an in-crate unit test and runs in `cargo test -p kuvatin --release`, which is already a release gate (`release.yml:212-213`).
- The offline-hive test (Task 5) needs SeBackup/SeRestore, so it returns early with the line `skipping: not elevated` when the process is not elevated. A hosted `windows-latest` runner is elevated, so it runs there; a normal dev shell skips it. Task 14b adds a step that runs it **by exact name** and fails if the output carries the skip line or lacks the passing line, so it can never pass by skipping on CI.
- The junction-safe file test (Task 7) creates a real directory junction with `cmd /c mklink /J` (no privilege needed) and runs everywhere; if junction creation fails it skips cleanly.
- End-to-end proof (all accounts) is the new CI step (Task 14c), gated by `PACKAGE == 'true'`, which runs on tags, on manual dispatch, and on pull requests that touch the installer (`release.yml:63,94-112`).

**Known gap (accepted):** CI proves package removal for accounts that are *signed in* at uninstall time, plus the offline **registry** path (Task 5's hive test). It does not prove package removal for an account that is *signed out* at uninstall time — a scheduled-task logon cannot be forced to release its profile, and a hosted runner has no second interactive session. For those accounts we rely on Windows' documented `RemovalOptions.RemoveForAllUsers` behaviour. If a future report says a signed-out account keeps its registration, this is the first place to look.

**Commit rule:** every commit message ends with exactly:
```
Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```
Commit titles are plain sentences in this repo's voice — no `feat:`, `wix:`, `ci:` or `docs:` prefixes. Compare `git log --oneline -15`: "Undo is gated in CI and in the changelog", "The mouse wheel no longer scrolls behind an open dialog", "Every undo step describes itself the same way".

---

## File structure

- **Create `crates/kuvatin/src/shell/regutil.rs`** — generic-`HKEY` registry helpers (`wide`, `open_subkey`, `enum_subkeys`, `is_reg_link`, `delete_tree_under`). No HKCU assumptions. Used by `verbs.rs`, `hive.rs`, `profiles.rs`.
- **Create `crates/kuvatin/src/shell/verbs.rs`** — the single source of truth for the classic verb subkey list, relative to a classes root. Consumed by `windows.rs::unregister()` and by `hive.rs`.
- **Create `crates/kuvatin/src/shell/profiles.rs`** — `ProfileList` enumeration and the SID filter.
- **Create `crates/kuvatin/src/shell/paths.rs`** — per-profile file/dir plan (pure), plus the "never the whole Kuvatin folder" guard and the `Packages\VilleMattila.Kuvatin_*` prefix.
- **Create `crates/kuvatin/src/shell/files.rs`** — junction-safe recursive delete.
- **Create `crates/kuvatin/src/shell/hive.rs`** — privilege enable, elevation check, load/delete/unload a profile's classes hive, refuse `REG_LINK`; the offline-hive test.
- **Modify `crates/kuvatin/src/shell/package.rs`** — add `unregister_all_users()` (all users, bounded wait).
- **Create `crates/kuvatin/src/shell/allusers.rs`** — the orchestrator `unregister_all_users()` that ties profiles + hive + files + package together, stdout only.
- **Modify `crates/kuvatin/src/shell/windows.rs`** — `unregister()` consumes `verbs::classes_subkeys()`.
- **Modify `crates/kuvatin/src/shell/mod.rs`** — declare the new modules; export `unregister_all_users`; add the non-windows stub.
- **Modify `crates/kuvatin/src/cli.rs`** — `--unregister-all-users` flag and `Mode::UnregisterAllUsers`.
- **Modify `crates/kuvatin/src/main.rs`** — dispatch the new mode before the panic hook and engine setup, stdout only.
- **Modify `crates/kuvatin/wix/main.wxs`** — `SetProperty` + `CustomAction` (`WixQuietExec64`) + sequencing.
- **Modify `.github/workflows/release.yml`** — throwaway signing key on non-tag packaging runs; the offline-hive test by name; the new uninstall-every-account step.
- **Modify `crates/kuvatin/wix/README.md`** and **`CHANGELOG.md`** — docs.

---

## Task 1: The generic registry helpers (`regutil.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/regutil.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [ ] **Step 1: Declare the module (compile scaffold)**

In `crates/kuvatin/src/shell/mod.rs`, add under the existing `#[cfg(windows)] mod windows;` block:

```rust
#[cfg(windows)]
mod regutil;
```

- [ ] **Step 2: Write `regutil.rs` with its unit test**

Create `crates/kuvatin/src/shell/regutil.rs`:

```rust
//! Registry helpers that operate on an arbitrary open `HKEY` root, so the same
//! code can act on `HKEY_CURRENT_USER\Software\Classes` (the per-user unregister)
//! and on a mounted `HKEY_USERS\<SID>_Classes` hive (the all-users uninstall).

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteTreeW, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY,
    KEY_ALL_ACCESS, KEY_READ, REG_LINK, REG_OPTION_OPEN_LINK, REG_VALUE_TYPE,
};

/// NUL-terminated UTF-16, for the `PCWSTR` registry APIs.
pub(super) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Open `root\subpath` for read, following any symbolic-link key. `None` when
/// the key does not exist or cannot be opened.
pub(super) fn open_subkey(root: HKEY, subpath: &str) -> Option<HKEY> {
    let w = wide(subpath);
    let mut h = HKEY::default();
    let status = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), 0, KEY_READ, &mut h) };
    (status == ERROR_SUCCESS).then_some(h)
}

pub(super) fn close(h: HKEY) {
    unsafe {
        let _ = RegCloseKey(h);
    }
}

/// The immediate subkey names of `root\subpath` (empty when the key is absent).
pub(super) fn enum_subkeys(root: HKEY, subpath: &str) -> Vec<String> {
    let Some(key) = open_subkey(root, subpath) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut index = 0u32;
    loop {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                PWSTR(name.as_mut_ptr()),
                &mut len,
                None,
                PWSTR::null(),
                None,
                None,
            )
        };
        if status == ERROR_NO_MORE_ITEMS || status != ERROR_SUCCESS {
            break;
        }
        out.push(String::from_utf16_lossy(&name[..len as usize]));
        index += 1;
    }
    close(key);
    out
}

/// True when `root\subpath` is a registry symbolic link (a planted `REG_LINK`
/// inside a user's own hive). Such a key must NOT be deleted with
/// `RegDeleteTreeW`, which would follow it.
pub(super) fn is_reg_link(root: HKEY, subpath: &str) -> bool {
    let w = wide(subpath);
    let mut h = HKEY::default();
    // REG_OPTION_OPEN_LINK opens the link itself rather than its target.
    let status = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(w.as_ptr()),
            REG_OPTION_OPEN_LINK.0,
            KEY_READ,
            &mut h,
        )
    };
    if status != ERROR_SUCCESS {
        return false;
    }
    let name = wide("SymbolicLinkValue");
    let mut kind = REG_VALUE_TYPE::default();
    let present =
        unsafe { RegQueryValueExW(h, PCWSTR(name.as_ptr()), None, Some(&mut kind), None, None) };
    close(h);
    present == ERROR_SUCCESS && kind == REG_LINK
}

/// Delete the subtree `root\subpath`. Returns `true` when the key existed and
/// was deleted, `false` when it was absent, a symbolic link (refused), or the
/// delete failed. Never follows a `REG_LINK`.
pub(super) fn delete_tree_under(root: HKEY, subpath: &str) -> bool {
    if is_reg_link(root, subpath) {
        return false;
    }
    if open_subkey(root, subpath).map(close).is_none() {
        return false; // absent
    }
    let w = wide(subpath);
    let status = unsafe { RegDeleteTreeW(root, PCWSTR(w.as_ptr())) };
    status == ERROR_SUCCESS
}

/// Open `root\subpath` with full access (for callers that then delete under it).
pub(super) fn open_subkey_rw(root: HKEY, subpath: &str) -> Option<HKEY> {
    let w = wide(subpath);
    let mut h = HKEY::default();
    let status = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), 0, KEY_ALL_ACCESS, &mut h) };
    (status == ERROR_SUCCESS).then_some(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::{
        RegCreateKeyExW, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    };

    /// A unique scratch key under HKCU that this test creates and removes.
    fn scratch() -> String {
        format!(r"Software\Kuvatin-regutil-test-{}", std::process::id())
    }

    fn create(path: &str) {
        let w = wide(path);
        let mut h = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(w.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut h,
                None,
            )
        };
        assert_eq!(status, ERROR_SUCCESS, "create {path}");
        close(h);
    }

    #[test]
    fn enumerates_and_deletes_subkeys() {
        let base = scratch();
        create(&format!(r"{base}\alpha"));
        create(&format!(r"{base}\beta"));
        let mut kids = enum_subkeys(HKEY_CURRENT_USER, &base);
        kids.sort();
        assert_eq!(kids, vec!["alpha".to_string(), "beta".to_string()]);
        assert!(delete_tree_under(HKEY_CURRENT_USER, &format!(r"{base}\alpha")));
        assert!(!delete_tree_under(HKEY_CURRENT_USER, &format!(r"{base}\alpha"))); // gone now
        assert!(!is_reg_link(HKEY_CURRENT_USER, &format!(r"{base}\beta")));
        // cleanup
        delete_tree_under(HKEY_CURRENT_USER, &base);
    }
}
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p kuvatin --release regutil`
Expected: PASS — `test shell::regutil::tests::enumerates_and_deletes_subkeys ... ok`.

- [ ] **Step 4: Commit**

```bash
git add crates/kuvatin/src/shell/regutil.rs crates/kuvatin/src/shell/mod.rs
git commit -m "Registry helpers work on any classes hive, not just HKCU

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 2: The shared verb key list (`verbs.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/verbs.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [ ] **Step 1: Declare the module**

In `crates/kuvatin/src/shell/mod.rs`, add:

```rust
#[cfg(windows)]
mod verbs;
```

- [ ] **Step 2: Write the failing test first**

Create `crates/kuvatin/src/shell/verbs.rs` containing ONLY the test module and empty stubs, so the test compiles and fails:

```rust
use windows::Win32::System::Registry::HKEY;

pub(super) fn classes_subkeys() -> Vec<String> {
    Vec::new()
}

pub(super) fn subkeys_to_delete(_classes_root: HKEY) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_list_covers_every_piece() {
        let keys = classes_subkeys();
        assert!(keys.contains(&r"SystemFileAssociations\.png\shell\Kuvatin".to_string()));
        assert!(keys.contains(&r"SystemFileAssociations\.exr\shell\Kuvatin".to_string()));
        assert!(keys.contains(&r"SystemFileAssociations\image\shell\Kuvatin".to_string()));
        assert!(keys.contains(&r"Directory\shell\Kuvatin".to_string()));
        assert!(keys.contains(&r"Directory\Background\shell\Kuvatin".to_string()));
        assert!(keys.contains(&"Kuvatin.CommandStore".to_string()));
        assert!(keys.contains(&"Kuvatin.CommandStore.Background".to_string()));
        assert!(keys.contains(&"Kuvatin.CommandStore.Frames".to_string()));
        // One root per menu extension, plus image + 2 dirs + 3 stores.
        assert_eq!(keys.len(), super::super::windows::menu_extensions().len() + 6);
    }
}
```

- [ ] **Step 3: Run it to see it fail**

Run: `cargo test -p kuvatin --release verbs::`
Expected: FAIL — `assertion failed` on the first `contains` (the stub returns an empty list).

- [ ] **Step 4: Write the real implementation**

Replace the two stubs in `crates/kuvatin/src/shell/verbs.rs` (keep the test module as-is) so the file reads:

```rust
//! The classic Explorer verb keys Kuvatin creates under a classes root
//! (`HKCU\Software\Classes` per user, or a mounted `HKEY_USERS\<SID>_Classes`).
//! This is the ONE list both the per-user `unregister()` and the all-users
//! uninstall delete, so they can never drift.

use windows::Win32::System::Registry::HKEY;

/// The three command stores (`ExtendedSubCommandsKey` targets).
const STORES: &[&str] = &[
    "Kuvatin.CommandStore",
    "Kuvatin.CommandStore.Background",
    "Kuvatin.CommandStore.Frames",
];

/// The verb subkeys, relative to a classes root, that today's build writes.
/// Every extension the menu attaches to, plus the pre-schema-4 perceived-type
/// root (`image`), the folder and folder-background verbs, and the stores.
pub(super) fn classes_subkeys() -> Vec<String> {
    let mut keys: Vec<String> = super::windows::menu_extensions()
        .iter()
        .map(|e| format!(r"SystemFileAssociations\.{e}\shell\Kuvatin"))
        .collect();
    keys.push(r"SystemFileAssociations\image\shell\Kuvatin".to_string());
    keys.push(r"Directory\shell\Kuvatin".to_string());
    keys.push(r"Directory\Background\shell\Kuvatin".to_string());
    keys.extend(STORES.iter().map(|s| s.to_string()));
    keys
}

/// The subkeys to delete under `classes_root`: the static [`classes_subkeys`]
/// set, PLUS any `SystemFileAssociations\<assoc>\shell\Kuvatin` found by
/// enumeration (so a verb from an older schema, or an extension later dropped
/// from the list, is still cleaned). Deterministic order, de-duplicated.
pub(super) fn subkeys_to_delete(classes_root: HKEY) -> Vec<String> {
    let mut keys = classes_subkeys();
    for child in super::regutil::enum_subkeys(classes_root, "SystemFileAssociations") {
        let candidate = format!(r"SystemFileAssociations\{child}\shell\Kuvatin");
        if super::regutil::open_subkey(classes_root, &candidate)
            .map(super::regutil::close)
            .is_some()
            && !keys.contains(&candidate)
        {
            keys.push(candidate);
        }
    }
    keys
}
```

- [ ] **Step 5: Run the test**

Run: `cargo test -p kuvatin --release verbs::`
Expected: PASS — `test shell::verbs::tests::static_list_covers_every_piece ... ok`.

- [ ] **Step 6: Commit**

```bash
git add crates/kuvatin/src/shell/verbs.rs crates/kuvatin/src/shell/mod.rs
git commit -m "One list names every context-menu key Kuvatin writes

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 3: Per-user `unregister()` uses the shared list

**Files:**
- Modify: `crates/kuvatin/src/shell/windows.rs:474-491`

- [ ] **Step 1: Replace the hardcoded deletion loops in `unregister()`**

In `crates/kuvatin/src/shell/windows.rs`, replace the head of `unregister()`:

```rust
pub fn unregister() -> Result<()> {
    for (root, _) in extension_roots() {
        delete_tree(&root);
    }
    for path in [LEGACY_ROOT, FOLDER_ROOT, BACKGROUND_ROOT] {
        delete_tree(path);
    }
    for store in [STORE_ITEM, STORE_BACKGROUND, STORE_FRAMES] {
        delete_tree(&format!(r"Software\Classes\{store}"));
    }
```

with:

```rust
pub fn unregister() -> Result<()> {
    // The one shared list (see `shell::verbs`), rooted at this user's HKCU classes.
    for sub in super::verbs::classes_subkeys() {
        delete_tree(&format!(r"Software\Classes\{sub}"));
    }
```

Leave the rest of the function (the `super::package::unregister()` match, the `println!`, `Ok(())`) untouched, and leave `extension_roots`, `LEGACY_ROOT`, `FOLDER_ROOT`, `BACKGROUND_ROOT`, `STORE_*` in place — `register_quiet()` and `write_store()` still use them. `classes_subkeys()` produces the identical set of paths.

- [ ] **Step 2: Verify nothing regressed**

Run: `cargo test -p kuvatin --release`
Expected: PASS, including the untouched `msix_build_script_lists_the_same_extensions` and `verb_roots_follow_the_canonical_extension_list`.

- [ ] **Step 3: Commit**

```bash
git add crates/kuvatin/src/shell/windows.rs
git commit -m "The per-user unregister deletes from the shared list

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 4: The profile enumeration and SID filter (`profiles.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/profiles.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [ ] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod profiles;
```

- [ ] **Step 2: Write the failing test first**

Create `crates/kuvatin/src/shell/profiles.rs` with a stub and its test:

```rust
pub(super) fn is_cleanup_sid(_sid: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_end_user_sids_are_cleaned() {
        assert!(is_cleanup_sid("S-1-5-21-1004336348-1177238915-682003330-1001"));
        assert!(is_cleanup_sid("S-1-12-1-111111111-2222222222-3333333333-4444444444"));
        assert!(!is_cleanup_sid("S-1-5-18")); // Local System
        assert!(!is_cleanup_sid("S-1-5-19")); // Local Service
        assert!(!is_cleanup_sid("S-1-5-20")); // Network Service
        assert!(!is_cleanup_sid("S-1-5-21-1-2-3-1001.bak")); // temp-profile marker
        assert!(!is_cleanup_sid(".DEFAULT"));
        assert!(!is_cleanup_sid("S-1-5-80-anything")); // service account
    }
}
```

- [ ] **Step 3: Run it to see it fail**

Run: `cargo test -p kuvatin --release profiles::`
Expected: FAIL — the first assertion fails (the stub always returns `false`).

- [ ] **Step 4: Write the real module**

Replace the whole of `crates/kuvatin/src/shell/profiles.rs` with (keep the test module and add the second test):

```rust
//! Enumerate the real user profiles from
//! `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList`, so the
//! all-users uninstall can visit each one's classes hive and files.

use std::path::PathBuf;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};

use super::regutil::{enum_subkeys, wide};

const PROFILE_LIST: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Profile {
    pub sid: String,
    pub dir: PathBuf,
}

/// A real, cleanable end-user account SID: a local/domain account
/// (`S-1-5-21-...`) or an Entra ID account (`S-1-12-1-...`). Service SIDs
/// (`S-1-5-18/19/20`), the `.bak` temp-profile markers and anything else are
/// rejected.
pub(super) fn is_cleanup_sid(sid: &str) -> bool {
    if sid.ends_with(".bak") {
        return false;
    }
    let known_prefix = sid.starts_with("S-1-5-21-") || sid.starts_with("S-1-12-1-");
    known_prefix && sid.chars().all(|c| c.is_ascii_digit() || c == '-' || c == 'S')
}

/// Read the `ProfileImagePath` of a ProfileList entry unexpanded, then expand
/// the environment strings ourselves. `None` when absent/unreadable.
fn profile_dir(sid: &str) -> Option<PathBuf> {
    let subkey = wide(&format!(r"{PROFILE_LIST}\{sid}"));
    let name = wide("ProfileImagePath");
    let mut buf = [0u16; 1024];
    let mut cb = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            // ProfileImagePath is REG_EXPAND_SZ; accept both types and do not
            // auto-expand (we expand with the machine environment ourselves).
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut cb),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let units = (cb as usize / 2).saturating_sub(1).min(buf.len());
    let raw = String::from_utf16_lossy(&buf[..units]);
    Some(PathBuf::from(expand_env(&raw)))
}

/// Expand `%SystemDrive%`-style tokens from the process environment, which for
/// the SYSTEM installer holds the machine variables these paths are written
/// against. An unknown token is left as it stands.
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            out.push('%');
            return out + after;
        };
        let var = &after[..end];
        match std::env::var(var) {
            Ok(v) => out.push_str(&v),
            Err(_) => {
                out.push('%');
                out.push_str(var);
                out.push('%');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Every cleanable profile whose directory exists.
pub(super) fn all() -> Vec<Profile> {
    enum_subkeys(HKEY_LOCAL_MACHINE, PROFILE_LIST)
        .into_iter()
        .filter(|sid| is_cleanup_sid(sid))
        .filter_map(|sid| {
            let dir = profile_dir(&sid)?;
            dir.is_dir().then_some(Profile { sid, dir })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_end_user_sids_are_cleaned() {
        assert!(is_cleanup_sid("S-1-5-21-1004336348-1177238915-682003330-1001"));
        assert!(is_cleanup_sid("S-1-12-1-111111111-2222222222-3333333333-4444444444"));
        assert!(!is_cleanup_sid("S-1-5-18")); // Local System
        assert!(!is_cleanup_sid("S-1-5-19")); // Local Service
        assert!(!is_cleanup_sid("S-1-5-20")); // Network Service
        assert!(!is_cleanup_sid("S-1-5-21-1-2-3-1001.bak")); // temp-profile marker
        assert!(!is_cleanup_sid(".DEFAULT"));
        assert!(!is_cleanup_sid("S-1-5-80-anything")); // service account
    }

    #[test]
    fn expands_the_profile_path_tokens() {
        std::env::set_var("KUVATIN_TEST_DRIVE", "C:");
        assert_eq!(
            expand_env(r"%KUVATIN_TEST_DRIVE%\Users\alice"),
            r"C:\Users\alice"
        );
        assert_eq!(expand_env(r"C:\Users\bob"), r"C:\Users\bob");
        assert_eq!(expand_env("%NO_SUCH_VAR_HERE%\\x"), "%NO_SUCH_VAR_HERE%\\x");
    }

    /// Every profile this returns must be a cleanable account with a real
    /// directory — a leaked service SID would mean cleaning the wrong hive.
    #[test]
    fn enumeration_returns_only_real_user_profiles() {
        for p in all() {
            assert!(is_cleanup_sid(&p.sid), "leaked non-user SID {}", p.sid);
            assert!(p.dir.is_dir(), "{} has no directory", p.sid);
        }
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p kuvatin --release profiles::`
Expected: PASS — three tests ok.

- [ ] **Step 6: Commit**

```bash
git add crates/kuvatin/src/shell/profiles.rs crates/kuvatin/src/shell/mod.rs
git commit -m "The uninstaller can enumerate every real user profile

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 5: The classes hive, loaded or signed out (`hive.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/hive.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

The runner cannot exercise the *signed-out* path with a live account (a logged-on account's hive stays mounted — the probe confirmed it stayed mounted even after its task ended), so the test here builds a throwaway `UsrClass.dat`-shaped hive file, seeds Kuvatin verbs plus a bystander, runs the production `clean_offline` against that FILE, and re-mounts to check. It needs SeBackup/SeRestore, so it returns early with `skipping: not elevated` when unelevated.

- [ ] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod hive;
```

- [ ] **Step 2: Write `hive.rs` (implementation + test together — the test cannot run before the code it calls exists)**

Create `crates/kuvatin/src/shell/hive.rs`:

```rust
//! Remove the classic Kuvatin verbs from one profile's classes hive. When the
//! profile is signed in, its hive is already mounted at
//! `HKEY_USERS\<SID>_Classes` and we edit it in place. Otherwise we load the
//! profile's `UsrClass.dat` under a private name with `RegLoadKeyW`, delete, and
//! unload — which needs the backup/restore privileges the SYSTEM installer has
//! but which are disabled by default.

use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LookupPrivilegeValueW, TokenElevation,
    LUID_AND_ATTRIBUTES, SE_BACKUP_NAME, SE_PRIVILEGE_ENABLED, SE_RESTORE_NAME,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Registry::{RegLoadKeyW, RegUnLoadKeyW, HKEY, HKEY_USERS};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::regutil::{close, delete_tree_under, open_subkey_rw, wide};
use super::verbs::subkeys_to_delete;

/// Delete every Kuvatin verb subkey under an already-open classes root.
/// Returns how many were deleted. Refuses `REG_LINK` keys (see `regutil`).
pub(super) fn delete_verbs(classes_root: HKEY) -> usize {
    subkeys_to_delete(classes_root)
        .into_iter()
        .filter(|sub| delete_tree_under(classes_root, sub))
        .count()
}

/// Clean the verbs of a profile whose classes hive is already mounted at
/// `HKEY_USERS\<SID>_Classes`. `None` when that hive cannot be opened.
pub(super) fn clean_loaded(sid: &str) -> Option<usize> {
    let root = open_subkey_rw(HKEY_USERS, &format!("{sid}_Classes"))?;
    let n = delete_verbs(root);
    close(root);
    Some(n)
}

/// Clean the verbs of a signed-out profile by loading its `UsrClass.dat`
/// (`<profile>\AppData\Local\Microsoft\Windows\UsrClass.dat`) under
/// `mount_name`, deleting, and unloading again.
pub(super) fn clean_offline(usrclass: &Path, mount_name: &str) -> anyhow::Result<usize> {
    enable_backup_restore()?;
    let file = wide(&usrclass.to_string_lossy());
    let name = wide(mount_name);
    let status = unsafe { RegLoadKeyW(HKEY_USERS, PCWSTR(name.as_ptr()), PCWSTR(file.as_ptr())) };
    if status != ERROR_SUCCESS {
        anyhow::bail!("RegLoadKeyW({}) failed: {status:?}", usrclass.display());
    }
    let deleted = match open_subkey_rw(HKEY_USERS, mount_name) {
        Some(root) => {
            let n = delete_verbs(root);
            close(root);
            n
        }
        None => 0,
    };
    // Unload with retries: a just-closed key can leave the hive briefly busy.
    let mut last = ERROR_SUCCESS;
    for _ in 0..10 {
        let status = unsafe { RegUnLoadKeyW(HKEY_USERS, PCWSTR(name.as_ptr())) };
        if status == ERROR_SUCCESS {
            return Ok(deleted);
        }
        last = status;
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    anyhow::bail!("RegUnLoadKeyW({mount_name}) failed after retries: {last:?}");
}

/// Enable `SeBackupPrivilege` and `SeRestorePrivilege` in this process token:
/// `RegLoadKeyW`/`RegUnLoadKeyW` require both, and they are present but
/// disabled in the SYSTEM installer's token.
fn enable_backup_restore() -> anyhow::Result<()> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )?;
    }
    for priv_name in [SE_BACKUP_NAME, SE_RESTORE_NAME] {
        let mut luid = LUID::default();
        unsafe { LookupPrivilegeValueW(PCWSTR::null(), priv_name, &mut luid)? };
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        unsafe { AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None)? };
    }
    close_handle(token);
    Ok(())
}

fn close_handle(h: HANDLE) {
    unsafe {
        let _ = CloseHandle(h);
    }
}

/// Whether this process runs with a full (elevated) token. The all-users
/// cleanup only works elevated; the orchestrator logs it, and the offline test
/// skips without it.
pub(super) fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut ret = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
    };
    close_handle(token);
    ok.is_ok() && elevation.TokenIsElevated != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, KEY_ALL_ACCESS, KEY_READ, KEY_WRITE,
        REG_OPTION_NON_VOLATILE,
    };

    fn create(root: HKEY, sub: &str) {
        let w = wide(sub);
        let mut h = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                root,
                PCWSTR(w.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut h,
                None,
            )
        };
        assert_eq!(status, ERROR_SUCCESS, "create {sub}");
        unsafe {
            let _ = RegCloseKey(h);
        }
    }

    fn exists(root: HKEY, sub: &str) -> bool {
        let w = wide(sub);
        let mut h = HKEY::default();
        let status = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), 0, KEY_READ, &mut h) };
        if status == ERROR_SUCCESS {
            unsafe {
                let _ = RegCloseKey(h);
            }
            return true;
        }
        false
    }

    fn mount(name: &str, file: &Path) -> u32 {
        let w = wide(name);
        let f = wide(&file.to_string_lossy());
        unsafe { RegLoadKeyW(HKEY_USERS, PCWSTR(w.as_ptr()), PCWSTR(f.as_ptr())).0 }
    }

    fn unmount(name: &str) {
        let w = wide(name);
        unsafe {
            let _ = RegUnLoadKeyW(HKEY_USERS, PCWSTR(w.as_ptr()));
        }
    }

    /// The signed-out path: build a hive FILE, seed a known-extension verb, an
    /// UNKNOWN-extension verb (which only enumeration can find) and a store,
    /// plus a bystander key that must survive; clean the file; re-mount and
    /// check. Needs SeBackup/SeRestore, so it runs only elevated — CI's runner
    /// is elevated and Task 14b fails the build if this ever skips there.
    #[test]
    fn offline_cleanup_removes_only_kuvatin_verbs() {
        if !is_elevated() {
            println!("skipping: not elevated");
            return;
        }
        enable_backup_restore().expect("enable SeBackup/SeRestore");

        let dir = tempfile::tempdir().unwrap();
        let hive = dir.path().join("UsrClass.dat");
        let seed = format!("kuvatin-test-seed-{}", std::process::id());
        let clean = format!("kuvatin-test-clean-{}", std::process::id());

        // 1) RegLoadKeyW creates the hive file when it does not exist.
        assert_eq!(mount(&seed, &hive), 0, "seeding RegLoadKeyW");
        let root = open_subkey_rw(HKEY_USERS, &seed).expect("open seeded hive");
        create(root, r"SystemFileAssociations\.png\shell\Kuvatin\command");
        create(root, r"SystemFileAssociations\.zzz\shell\Kuvatin\command");
        create(root, "Kuvatin.CommandStore\\shell\\item");
        create(root, r"SystemFileAssociations\.png\shell\OpenWithOther\command");
        close(root);
        unmount(&seed);

        // 2) The production offline cleanup, against the file.
        let deleted = clean_offline(&hive, &clean).expect("clean_offline");
        assert!(deleted >= 3, "expected at least three keys deleted, got {deleted}");

        // 3) Re-mount and inspect.
        assert_eq!(mount(&seed, &hive), 0, "verifying RegLoadKeyW");
        let root = open_subkey_rw(HKEY_USERS, &seed).expect("re-open hive");
        assert!(
            !exists(root, r"SystemFileAssociations\.png\shell\Kuvatin"),
            "known-extension verb left behind"
        );
        assert!(
            !exists(root, r"SystemFileAssociations\.zzz\shell\Kuvatin"),
            "unknown-extension verb left behind (enumeration missed it)"
        );
        assert!(!exists(root, "Kuvatin.CommandStore"), "store left behind");
        assert!(
            exists(root, r"SystemFileAssociations\.png\shell\OpenWithOther"),
            "a bystander key was wrongly deleted"
        );
        close(root);
        unmount(&seed);
    }
}
```

- [ ] **Step 3: Run the test**

Run (elevated shell): `cargo test -p kuvatin --release -- --exact shell::hive::tests::offline_cleanup_removes_only_kuvatin_verbs --nocapture`
Expected when elevated: PASS — `test shell::hive::tests::offline_cleanup_removes_only_kuvatin_verbs ... ok`, with no `skipping` line.
Expected when NOT elevated: prints `skipping: not elevated` and PASS. (That is what CI forbids; see Task 14b.)

- [ ] **Step 4: Run the whole suite**

Run: `cargo test -p kuvatin --release`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/kuvatin/src/shell/hive.rs crates/kuvatin/src/shell/mod.rs
git commit -m "A signed-out account's classes hive can be cleaned

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 6: The per-profile file plan (`paths.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/paths.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [ ] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod paths;
```

- [ ] **Step 2: Write the failing tests first**

Create `crates/kuvatin/src/shell/paths.rs` with stubs plus the tests:

```rust
use std::path::{Path, PathBuf};

pub(super) const PACKAGE_DATA_PREFIX: &str = "VilleMattila.Kuvatin_";

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct FilePlan {
    pub files: Vec<PathBuf>,
    pub trees: Vec<PathBuf>,
    pub prune_if_empty: Vec<PathBuf>,
}

pub(super) fn plan(_profile: &Path, _package_data_dirs: &[PathBuf]) -> FilePlan {
    FilePlan::default()
}

pub(super) fn is_protected(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_deletes_logs_and_temp_keeps_presets() {
        let profile = PathBuf::from(r"C:\Users\alice");
        let pkg = vec![PathBuf::from(
            r"C:\Users\alice\AppData\Local\Packages\VilleMattila.Kuvatin_5jce0xfqz5w2a",
        )];
        let p = plan(&profile, &pkg);

        assert!(p
            .files
            .contains(&PathBuf::from(r"C:\Users\alice\AppData\Local\Kuvatin\kuvatin.log")));
        assert!(p
            .files
            .contains(&PathBuf::from(r"C:\Users\alice\AppData\Local\Kuvatin\kuvatin.log.1")));
        assert!(p
            .files
            .contains(&PathBuf::from(r"C:\Users\alice\AppData\Local\Kuvatin\crash.log")));
        assert!(p
            .trees
            .contains(&PathBuf::from(r"C:\Users\alice\AppData\Local\Temp\kuvatin")));
        assert!(p.trees.contains(&pkg[0]));

        // Presets and settings are never touched.
        assert!(!p
            .files
            .contains(&PathBuf::from(r"C:\Users\alice\AppData\Roaming\Kuvatin\presets.toml")));
        assert!(!p
            .trees
            .contains(&PathBuf::from(r"C:\Users\alice\AppData\Roaming\Kuvatin")));
    }

    #[test]
    fn never_deletes_the_whole_local_kuvatin_folder() {
        let profile = PathBuf::from(r"C:\Users\alice");
        let p = plan(&profile, &[]);
        let local_kuvatin = PathBuf::from(r"C:\Users\alice\AppData\Local\Kuvatin");
        assert!(
            !p.trees.contains(&local_kuvatin),
            "Local\\Kuvatin must never be tree-deleted (it can hold signing keys)"
        );
        assert!(p.prune_if_empty.contains(&local_kuvatin));
    }

    #[test]
    fn protected_guard_rejects_data_folders() {
        assert!(is_protected(Path::new(r"C:\Users\alice\AppData\Roaming\Kuvatin")));
        assert!(is_protected(Path::new(r"C:\Users\alice\AppData\Local\Kuvatin")));
        assert!(!is_protected(Path::new(r"C:\Users\alice\AppData\Local\Temp\kuvatin")));
        assert!(!is_protected(Path::new(
            r"C:\Users\alice\AppData\Local\Packages\VilleMattila.Kuvatin_x"
        )));
    }

    #[test]
    fn package_folder_names_match_by_prefix() {
        assert!("VilleMattila.Kuvatin_5jce0xfqz5w2a".starts_with(PACKAGE_DATA_PREFIX));
        assert!(!"Microsoft.WindowsStore_8wekyb3d8bbwe".starts_with(PACKAGE_DATA_PREFIX));
    }
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test -p kuvatin --release paths::`
Expected: FAIL — `plan_deletes_logs_and_temp_keeps_presets` and `never_deletes_the_whole_local_kuvatin_folder` and `protected_guard_rejects_data_folders` all fail against the stubs.

- [ ] **Step 4: Write the real module**

Replace the stubs in `crates/kuvatin/src/shell/paths.rs` (keep the tests) so the file reads:

```rust
//! Which of a profile's Kuvatin files and folders the uninstall deletes, and
//! which it keeps. Presets and settings (`AppData\Roaming\Kuvatin`) stay; logs,
//! `%TEMP%\kuvatin` and the sparse package's data folder go. The whole
//! `AppData\Local\Kuvatin` folder is NEVER deleted outright — it can hold the
//! user's signing keys — only named files inside it, and then the folder itself
//! only when it ends up empty.

use std::path::{Path, PathBuf};

/// The family-name prefix of the sparse package's per-user data folder under
/// `AppData\Local\Packages`. The suffix is a Publisher hash Windows computes,
/// so we match by prefix.
pub(super) const PACKAGE_DATA_PREFIX: &str = "VilleMattila.Kuvatin_";

/// What to remove in one profile.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct FilePlan {
    /// Individual files to delete.
    pub files: Vec<PathBuf>,
    /// Directory trees to delete (junction-safe walk).
    pub trees: Vec<PathBuf>,
    /// Directories to remove ONLY if they are empty afterwards.
    pub prune_if_empty: Vec<PathBuf>,
}

/// Build the plan for a profile directory. Pure — it touches no disk — so the
/// caller passes in the package data folders it globbed.
pub(super) fn plan(profile: &Path, package_data_dirs: &[PathBuf]) -> FilePlan {
    let local = profile.join("AppData").join("Local");
    let kuvatin_local = local.join("Kuvatin");
    let temp_kuvatin = local.join("Temp").join("kuvatin");

    let mut plan = FilePlan {
        files: vec![
            kuvatin_local.join("kuvatin.log"),
            kuvatin_local.join("kuvatin.log.1"),
            kuvatin_local.join("crash.log"),
        ],
        // The whole %TEMP%\kuvatin tree: rendezvous and seq-cache both live there.
        trees: vec![temp_kuvatin],
        prune_if_empty: vec![kuvatin_local],
    };
    plan.trees.extend(package_data_dirs.iter().cloned());
    plan
}

/// The `AppData\Local\Packages\VilleMattila.Kuvatin_*` folders in one profile
/// (usually zero or one). Reads the disk; `[]` when the parent is absent.
pub(super) fn package_data_dirs(profile: &Path) -> Vec<PathBuf> {
    let packages = profile.join("AppData").join("Local").join("Packages");
    let Ok(rd) = std::fs::read_dir(&packages) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(PACKAGE_DATA_PREFIX)
        })
        .map(|e| e.path())
        .collect()
}

/// A guard used before any recursive delete: refuse a Roaming Kuvatin folder
/// (presets/settings) or the bare `AppData\Local\Kuvatin` folder.
pub(super) fn is_protected(path: &Path) -> bool {
    let ends_with = |p: &Path, parent: &str, leaf: &str| {
        let mut it = p.components().rev();
        it.next()
            .map(|c| c.as_os_str().eq_ignore_ascii_case(leaf))
            .unwrap_or(false)
            && it
                .next()
                .map(|c| c.as_os_str().eq_ignore_ascii_case(parent))
                .unwrap_or(false)
    };
    ends_with(path, "Roaming", "Kuvatin") || ends_with(path, "Local", "Kuvatin")
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p kuvatin --release paths::`
Expected: PASS — four tests ok.

- [ ] **Step 6: Commit**

```bash
git add crates/kuvatin/src/shell/paths.rs crates/kuvatin/src/shell/mod.rs
git commit -m "Each account's cleanup keeps presets and drops logs and cache

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 7: The junction-safe delete (`files.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/files.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [ ] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod files;
```

- [ ] **Step 2: Write the failing junction test first**

Create `crates/kuvatin/src/shell/files.rs` with stubs plus the tests:

```rust
use std::path::Path;

pub(super) fn remove_tree_no_follow(_path: &Path) {}

pub(super) fn remove_dir_if_empty(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn removes_a_real_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("kuvatin");
        fs::create_dir_all(root.join("seq-cache").join("abc")).unwrap();
        fs::write(root.join("seq-cache").join("abc").join(".complete"), "x").unwrap();
        fs::write(root.join("spool.paths"), "x").unwrap();
        remove_tree_no_follow(&root);
        assert!(!root.exists());
    }

    /// A junction inside the tree must be unlinked, and its TARGET's contents
    /// must survive — SYSTEM deleting inside a user-writable profile is exactly
    /// where a planted junction would redirect us.
    #[test]
    fn never_follows_a_junction() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let keeper = outside.join("keep.txt");
        fs::write(&keeper, "precious").unwrap();

        let root = dir.path().join("kuvatin");
        fs::create_dir_all(&root).unwrap();
        let link = root.join("seq-cache");
        let made = std::process::Command::new("cmd")
            .args([
                "/c",
                "mklink",
                "/J",
                &link.to_string_lossy(),
                &outside.to_string_lossy(),
            ])
            .status();
        if !matches!(made, Ok(s) if s.success()) {
            println!("skipping: could not create a directory junction");
            return;
        }

        remove_tree_no_follow(&root);
        assert!(!root.exists(), "the kuvatin tree should be gone");
        assert!(keeper.exists(), "the junction TARGET must survive");
        assert_eq!(fs::read_to_string(&keeper).unwrap(), "precious");
    }

    #[test]
    fn prunes_only_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("full");
        fs::create_dir_all(&full).unwrap();
        fs::write(full.join("keep.pfx"), "key").unwrap();
        remove_dir_if_empty(&full);
        assert!(full.exists(), "a non-empty folder must stay");

        let empty = dir.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        remove_dir_if_empty(&empty);
        assert!(!empty.exists(), "an empty folder should be pruned");
    }
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test -p kuvatin --release files::`
Expected: FAIL — `removes_a_real_tree` fails (`assert!(!root.exists())`) because the stub deletes nothing.

- [ ] **Step 4: Write the real module**

Replace the stubs in `crates/kuvatin/src/shell/files.rs` (keep the tests) so the file reads:

```rust
//! Delete a directory tree without ever following a reparse point (junction or
//! symlink). SYSTEM deleting inside a user-writable profile is a classic
//! redirection target, so at every level we check the reparse attribute and, for
//! a reparse point, remove the link entry itself without descending into
//! whatever it points at.

use std::os::windows::fs::MetadataExt;
use std::path::Path;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Delete `path` and everything under it, never following a reparse point.
/// Best-effort: one locked file must not stop the rest.
pub(super) fn remove_tree_no_follow(path: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return; // absent
    };
    let attrs = meta.file_attributes();
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        // A junction or symlink: remove the link, never its target's contents.
        if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
            let _ = std::fs::remove_dir(path);
        } else {
            let _ = std::fs::remove_file(path);
        }
        return;
    }
    if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Ok(rd) = std::fs::read_dir(path) {
        for entry in rd.flatten() {
            remove_tree_no_follow(&entry.path());
        }
    }
    let _ = std::fs::remove_dir(path);
}

/// Remove a directory only when it is empty — used to prune a `Local\Kuvatin`
/// folder after its log files are gone, without touching anything else the user
/// left there.
pub(super) fn remove_dir_if_empty(path: &Path) {
    if std::fs::read_dir(path)
        .map(|mut r| r.next().is_none())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_dir(path);
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p kuvatin --release files::`
Expected: PASS — three tests ok (`never_follows_a_junction` prints a skip line only if the machine forbids `mklink /J`; `windows-latest` allows it).

- [ ] **Step 6: Commit**

```bash
git add crates/kuvatin/src/shell/files.rs crates/kuvatin/src/shell/mod.rs
git commit -m "Deleting an account's files never follows a junction

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 8: Remove the sparse package for all users (`package.rs`)

**Files:**
- Modify: `crates/kuvatin/src/shell/package.rs`

- [ ] **Step 1: Extend the imports**

In `crates/kuvatin/src/shell/package.rs`, extend the WinRT imports (currently `package.rs:17-20`) so all the names below resolve — the file already has `use anyhow::{bail, Context, Result};` and `use windows::core::HSTRING;`:

```rust
use windows::Foundation::{AsyncOperationWithProgressCompletedHandler, IAsyncOperationWithProgress};
use windows::Management::Deployment::{
    AddPackageOptions, DeploymentProgress, DeploymentResult, PackageManager, RemovalOptions,
};
```

- [ ] **Step 2: Add the all-users removal after `unregister()` (`package.rs:141-152`)**

```rust
/// How long to wait for one package removal before giving up on it and moving
/// on: the uninstall must not hang inside msiexec.
const REMOVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Await a WinRT deployment operation with a timeout. WinRT fires the handler
/// immediately when the operation has already finished, so there is no race.
fn await_bounded(
    op: &IAsyncOperationWithProgress<DeploymentResult, DeploymentProgress>,
    dur: std::time::Duration,
) -> Result<DeploymentResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    op.SetCompleted(&AsyncOperationWithProgressCompletedHandler::new(
        move |_, _| {
            let _ = tx.send(());
            Ok(())
        },
    ))?;
    match rx.recv_timeout(dur) {
        Ok(()) => Ok(op.GetResults()?),
        Err(_) => bail!("timed out after {dur:?}"),
    }
}

/// Remove the sparse package for EVERY user on the machine. Prints to stdout —
/// the SYSTEM uninstall path must not write app logs. Best-effort: each failure
/// is reported and the rest continue. Returns how many registrations went.
pub fn unregister_all_users() -> usize {
    if !os_supports_package() {
        println!("Windows 11 menu: not supported on this build; no package to remove.");
        return 0;
    }
    init_com();
    let pm = match PackageManager::new() {
        Ok(pm) => pm,
        Err(e) => {
            println!("Windows 11 menu: PackageManager unavailable ({e:#}); package left as it is.");
            return 0;
        }
    };

    // Every registration of our package, across all users (needs admin).
    let mut fulls: Vec<String> = Vec::new();
    let mut family: Option<String> = None;
    match pm.FindPackages() {
        Ok(pkgs) => {
            for p in pkgs {
                let Ok(id) = p.Id() else { continue };
                if id.Name().map(|n| n != PACKAGE_NAME).unwrap_or(true) {
                    continue;
                }
                if family.is_none() {
                    family = id.FamilyName().ok().map(|h| h.to_string());
                }
                if let Ok(full) = id.FullName() {
                    let full = full.to_string();
                    if !fulls.contains(&full) {
                        fulls.push(full);
                    }
                }
            }
        }
        Err(e) => println!("Windows 11 menu: could not enumerate packages ({e:#})."),
    }
    println!("Windows 11 menu: {} registration(s) found.", fulls.len());

    // Deprovision only when the package really is provisioned machine-wide (it
    // never is today, but a future install-side change might do it).
    if let Some(fam) = &family {
        let provisioned = pm
            .FindProvisionedPackages()
            .map(|v| {
                v.into_iter().any(|p| {
                    p.Id()
                        .and_then(|i| i.FamilyName())
                        .map(|h| h.to_string() == *fam)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if provisioned {
            match pm.DeprovisionPackageForAllUsersAsync(&HSTRING::from(fam.as_str())) {
                Ok(op) => match await_bounded(&op, REMOVE_TIMEOUT) {
                    Ok(_) => println!("Windows 11 menu: deprovisioned {fam}."),
                    Err(e) => println!("Windows 11 menu: deprovisioning {fam} failed ({e:#})."),
                },
                Err(e) => println!("Windows 11 menu: deprovision call failed ({e:#})."),
            }
        }
    }

    let mut removed = 0usize;
    for full in &fulls {
        let op = pm.RemovePackageWithOptionsAsync(
            &HSTRING::from(full.as_str()),
            RemovalOptions::RemoveForAllUsers,
        );
        match op {
            Ok(op) => match await_bounded(&op, REMOVE_TIMEOUT) {
                Ok(result) => match result.ExtendedErrorCode() {
                    Ok(code) if code.is_ok() => {
                        println!("Windows 11 menu: removed {full} for all users.");
                        removed += 1;
                    }
                    Ok(code) => println!(
                        "Windows 11 menu: removing {full} reported 0x{:08X}.",
                        code.0 as u32
                    ),
                    Err(e) => println!("Windows 11 menu: removing {full}: {e:#}"),
                },
                Err(e) => println!("Windows 11 menu: removing {full}: {e:#}"),
            },
            Err(e) => println!("Windows 11 menu: RemovePackage call failed for {full}: {e:#}"),
        }
    }

    // Anything still registered (e.g. an account left in a Staged state).
    if let Ok(pkgs) = pm.FindPackages() {
        for p in pkgs {
            let Ok(id) = p.Id() else { continue };
            if id.Name().map(|n| n == PACKAGE_NAME).unwrap_or(false) {
                let full = id.FullName().map(|h| h.to_string()).unwrap_or_default();
                if let Ok(users) = pm.FindUsers(&HSTRING::from(full.as_str())) {
                    for u in users {
                        let sid = u.UserSecurityId().map(|h| h.to_string()).unwrap_or_default();
                        let state = u.InstallState().map(|s| s.0).unwrap_or(-1);
                        println!(
                            "Windows 11 menu: {full} still registered for {sid} (state {state})."
                        );
                    }
                }
            }
        }
    }

    removed
}
```

- [ ] **Step 3: Compile and run the module's existing tests**

Run: `cargo test -p kuvatin --release package::`
Expected: PASS — `file_uris_escape_spaces_and_use_forward_slashes`, `same_dir_ignores_case_and_trailing_separators`, `windows_build_is_a_real_number_here`. There is no unit test for `unregister_all_users`: it needs a real registered package, and it is proven end to end by Task 14c.

- [ ] **Step 4: Commit**

```bash
git add crates/kuvatin/src/shell/package.rs
git commit -m "The Windows 11 menu package is removed for every account

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 9: The orchestrator (`allusers.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/allusers.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [ ] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod allusers;
```

- [ ] **Step 2: Write `allusers.rs`**

Create `crates/kuvatin/src/shell/allusers.rs`:

```rust
//! The `--unregister-all-users` entry point: clean the classic verbs, the
//! sparse package and the per-user files for EVERY account, from the SYSTEM
//! deferred custom action that uninstall runs before RemoveFiles. Prints to
//! stdout only — never the app log, which as SYSTEM would seed a fresh folder
//! in the system profile.

use windows::Win32::System::Registry::HKEY_USERS;

use super::{files, hive, package, paths, profiles, regutil};

/// Remove Kuvatin's per-user footprint from every profile on the machine.
/// Never fails hard: msiexec ignores the exit code, and one stuck profile must
/// not block the others.
pub fn unregister_all_users() {
    println!("Kuvatin: removing the context menu for every account.");
    println!("Kuvatin: elevated: {}.", hive::is_elevated());

    // 1) The sparse package, all users, first: its removal shuts down the
    //    dllhost surrogate holding kuvatin_shellext.dll before RemoveFiles
    //    deletes the DLL.
    let removed = package::unregister_all_users();
    println!("Kuvatin: package removed for {removed} registration(s).");

    // 2) The classic verbs and the files, profile by profile.
    let profiles = profiles::all();
    println!("Kuvatin: {} profile(s) to clean.", profiles.len());
    for profile in profiles {
        let mounted = regutil::open_subkey(HKEY_USERS, &format!("{}_Classes", profile.sid));
        let verbs = if let Some(h) = mounted {
            regutil::close(h);
            hive::clean_loaded(&profile.sid)
        } else {
            let usrclass = profile
                .dir
                .join("AppData")
                .join("Local")
                .join("Microsoft")
                .join("Windows")
                .join("UsrClass.dat");
            if usrclass.is_file() {
                match hive::clean_offline(&usrclass, &format!("Kuvatin_Cleanup_{}", profile.sid)) {
                    Ok(n) => Some(n),
                    Err(e) => {
                        println!("Kuvatin: {}: hive cleanup failed ({e:#}).", profile.sid);
                        None
                    }
                }
            } else {
                println!(
                    "Kuvatin: {}: no classes hive at {}.",
                    profile.sid,
                    usrclass.display()
                );
                None
            }
        };
        match verbs {
            Some(n) => println!("Kuvatin: {}: removed {n} verb key(s).", profile.sid),
            None => println!("Kuvatin: {}: no verb keys removed.", profile.sid),
        }

        // 3) The files, guarded so a protected data folder is never tree-deleted.
        let pkg_dirs = paths::package_data_dirs(&profile.dir);
        let plan = paths::plan(&profile.dir, &pkg_dirs);
        for f in &plan.files {
            let _ = std::fs::remove_file(f);
        }
        for tree in &plan.trees {
            if paths::is_protected(tree) {
                println!("Kuvatin: refusing to delete protected {}.", tree.display());
                continue;
            }
            files::remove_tree_no_follow(tree);
        }
        for dir in &plan.prune_if_empty {
            files::remove_dir_if_empty(dir);
        }
        println!(
            "Kuvatin: {}: files cleaned ({} package folder(s)).",
            profile.sid,
            pkg_dirs.len()
        );
    }

    println!("Kuvatin: all-users cleanup done.");
}
```

- [ ] **Step 3: Compile**

Run: `cargo build -p kuvatin --release`
Expected: builds. The orchestrator has no unit test — it is proven end to end by Task 14c.

- [ ] **Step 4: Commit**

```bash
git add crates/kuvatin/src/shell/allusers.rs crates/kuvatin/src/shell/mod.rs
git commit -m "One pass cleans the menu, the package and the files for every account

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 10: Export the entry point; non-windows stub

**Files:**
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [ ] **Step 1: Export the orchestrator**

In `crates/kuvatin/src/shell/mod.rs`, after the existing windows re-export (`mod.rs:6-10`), add:

```rust
#[cfg(windows)]
pub use allusers::unregister_all_users;
```

No test hooks are exported: every test in this plan lives inside its own module.

- [ ] **Step 2: Add the non-windows stub**

Below the existing non-windows `unregister()` stub (`mod.rs:22-25`), add:

```rust
/// Remove the context menu for every account; no-op off Windows.
#[cfg(not(windows))]
pub fn unregister_all_users() {}
```

- [ ] **Step 3: Build and lint**

Run: `cargo clippy -p kuvatin --all-targets -- -D warnings`
Expected: clean. (This is the gate CI runs at `release.yml:206-207`, so fix any dead-code or style warnings here rather than in CI.)

- [ ] **Step 4: Commit**

```bash
git add crates/kuvatin/src/shell/mod.rs
git commit -m "The all-users cleanup is reachable from the shell module

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 11: The CLI flag and mode

**Files:**
- Modify: `crates/kuvatin/src/cli.rs`

- [ ] **Step 1: Write the failing test first**

In `crates/kuvatin/src/cli.rs`, add to the test module (next to `register_flag`, `cli.rs:156-162`):

```rust
#[test]
fn unregister_all_users_flag() {
    assert_eq!(
        mode_of(&["--unregister-all-users", "--quiet"]),
        Mode::UnregisterAllUsers
    );
    // Mutually exclusive with every other headless mode.
    assert!(parse_err(&["--unregister-all-users", "--register"]));
    assert!(parse_err(&["--unregister-all-users", "--unregister"]));
    assert!(parse_err(&["--unregister-all-users", "--preset", "X"]));
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test -p kuvatin --release unregister_all_users_flag`
Expected: FAIL to compile — `no variant named UnregisterAllUsers found for enum Mode`.

- [ ] **Step 3: Add the flag, the variant and the dispatch**

In the `Cli` struct (`cli.rs:11-50`), after the `unregister` field (`cli.rs:34-36`):

```rust
    /// Remove the Explorer context-menu entries for EVERY account and exit
    /// (the installer runs this as SYSTEM during uninstall).
    #[arg(
        long,
        conflicts_with_all = ["register", "unregister", "preset", "sequence_mp4", "print_extensions"]
    )]
    pub unregister_all_users: bool,
```

In the `Mode` enum (`cli.rs:52-70`), after `Unregister,`:

```rust
    UnregisterAllUsers,
```

In `into_mode()` (`cli.rs:72-101`), insert a branch so the head reads:

```rust
        if self.register {
            Mode::Register
        } else if self.unregister {
            Mode::Unregister
        } else if self.unregister_all_users {
            Mode::UnregisterAllUsers
        } else if self.print_extensions {
            Mode::PrintExtensions
        } else if self.sequence_mp4 {
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p kuvatin --release unregister_all_users_flag`
Expected: PASS — `test cli::tests::unregister_all_users_flag ... ok`.

- [ ] **Step 5: Commit**

```bash
git add crates/kuvatin/src/cli.rs
git commit -m "A switch asks for the all-users cleanup

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 12: Dispatch the new mode in `main.rs`, before anything touches a profile

**Files:**
- Modify: `crates/kuvatin/src/main.rs:142-157`

**What runs before the dispatch today, and why it matters.** As SYSTEM, `%LOCALAPPDATA%` resolves into `C:\Windows\System32\config\systemprofile`, so anything that writes there would be a brand-new leftover — the very thing this feature removes. Checked line by line:
- `shell::attach_parent_console()` (`main.rs:145` → `windows.rs:427-431`) — `AttachConsole` only. Safe.
- `applog::install_panic_hook()` (`main.rs:146` → `applog.rs:82-126`) — installing the hook writes nothing, but the hook it installs calls `crash_path()` (`applog.rs:33-35` → `dir()` at `applog.rs:19-27`, which does `create_dir_all`) and `log()` **if the process later panics**. In the SYSTEM pass that would create `…\systemprofile\AppData\Local\Kuvatin\crash.log`. So the new mode is dispatched **before** the hook is installed.
- `configure_bundled_gstreamer()` (`main.rs:147` → `main.rs:24-42`) — reads `current_exe`, sets two process env vars. Safe, but unnecessary here.
- `configure_batch_memory()` (`main.rs:148` → `main.rs:117-120`) — `GlobalMemoryStatusEx` into a static. Safe, but unnecessary here.
- The sequence-cache sweep is **not** before the dispatch: `kuvatin_video::sweep_sequence_cache` is called only inside the `Mode::SequenceMp4` arm (`main.rs:227-230`) and from `gui::run` (`gui/mod.rs:101-106`).
- Settings are **not** loaded before the dispatch: `Settings::load()` has one caller, `gui/updates.rs:16`.
- No registry work happens before the dispatch: `ensure_registered()` is called only from `gui::run` (`gui/mod.rs:97`).

- [ ] **Step 1: Restructure `main()` so the new mode returns before the hook and the engine setup**

Replace `main()`'s head (`main.rs:142-157`), which currently reads:

```rust
fn main() {
    // A windowed exe run from a terminal joins that terminal's console, so
    // `--register` / errors print where the user is looking.
    shell::attach_parent_console();
    applog::install_panic_hook();
    configure_bundled_gstreamer();
    configure_batch_memory();
    let args: Vec<std::ffi::OsString> = std::env::args_os()
        .map(|a| cli::repair_drive_root(&a))
        .collect();
    let cli = Cli::parse_from(args);
    let quiet = cli.quiet;
    if quiet {
        shell::set_quiet(true);
    }
    match cli.into_mode() {
```

with:

```rust
fn main() {
    // A windowed exe run from a terminal joins that terminal's console, so
    // `--register` / errors print where the user is looking.
    shell::attach_parent_console();
    let args: Vec<std::ffi::OsString> = std::env::args_os()
        .map(|a| cli::repair_drive_root(&a))
        .collect();
    let cli = Cli::parse_from(args);
    let quiet = cli.quiet;
    if quiet {
        shell::set_quiet(true);
    }
    let mode = cli.into_mode();
    // The uninstaller's SYSTEM pass is dispatched FIRST, before the panic hook
    // and the engine setup: as SYSTEM, applog's crash log would land in
    // C:\Windows\System32\config\systemprofile — a new leftover, which is
    // exactly what this mode exists to remove. It reports on stdout, which the
    // installer's WixQuietExec64 action captures into the MSI log.
    if matches!(mode, Mode::UnregisterAllUsers) {
        println!("kuvatin --unregister-all-users starting");
        shell::unregister_all_users();
        println!("kuvatin --unregister-all-users done");
        return;
    }
    applog::install_panic_hook();
    configure_bundled_gstreamer();
    configure_batch_memory();
    match mode {
```

- [ ] **Step 2: Make the `match` exhaustive**

In the same `match` (now over `mode`), add an arm next to `Mode::Unregister` (`main.rs:166-173`):

```rust
        // Handled above, before the panic hook and the engine setup.
        Mode::UnregisterAllUsers => unreachable!("--unregister-all-users is dispatched earlier"),
```

**Trade-off to note in review:** the panic hook now starts one step later, so a panic inside `repair_drive_root`/`Cli::parse_from` would no longer reach `crash.log`. Clap exits (it does not panic) on bad arguments, so the exposure is a few lines of argument munging.

- [ ] **Step 3: Build and run the suite**

Run: `cargo test -p kuvatin --release`
Expected: PASS.

Run: `cargo clippy -p kuvatin --all-targets -- -D warnings`
Expected: clean.

Do **not** run `target\release\kuvatin.exe --unregister-all-users` on your own machine: it removes the context menu, the package and the logs for every account on it. CI proves the behaviour (Task 14c).

- [ ] **Step 4: Commit**

```bash
git add crates/kuvatin/src/main.rs
git commit -m "The all-users switch runs without touching the system profile

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 13: WiX — the SYSTEM custom action, sequencing and conditions

**Files:**
- Modify: `crates/kuvatin/wix/main.wxs`

**Context:** `WixUtilExtension`/`WixCA` are already linked (the file uses `<util:CloseApplication>` at `main.wxs:111-116`), so `WixQuietExec64` from the `WixCA` binary is available with no new `-ext`. For a **deferred** `WixQuietExec64`, the command line is passed in a property whose Id equals the custom action's Id, set with `SetProperty` immediately before it. `[#exe0]` resolves to the installed path of the `File` with Id `exe0` (`main.wxs:145-152`).

- [ ] **Step 1: Add the custom action and its command-line property**

In `crates/kuvatin/wix/main.wxs`, right after the `KuvatinUnregister` custom action (`main.wxs:276-281`) and before the `<?ifdef SignCerPath?>` certificate block (`main.wxs:283`):

```xml
        <!--
          Clean the context menu, the sparse package and the per-user files for
          EVERY account, as SYSTEM (Impersonate='no'), during uninstall. The
          impersonated KuvatinUnregister above still cleans the uninstalling
          user; this adds every other account. WixQuietExec64 (from the already
          linked WixUtilExtension) runs the command line held in the property
          whose Id matches this action's Id, and copies the command's stdout
          into the MSI log. Return='ignore' so one stuck profile cannot abort
          the uninstall.
        -->
        <SetProperty Id='KuvatinUnregisterAllUsers'
                     Before='KuvatinUnregisterAllUsers'
                     Sequence='execute'
                     Value='&quot;[#exe0]&quot; --unregister-all-users --quiet' />
        <CustomAction Id='KuvatinUnregisterAllUsers'
                      BinaryKey='WixCA'
                      DllEntry='WixQuietExec64'
                      Execute='deferred'
                      Impersonate='no'
                      Return='ignore' />
```

- [ ] **Step 2: Sequence it in both branches of `InstallExecuteSequence`**

In `<InstallExecuteSequence>` (`main.wxs:305-315`):

```xml
<?ifdef SignCerPath?>
            <Custom Action='KuvatinTrustCert' After='InstallFiles'>NOT Installed</Custom>
            <Custom Action='KuvatinRegister' After='KuvatinTrustCert'>NOT Installed</Custom>
            <Custom Action='KuvatinUnregister' Before='RemoveFiles'>Installed AND (REMOVE="ALL")</Custom>
            <Custom Action='KuvatinUnregisterAllUsers' After='KuvatinUnregister'>Installed AND (REMOVE="ALL") AND NOT UPGRADINGPRODUCTCODE</Custom>
            <Custom Action='KuvatinUntrustCert' After='KuvatinUnregisterAllUsers'>Installed AND (REMOVE="ALL")</Custom>
<?else?>
            <Custom Action='KuvatinRegister' After='InstallFiles'>NOT Installed</Custom>
            <Custom Action='KuvatinUnregister' Before='RemoveFiles'>Installed AND (REMOVE="ALL")</Custom>
            <Custom Action='KuvatinUnregisterAllUsers' After='KuvatinUnregister'>Installed AND (REMOVE="ALL") AND NOT UPGRADINGPRODUCTCODE</Custom>
<?endif?>
```

**Why `NOT UPGRADINGPRODUCTCODE`:** a major upgrade removes the old product with `REMOVE="ALL"` too (`MajorUpgrade`, `main.wxs:87-90`). Without the guard every upgrade would strip other users' menus, and a right-click-only user would lose the menu until they next opened the GUI. A real uninstall leaves `UPGRADINGPRODUCTCODE` unset, so the action runs then. `KuvatinUntrustCert` now runs after the package is gone — the package must be removed before its certificate is untrusted.

- [ ] **Step 3: Know how to read the action's output when CI fails**

`WixQuietExec64` writes the command line and every line the process prints into the **verbose** MSI log, so the run must use `/l*v` (Task 14c does). To diagnose, search that log for `KuvatinUnregisterAllUsers` (the action and its property), `WixQuietExec64` (the wrapper's own lines) and `Kuvatin:` (the orchestrator's own output, e.g. `Kuvatin: 3 profile(s) to clean.`). A missing `Kuvatin:` block means the action never ran — check the condition and the sequencing; a present block that ends early names the profile it stopped on.

- [ ] **Step 4: Verify the MSI still builds**

The MSI is built only under `PACKAGE`/tag in CI, which Task 14 exercises. If building locally, follow `crates/kuvatin/wix/README.md`. A `light` error naming `WixQuietExec64` or an unresolved `Binary:WixCA` would mean the util extension is not linked — it is, so expect a clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/kuvatin/wix/main.wxs
git commit -m "Uninstall cleans every account, not just the uninstalling one

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 14: CI — throwaway signing key, the offline test by name, the every-account uninstall test

**Files:**
- Modify: `.github/workflows/release.yml`

Three edits, committed together at the end.

**Do not touch** the `Test (video engine, self-contained — gates the release)` filter line (`release.yml:219-229`). It already carries the branch's crash-test names and may gain more before this task runs; this plan has no business editing it.

### 14a. Sign non-tag packaging runs with a throwaway key

**Read first:** the Build MSI step (`release.yml:342-432`). Today it signs only when `SIGN_PFX_BASE64` is set (tag runs, from the `release-signing` environment); on a tag with no key it throws; otherwise (manual dispatch, installer pull requests) it builds the msix **unsigned** and sets `SIGNED=false`. An unsigned package cannot register (0x80073D2B, `package.rs:7-13`), so the package half of this fix would go unproven until a tag.

**Publisher must match exactly.** `build-msix.ps1` takes `-Publisher` (`crates/kuvatin/msix/build-msix.ps1:33`, default `'CN=Ville Mattila'`) and substitutes it into `@PUBLISHER@` in the manifest (`build-msix.ps1:56`, manifest `crates/kuvatin/msix/AppxManifest.xml:28-31`), and Windows requires the manifest Publisher to equal the signing certificate's Subject. The real-key branch passes `-Publisher $cert.Subject` (`release.yml:376`); the throwaway branch below passes `-Publisher $throwaway.Subject` for exactly the same reason, with the subject `CN=Ville Mattila` so both branches produce the same Publisher string — and therefore the same package family name, `VilleMattila.Kuvatin_5jce0xfqz5w2a`, that Task 14c's assertions glob for.

- [ ] **Step 1: Replace the unsigned `else` branch**

The current final `else` (`release.yml:394-406`) reads:

```powershell
          } else {
            # A tag publishes, and an unsigned package there is a release whose
            # Windows 11 entry never registers. ...
            if ($env:GITHUB_REF -like 'refs/tags/*') {
              throw 'No signing key on a tag run: ...'
            }
            & crates\kuvatin\msix\build-msix.ps1 -Version $ver -Out $msix
            Write-Host '::warning::No signing key ...'
            'SIGNED=false' | Out-File $env:GITHUB_ENV -Append
          }
```

Replace it with:

```powershell
          } else {
            # A tag MUST use the real key from the release-signing environment.
            if ($env:GITHUB_REF -like 'refs/tags/*') {
              throw 'No signing key on a tag run: set KUVATIN_SIGN_PFX_BASE64 and KUVATIN_SIGN_PFX_PASSWORD in the release-signing environment (README, "Signing the menu package").'
            }
            # Non-tag packaging runs (installer pull requests, manual dispatch)
            # sign with a THROWAWAY self-signed key, made fresh here and
            # discarded before the step ends, so CI can prove the signed package
            # registers and that uninstall removes it for every account. The
            # subject must equal the manifest Publisher exactly.
            $throwaway = New-SelfSignedCertificate -Type Custom -Subject 'CN=Ville Mattila' `
              -KeyUsage DigitalSignature -FriendlyName 'Kuvatin CI throwaway' `
              -CertStoreLocation 'Cert:\CurrentUser\My' `
              -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
            $pfx = "$env:RUNNER_TEMP\kuvatin-ci-sign.pfx"
            $pw = [Guid]::NewGuid().ToString('N')
            Write-Host "::add-mask::$pw"
            $store = "Cert:\CurrentUser\My\$($throwaway.Thumbprint)"
            Export-PfxCertificate -Cert $store -FilePath $pfx -Password (ConvertTo-SecureString $pw -AsPlainText -Force) | Out-Null
            try {
              & crates\kuvatin\msix\build-msix.ps1 -Version $ver -Out $msix -Publisher $throwaway.Subject
              $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" | Sort-Object { [version]($_.Directory.Parent.Name) } | Select-Object -Last 1
              & $signtool.FullName sign /fd SHA256 /f $pfx /p $pw $msix
              if ($LASTEXITCODE -ne 0) { throw "signtool (throwaway key) exited $LASTEXITCODE" }
              $cer = "$env:GITHUB_WORKSPACE\target\msix\kuvatin-signing.cer"
              Export-Certificate -Cert $store -FilePath $cer | Out-Null
              $thumb = $throwaway.Thumbprint
              Write-Host "package signed with a THROWAWAY key ($($throwaway.Subject) $thumb)"
              'SIGNED=true' | Out-File $env:GITHUB_ENV -Append
            } finally {
              # Never leave the private key behind, not even when signing threw.
              Remove-Item $pfx -Force -ErrorAction SilentlyContinue
              Remove-Item $store -Force -ErrorAction SilentlyContinue
            }
          }
```

The real-key branch (`release.yml:372-393`) is untouched, and the existing `if ($cer)` block (`release.yml:411-417`) now passes `SignCerPath`/`SignCerThumbprint` on these runs too, so the installer trusts the throwaway certificate and `KuvatinUntrustCert` removes it by thumbprint (`main.wxs:297-302`). Keep `$cer` on disk — the WiX build and the install test need it; only the `.pfx` and the store entry go.

**Consequence to expect:** with `SIGNED=true` on pull-request and dispatch runs, the Install test's package assertions (`release.yml:469-482`) now run there as well. `windows-latest` is Windows Server 2025, build 26100, comfortably over the 22000 the check requires, so the sparse package really does register on those runs.

- [ ] **Step 2: Update the step's own comments**

The step header comment (`release.yml:365-370`) and the workflow's pull-request note (`release.yml:88-93`) both say a pull-request package is unsigned. Reword both to say non-tag packaging runs sign with a throwaway key made and destroyed inside the run, while tag runs use the release key.

### 14b. Run the offline-hive test by name and fail if it skips

**Read first:** the deterministic gate `Test (core + gui — gates the release)` (`release.yml:212-213`) already compiles and runs every in-crate test, so `shell::hive::tests::offline_cleanup_removes_only_kuvatin_verbs` runs there too. That gate catches a *failure*, but not a silent *skip*. Add a second, explicit step right after it.

- [ ] **Step 3: Add the step**

```yaml
      # The all-users uninstall's signed-out hive path cannot be exercised by a
      # signed-in account (its hive stays mounted), so it is proven by a test
      # that builds a throwaway UsrClass.dat and cleans it. That test SKIPS when
      # the process is not elevated; the hosted runner IS elevated, so it must
      # really run here — a skip means the path went unproven.
      - name: Test (offline hive cleanup — gates the release)
        shell: pwsh
        run: |
          $out = cargo test -p kuvatin --release -- --exact shell::hive::tests::offline_cleanup_removes_only_kuvatin_verbs --nocapture 2>&1 | Out-String
          Write-Host $out
          if ($LASTEXITCODE -ne 0) { throw 'the offline hive test failed' }
          if ($out -match 'skipping: not elevated') {
            throw 'the offline hive test skipped (runner not elevated?) — the signed-out path went unproven'
          }
          if ($out -notmatch 'offline_cleanup_removes_only_kuvatin_verbs \.\.\. ok') {
            throw 'the offline hive test did not report a pass'
          }
```

### 14c. Replace the single-user uninstall check with an every-account test

**Read first:** the Install test step (`release.yml:437-507`). It sets no `$ErrorActionPreference`, so it runs at the default `Continue` and native exit codes never abort it; it checks `Start-Process` results explicitly via `$p.ExitCode`. The new step below sets `$ErrorActionPreference = 'Stop'` for cmdlets **and** `$PSNativeCommandUseErrorActionPreference = $false` so that `reg query` on a missing key (exit 1) and `secedit` cannot abort it, whatever the pwsh default is. The probe worked without `Stop`; this keeps working with it.

- [ ] **Step 4: Delete the old uninstall tail**

Remove these lines from the end of the Install test step (`release.yml:501-507`):

```powershell
          $p = Start-Process msiexec -ArgumentList "/x","`"$msi`"","/qn","/norestart" -Wait -PassThru
          if ($p.ExitCode -ne 0) { throw "msiexec /x exited $($p.ExitCode)" }
          if (Test-Path $exe) { throw "uninstall left $exe behind" }
          if (Test-Path $verb) { throw "uninstall left the Explorer verb behind" }
          if (Test-Path $startMenu) { throw "uninstall left the Start menu folder behind" }
          if ($build -ge 22000 -and (Get-AppxPackage VilleMattila.Kuvatin)) { throw "uninstall left the sparse package registered" }
          if ($env:SIGNED -eq 'true' -and (Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object { $_.Subject -like 'CN=Ville Mattila*' })) { throw "uninstall left the signing certificate in Trusted People" }
```

The Install test now ends at its `installed exe OK: …` line (`release.yml:500`), leaving the product installed for the next step.

- [ ] **Step 5: Add the every-account uninstall step**

```yaml
      # Uninstall must clean EVERY account, not just the uninstalling one
      # (backlog pk-uninst). Two extra local accounts register Kuvatin the way a
      # real first launch would: a password-logon scheduled task loads the user's
      # profile, so HKCU is their own classes hive and the sparse package
      # registers in their context (proven by the pk-uninst probe). A fresh
      # account has no "Log on as a batch job" right, so secedit grants it first
      # — without that the tasks never start (0x41303). Then uninstall as the
      # runner and assert all three accounts are clean.
      - name: Uninstall test (every account)
        if: env.PACKAGE == 'true'
        shell: pwsh
        run: |
          $ErrorActionPreference = 'Stop'
          # reg query on a missing key exits 1, and secedit is chatty: native
          # exit codes must not abort this step.
          $PSNativeCommandUseErrorActionPreference = $false
          $exe = 'C:\Program Files\kuvatin\bin\kuvatin.exe'
          $msi = (Get-ChildItem target\wix\kuvatin-*-x86_64.msi | Select-Object -First 1).FullName
          $build = [int](Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').CurrentBuildNumber
          New-Item -ItemType Directory -Force C:\kvtest\users | Out-Null
          $png1x1 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='

          # What each account runs: register, one right-click conversion, and a
          # stand-in cache entry (a real one needs EXR frames).
          @'
          param([Parameter(Mandatory)][string]$User)
          $exe = 'C:\Program Files\kuvatin\bin\kuvatin.exe'
          Start-Process $exe -ArgumentList '--register','--quiet' -Wait
          $png = "C:\kvtest\users\$User\photo.png"
          Start-Process $exe -ArgumentList '--preset','"Convert to WebP"','--quiet',"`"$png`"" -Wait
          New-Item -ItemType Directory -Force "$env:TEMP\kuvatin\seq-cache\probe" | Out-Null
          Set-Content "$env:TEMP\kuvatin\seq-cache\probe\.complete" 'x'
          '@ | Set-Content C:\kvtest\as-user.ps1

          $sids = @()
          foreach ($u in 'kvoff','kvon') {
            # 14 characters: net user stops to ask a Y/N question for longer ones.
            $pw = 'Kv1!' + [guid]::NewGuid().ToString('N').Substring(0,10)
            Write-Host "::add-mask::$pw"
            net user $u $pw /add /passwordchg:no /expires:never | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "net user $u exited $LASTEXITCODE" }
            New-Item -ItemType Directory -Force "C:\kvtest\users\$u" | Out-Null
            [IO.File]::WriteAllBytes("C:\kvtest\users\$u\photo.png", [Convert]::FromBase64String($png1x1))
            icacls "C:\kvtest\users\$u" /grant "${u}:(OI)(CI)M" | Out-Null
            Set-Variable "PW_$u" $pw
            $sids += '*' + ([Security.Principal.NTAccount]::new($u)).Translate([Security.Principal.SecurityIdentifier]).Value
          }
          secedit /export /cfg C:\kvtest\rights.inf /areas USER_RIGHTS | Out-Null
          $inf = Get-Content C:\kvtest\rights.inf
          if ($inf -match '^SeBatchLogonRight') {
            $inf = $inf -replace '^(SeBatchLogonRight\s*=.*)$', ('$1,' + ($sids -join ','))
          } else {
            $inf = $inf -replace '^(\[Privilege Rights\])$', ("`$1`r`nSeBatchLogonRight = " + ($sids -join ','))
          }
          $inf | Set-Content -Encoding Unicode C:\kvtest\rights-new.inf
          secedit /configure /db C:\kvtest\rights.sdb /cfg C:\kvtest\rights-new.inf /areas USER_RIGHTS | Out-Null
          Write-Host "SeBatchLogonRight now: $((secedit /export /cfg C:\kvtest\after.inf /areas USER_RIGHTS | Out-Null; Get-Content C:\kvtest\after.inf) -match '^SeBatchLogonRight')"

          function Sid([string]$u) { ([Security.Principal.NTAccount]::new($u)).Translate([Security.Principal.SecurityIdentifier]).Value }
          function ProfDir([string]$sid) { (Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\$sid").ProfileImagePath }

          function Register-As([string]$User,[string]$Password) {
            $arg = "-NoProfile -ExecutionPolicy Bypass -File C:\kvtest\as-user.ps1 -User $User"
            $a = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -Argument $arg
            $s = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Hours 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
            Register-ScheduledTask -TaskName "kv-$User" -Action $a -Settings $s -User "$env:COMPUTERNAME\$User" -Password $Password -RunLevel Limited -Force | Out-Null
            Start-ScheduledTask -TaskName "kv-$User"
            $deadline = (Get-Date).AddMinutes(4)
            do { Start-Sleep -Seconds 3; $state = (Get-ScheduledTask -TaskName "kv-$User").State } while ($state -in 'Queued','Running' -and (Get-Date) -lt $deadline)
            $rc = (Get-ScheduledTaskInfo -TaskName "kv-$User").LastTaskResult
            Write-Host ('kv-{0}: state {1}, last result 0x{2:X8}' -f $User, $state, $rc)
            if ($rc -ne 0) {
              Get-WinEvent -LogName Microsoft-Windows-TaskScheduler/Operational -MaxEvents 40 -ErrorAction SilentlyContinue |
                Where-Object { $_.Message -match "kv-$User" } |
                ForEach-Object { '  event {0}: {1}' -f $_.Id, ($_.Message -replace '\s+',' ') }
              throw "registration task kv-$User failed (0x$('{0:X8}' -f $rc))"
            }
          }

          # How many of the 17 classic keys an account still has. reg.exe only:
          # a PowerShell provider handle would block the hive unload.
          function Count-Keys([string]$sid) {
            $exts = '.png','.jpg','.jpeg','.jpe','.jfif','.webp','.bmp','.tiff','.tif','.gif','.exr'
            $rel = @($exts | ForEach-Object { "SystemFileAssociations\$_\shell\Kuvatin" }) + @(
              'SystemFileAssociations\image\shell\Kuvatin','Directory\shell\Kuvatin','Directory\Background\shell\Kuvatin',
              'Kuvatin.CommandStore','Kuvatin.CommandStore.Background','Kuvatin.CommandStore.Frames')
            $root = "HKU\$($sid)_Classes"
            $mounted = $false
            if (-not (Test-Path "Registry::HKEY_USERS\$($sid)_Classes")) {
              $root = "HKU\kv-check-$sid"
              reg load $root (Join-Path (ProfDir $sid) 'AppData\Local\Microsoft\Windows\UsrClass.dat') *> $null
              if ($LASTEXITCODE -ne 0) { $global:LASTEXITCODE = 0; return -1 }
              $mounted = $true
            }
            $n = @($rel | Where-Object { reg query "$root\$_" *> $null; $LASTEXITCODE -eq 0 }).Count
            if ($mounted) { [GC]::Collect(); [GC]::WaitForPendingFinalizers(); reg unload $root *> $null }
            $global:LASTEXITCODE = 0
            return $n
          }

          Register-As kvoff (Get-Variable PW_kvoff -ValueOnly)
          Register-As kvon (Get-Variable PW_kvon -ValueOnly)
          # kvon stays signed in: hold its hive mounted across the uninstall.
          $ha = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -Argument '-NoProfile -Command Start-Sleep -Seconds 900'
          $hs = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Hours 1)
          Register-ScheduledTask -TaskName kv-hold -Action $ha -Settings $hs -User "$env:COMPUTERNAME\kvon" -Password (Get-Variable PW_kvon -ValueOnly) -RunLevel Limited -Force | Out-Null
          Start-ScheduledTask -TaskName kv-hold
          Start-Sleep -Seconds 5

          # Preconditions, so the test cannot pass vacuously.
          foreach ($u in 'kvoff','kvon') {
            $sid = Sid $u
            $dir = ProfDir $sid
            # 16 of 17: the legacy 'image' root is absent on a fresh install.
            $have = Count-Keys $sid
            if ($have -lt 16) { throw "$u registered only $have verb key(s) before uninstall" }
            if ($env:SIGNED -eq 'true' -and $build -ge 22000) {
              $forUser = Get-AppxPackage -AllUsers VilleMattila.Kuvatin | Where-Object { $_.PackageUserInformation.UserSecurityId.Sid -contains $sid }
              if (-not $forUser) { throw "$u did not register the sparse package before uninstall" }
            }
            if (-not (Test-Path (Join-Path $dir 'AppData\Local\Kuvatin\kuvatin.log'))) { throw "$u has no log before uninstall" }
            if (-not (Test-Path (Join-Path $dir 'AppData\Local\Temp\kuvatin\seq-cache'))) { throw "$u has no cache before uninstall" }
            if (-not (Test-Path (Join-Path $dir 'AppData\Roaming\Kuvatin\presets.toml'))) { throw "$u has no presets before uninstall" }
          }

          # Uninstall as the runner, verbosely: WixQuietExec64 copies the SYSTEM
          # action's stdout into this log.
          $log = "$env:TEMP\kuvatin-uninstall.log"
          $p = Start-Process msiexec -ArgumentList "/x","`"$msi`"","/qn","/norestart","/l*v","`"$log`"" -Wait -PassThru
          if ($p.ExitCode -ne 0) { Get-Content $log | Select-Object -Last 60; throw "msiexec /x exited $($p.ExitCode)" }
          Get-Content $log | Select-String 'KuvatinUnregisterAllUsers|Kuvatin: ' | Select-Object -Last 20 | ForEach-Object { $_.Line }

          # The uninstalling account (unchanged expectations).
          if (Test-Path $exe) { throw "uninstall left $exe behind" }
          if (Test-Path 'HKCU:\Software\Classes\SystemFileAssociations\.png\shell\Kuvatin') { throw "uninstall left the runner's Explorer verb behind" }
          $startMenu = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\Kuvatin'
          if (Test-Path $startMenu) { throw "uninstall left the Start menu folder behind" }

          # Every other account.
          foreach ($u in 'kvoff','kvon') {
            $sid = Sid $u
            $dir = ProfDir $sid
            $left = Count-Keys $sid
            if ($left -ne 0) { throw "$u: uninstall left $left verb key(s) behind" }
            if (Test-Path (Join-Path $dir 'AppData\Local\Kuvatin\kuvatin.log')) { throw "$u: uninstall left the log behind" }
            if (Test-Path (Join-Path $dir 'AppData\Local\Temp\kuvatin')) { throw "$u: uninstall left %TEMP%\kuvatin behind" }
            $pkgLeft = Get-ChildItem (Join-Path $dir 'AppData\Local\Packages') -Filter 'VilleMattila.Kuvatin_*' -ErrorAction SilentlyContinue
            if ($pkgLeft) { throw "$u: uninstall left $($pkgLeft.Name) behind" }
            if (-not (Test-Path (Join-Path $dir 'AppData\Roaming\Kuvatin\presets.toml'))) { throw "$u: uninstall deleted the presets, which must be kept" }
          }
          if ($env:SIGNED -eq 'true' -and $build -ge 22000 -and (Get-AppxPackage -AllUsers VilleMattila.Kuvatin -ErrorAction SilentlyContinue)) {
            throw "uninstall left the sparse package registered for some account"
          }
          if ($env:SIGNED -eq 'true' -and (Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object { $_.Subject -like 'CN=Ville Mattila*' })) {
            throw "uninstall left the signing certificate in Trusted People"
          }
          # The SYSTEM pass must not have written into the system profile.
          if (Test-Path 'C:\Windows\System32\config\systemprofile\AppData\Local\Kuvatin') {
            throw "the SYSTEM cleanup seeded a Kuvatin folder in the system profile"
          }
          Write-Host 'every account is clean after uninstall'
```

- [ ] **Step 6: Commit all three edits together**

```bash
git add .github/workflows/release.yml
git commit -m "CI proves uninstall leaves nothing behind for any account

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 15: Docs — README and CHANGELOG

**Files:**
- Modify: `crates/kuvatin/wix/README.md:83-101`
- Modify: `CHANGELOG.md` (Unreleased → Fixed)

- [ ] **Step 1: Update the README's "Registration scope" section**

In `crates/kuvatin/wix/README.md`, replace the "**Uninstall is best-effort:**" bullet (`README.md:98-101`) with:

```markdown
- **Uninstall cleans every account:** the impersonated `--unregister` cleans the
  uninstalling user; a second custom action, `kuvatin.exe --unregister-all-users`,
  runs as SYSTEM (deferred, `Impersonate='no'`, `Return='ignore'`) after it and
  before `RemoveFiles`, and removes the classic verbs, the sparse package and the
  per-user files (logs, `%TEMP%\kuvatin`, the package's `AppData\Local\Packages`
  folder) for **every** profile — loading a signed-out profile's `UsrClass.dat`
  when its hive is not mounted. Presets and settings (`%APPDATA%\Kuvatin`) are
  kept. It is skipped during a major upgrade (`NOT UPGRADINGPRODUCTCODE`) so
  other users' menus survive an upgrade; `ensure_registered()` re-heals them per
  user at next launch.
```

- [ ] **Step 2: Update the CHANGELOG (CRLF)**

`CHANGELOG.md` is CRLF throughout and `core.autocrlf` is on. Add a bullet as the last item of the existing `### Fixed` list under `## [Unreleased]` (the list currently ends with the "Ctrl+Enter starts a conversion…" bullet, `CHANGELOG.md:41-42`), keeping the CRLF line endings of its neighbours:

```markdown
- Uninstalling now removes Kuvatin's right-click menu, its Windows 11 menu
  package and its leftover logs and cache for every account on the PC, not just
  the account that runs the uninstaller. Your saved presets are kept.
```

Check that the diff is additions only and the endings held:
Run: `git diff --stat CHANGELOG.md`
Expected: `1 file changed, 3 insertions(+)` (no deletions — a deletion count means the line endings were rewritten).

- [ ] **Step 3: Commit**

```bash
git add crates/kuvatin/wix/README.md CHANGELOG.md
git commit -m "The changelog and the installer README say uninstall covers every account

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Where every test runs in CI (so nothing passes by skipping)

| Test | Task | CI location | Runs when |
| --- | --- | --- | --- |
| `shell::regutil::tests::enumerates_and_deletes_subkeys` | 1 | `Test (core + gui — gates the release)` (`release.yml:212-213`) | every push/PR/tag/dispatch — no skip guard |
| `shell::verbs::tests::static_list_covers_every_piece` | 2 | same gate | always |
| `shell::profiles::tests::*` (3 tests) | 4 | same gate | always |
| `shell::hive::tests::offline_cleanup_removes_only_kuvatin_verbs` | 5 | the gate **and** the dedicated `Test (offline hive cleanup — gates the release)` step (Task 14b) | every push/PR/tag/dispatch; the dedicated step runs it by exact name and **fails if it skips** or does not report `ok` |
| `shell::paths::tests::*` (4 tests) | 6 | the core gate | always |
| `shell::files::tests::*` (3 tests) | 7 | the core gate | always (the junction case self-skips only if `mklink /J` is refused, which `windows-latest` allows) |
| `cli::tests::unregister_all_users_flag` | 11 | the core gate | always |
| `shell::package::tests::*` (existing 3) | 8 | the core gate | always |
| clippy over all targets | 10, 12 | `Clippy (warnings are errors)` (`release.yml:206-207`) | always |
| Every-account uninstall, end to end (verbs + package + files + presets kept + no system-profile leftover) | 13, 14c | `Uninstall test (every account)` | when `PACKAGE == 'true'`: tags, manual dispatch, and PRs touching the installer (`release.yml:63,94-112`); the package assertions need `SIGNED == 'true'`, which Task 14a now makes true on those runs too |

The signed-out hive is the one thing a live account on the runner cannot reproduce, which is why Task 5's test exists and why Task 14b refuses to let it skip. Package removal for a signed-out account remains the accepted gap recorded at the top of this plan.

---

## Self-review notes (checked against the coordinator's brief)

1. Pure unit-tested parts — SID filter (Task 4), shared key list including the `SystemFileAssociations\*` enumeration (Tasks 2 and 5), per-profile path plan with the "never the whole `Local\Kuvatin`" guard (Task 6). ✓
2. Registry part — loaded hive edited in place, otherwise `RegLoadKeyW` under a private name, delete, unload with retries; `REG_LINK` refused (Tasks 1, 5); in-crate offline test that CI runs by name and cannot silently skip (Tasks 5, 14b). ✓
3. Package part — `FindPackages` filtered by name, `RemovePackageWithOptionsAsync(RemoveForAllUsers)` with a bounded wait, deprovision only when provisioned, leftovers logged (Task 8). ✓
4. File part — junction-safe walk including `Packages\<family>` per profile, parents pruned only when empty, real reparse-point test (Tasks 6, 7, 9). ✓
5. `main.rs` switch, stdout only, never `applog`, dispatched before the panic hook with the pre-dispatch audit written down (Tasks 11, 12). ✓
6. WiX — `SetProperty` + `WixQuietExec64` deferred `Impersonate='no'` `Return='ignore'`, sequenced after `KuvatinUnregister` and before `KuvatinUntrustCert`, `NOT UPGRADINGPRODUCTCODE`, util extension already linked, plus how to read the action's output in the MSI log (Task 13). ✓
7. CI — every-account uninstall with the probe's proven recipe (secedit batch right, password tasks, `reg.exe`-only hive checks, `$global:LASTEXITCODE` resets, native-exit-code guard), hard assertions for signed-in accounts, `Packages` folder asserted gone, presets asserted kept, offline case left to the named test; throwaway signing key for non-tag packaging runs with tag runs unchanged and a keyless tag still failing (Task 14). ✓
8. Docs — README "Registration scope" and CHANGELOG Unreleased→Fixed in CRLF (Task 15). ✓
