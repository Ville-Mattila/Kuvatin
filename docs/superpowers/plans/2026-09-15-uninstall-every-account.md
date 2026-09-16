# Clean uninstall for every account (pk-uninst) Implementation Plan

> **For agentic workers:** this plan was executed with superpowers:subagent-driven-development, task by task, with a review after each. Every task is done and every box below is ticked. The plan has since been brought back into line with the code that was actually built from it, which is this repository's convention for a plan: it describes the final code, not the first guess at it. Read it beside `crates/kuvatin/src/shell/` and the two should say the same thing.

**Goal:** Make uninstall remove Kuvatin's Explorer menu, its Windows 11 sparse package and its per-user files for *every* account on the machine, not just the uninstalling one.

**Architecture:** A `kuvatin.exe --unregister-all-users` mode runs as SYSTEM from a deferred WiX custom action (`WixQuietExec64`, `Impersonate='no'`, `Return='ignore'`), after the existing impersonated `KuvatinUnregister` and before `KuvatinUntrustCert`, and only when not a major upgrade. It removes the sparse package for all users through the `PackageManager` API with `RemovalOptions::RemoveForAllUsers`, then walks every real user profile in `HKLM\...\ProfileList` and, for each, removes the classic verbs from that account's classes hive (the loaded `HKEY_USERS\<SID>_Classes` when the account is signed in, otherwise `RegLoadKeyW` of its `UsrClass.dat`) and deletes its Kuvatin logs, `%TEMP%\kuvatin` and `AppData\Local\Packages\VilleMattila.Kuvatin_*`. Presets and settings stay. The registry key list is one shared source of truth used by both the per-user unregister and the all-users path. Everything the mode does is reported on stdout, which `WixQuietExec64` copies into the verbose MSI log.

**The shape of it is set by three security findings**, not by the feature: the all-users path runs as SYSTEM against hives and directories the accounts themselves own and can change while it runs, so every registry key and every file is reached through a handle rather than through a name. That is the section straight after this orientation, and it is the part to read before any of the tasks.

**Tech Stack:** Rust (`windows` 0.58), WiX Toolset v3 (`WixUtilExtension` / `WixCA`), GitHub Actions (`windows-latest`, PowerShell 5.1 + pwsh 7).

---

## Orientation (read once)

**Where the work happened:** the worktree `C:\Työt\Koodaus\Kuvatin\.claude\worktrees\uninstall`, on branch `uninstall-every-account`, which starts from the tip of `after-undo`. Thirty-one commits, from `c1dba0e` (this plan) to the docs pass at the end.

**What existed before (cite before you change):**
- Per-user registration/unregistration: `crates/kuvatin/src/shell/windows.rs`. Registry writes all go to `HKEY_CURRENT_USER` via `create_key`, `delete_tree`, `wide`. The verb key roots `ROOT`/`LEGACY_ROOT`/`FOLDER_ROOT`/`BACKGROUND_ROOT`, the three command stores, `extension_roots()`, `menu_extensions()`. `unregister()` deleted a hardcoded list of 17 keys.
- The Windows 11 sparse package: `crates/kuvatin/src/shell/package.rs`. `PACKAGE_NAME = "VilleMattila.Kuvatin"`, `os_supports_package()`, `init_com()`, `registered()`/`unregister()`/`remove()`.
- The module surface: `crates/kuvatin/src/shell/mod.rs` (`pub use` list; non-windows stubs below it).
- CLI: `crates/kuvatin/src/cli.rs` (`Cli`, `Mode`, `into_mode()`, tests).
- Dispatch: `crates/kuvatin/src/main.rs`. Before the `match cli.into_mode()` it called `shell::attach_parent_console()`, `applog::install_panic_hook()`, `configure_bundled_gstreamer()`, `configure_batch_memory()`, then parsed args.
- Logs at `%LOCALAPPDATA%\Kuvatin\kuvatin.log` / `.log.1` / `crash.log`. Presets at `%APPDATA%\Kuvatin\presets.toml`. Settings at `%APPDATA%\Kuvatin\settings.toml`. Rendezvous at `%TEMP%\kuvatin\rendezvous`. Frame cache at `%TEMP%\kuvatin\seq-cache`.
- The WiX source: `crates/kuvatin/wix/main.wxs`. `<util:CloseApplication>` proves `WixUtilExtension`/`WixCA` are already linked by cargo-wix — no new `-ext` was needed.
- CI: `.github/workflows/release.yml` — build MSI + signing step, install test, env `PACKAGE`/`SIGNED`.

**Rules that held for the whole plan:**
- The SYSTEM path must **never** call `crate::applog::*` — as SYSTEM those functions resolve `%LOCALAPPDATA%` to `C:\Windows\System32\config\systemprofile\AppData\Local\Kuvatin`, which would be a brand-new leftover. Everything in the all-users path prints to **stdout** only, and only from `allusers.rs`: `hive`, `verbs`, `files`, `paths` and `package` report, and never print or log.
- The SYSTEM path must **never** call `load_store()`, `PresetStore::load_or_init`, `register_quiet()` or anything that creates presets — those would seed files in the SYSTEM profile. Verified by reading the whole call graph of `gather()`.
- **Never** delete a whole `AppData\Local\Kuvatin` or `AppData\Roaming\Kuvatin` folder outright: the local folder can hold the user's signing keys, and the roaming folder holds presets and settings we keep. Delete named files, then remove the parent only if it ends up empty.
- Keep the existing impersonated `KuvatinUnregister` action. The all-users action is additive.
- **No new crate targets.** The `kuvatin` package stays a binary with no `lib.rs` and no `tests/` directory; every test is an in-crate `#[cfg(test)]` test.

**What the toolchain needed.** The plan's first draft said no `Cargo.toml` change was required. That turned out to be wrong three separate times, and each time for a reason in the security section below. `crates/kuvatin/Cargo.toml` gained six `windows` features, each with a comment saying which module wants it and why:

| Feature | Wanted for | Where |
| --- | --- | --- |
| `Wdk_System_Registry` | `NtDeleteKey` — remove the key a vetted handle names, with no second name lookup for a planted link to hijack | `shell::regutil` |
| `Win32_Storage_FileSystem` | `SetFileInformationByHandle` + `FILE_DISPOSITION_INFO` — delete the file a vetted handle names | `shell::files` |
| `Wdk_Storage_FileSystem`, `Wdk_Foundation`, `Win32_System_IO`, `Win32_System_Kernel` | `NtOpenFile` + `OBJECT_ATTRIBUTES` + `IO_STATUS_BLOCK` + `OBJ_DONT_REPARSE` — open each component relative to its parent's handle | `shell::files` |

Everything else was already enabled: `RegLoadKeyW`, `RegUnLoadKeyW`, `RegOpenKeyExW`, `RegEnumKeyExW`, `RegQueryValueExW`, `RegGetValueW`, `HKEY_USERS`, `REG_LINK`, `REG_OPTION_OPEN_LINK` (`Win32_System_Registry`); `OpenProcessToken`, `GetCurrentProcess` (`Win32_System_Threading`); `AdjustTokenPrivileges`, `LookupPrivilegeValueW`, `GetTokenInformation`, `SE_BACKUP_NAME`, `SE_RESTORE_NAME`, `TokenElevation` (`Win32_Security`); `PackageManager::FindPackages`, `FindUsers`, `RemovePackageWithOptionsAsync`, `DeprovisionPackageForAllUsersAsync`, `FindProvisionedPackages`, `RemovalOptions::RemoveForAllUsers` (`Management_Deployment`).

**The 17 keys.** `menu_extensions()` returns the image inputs then `exr` — `png,jpg,jpeg,jpe,jfif,webp,bmp,tiff,tif,gif` + `exr` = 11 extension roots. With the `image` legacy root, `Directory`, `Directory\Background` and the three command stores, that is 17 classic keys per account, which is what `verbs::classes_subkeys()` returns and what `verbs::tests::static_list_covers_every_piece` counts.

**Known gaps, accepted.** Said here rather than implied away anywhere else:
- **A signed-out account's package removal is proven only by Windows.** CI proves package removal for accounts that are *signed in* at uninstall time, plus the offline **registry** path (Task 5's hive test). A scheduled-task logon cannot be forced to release its profile on demand, and a hosted runner has no second interactive session, so for a signed-out account we rely on `RemovalOptions.RemoveForAllUsers` behaving as documented. If a report ever says a signed-out account kept its registration, this is the first place to look.
- **Hard links are not covered.** A hard link (`mklink /H`) carries no reparse point and resolves to its own in-profile name, so it passes every check in `hive.rs` and `files.rs`. It needs write access to the target and the same volume, which is much narrower than a junction, but it is an opening. At a `files` path the disposition delete unlinks that name and leaves the file it shared, which is the harmless half; at `UsrClass.dat` it would have us mount a file somebody else's name also points at.
- **A reparse point above the profile is not covered.** A junction at `C:\Users` would redirect everything below it. Creating one there needs administrator rights, and an attacker who has those does not need this.
- **A file another process holds open is reported, not cleaned.** Any account can arrange that deliberately; it costs that account its own leftovers and the line naming the path is the whole of the damage.
- **The `RegLoadKeyW` window cannot be closed.** Between the last look at `UsrClass.dat` and the mount, its owner can still swap the path. Holding the file open ourselves is exactly what makes `RegLoadKeyW` fail, so what is left is to make the window as small as two adjacent statements and say plainly that it is there. Everything a junction could aim at outside the profile is refused before that point.

**Commit rule:** most commit messages on this branch end with exactly:
```
Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```
The session's attribution changed part-way through: the later commits — `5fbfff3`, `320b665`, `6f0e4a6` and `532f0ea` — carry `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>` instead (the last commit of the branch was made by a different model and carries its own name; the rule is the trailer, not the name in it). Commit titles are plain sentences in this repo's voice — no `feat:`, `wix:`, `ci:` or `docs:` prefixes.

---

## Three security findings, and why the code looks like it does

Three reviews found the same class of bug in three different places: a name resolved once and then resolved again. Each is written up here with what was measured, because the measurement is the reason the final shape is what it is, and a future reader who does not know it will "simplify" the code straight back into the bug. Every measurement below has a test pinning it.

### 1. A registry delete follows a symbolic link, and both of Windows' tree deletes do

**Measured** (`regutil::tests::reg_delete_key_ex_follows_a_link`, `reg_delete_tree_follows_a_nested_link`, which exist only to pin this and are the only callers of either API in the crate):

- `RegDeleteKeyExW` on a key that is a `REG_LINK` deletes the link's **target**, not the link.
- `RegDeleteTreeW(root, leaf)` follows a `REG_LINK` nested **inside** the subtree and deletes what it points at.

A user's own classes hive is a place that user can write. Planting a `SymbolicLinkValue` needs no privilege. So either API pointed at another account's hive is an arbitrary registry-delete primitive for SYSTEM, and the first draft of `delete_tree_under` — check `is_reg_link`, then call `RegDeleteTreeW` — was exactly that: the check and the delete resolved the name twice, and the second one is the one that counts.

**The resolution.** `regutil` never calls either outside those two tests, and the module documentation says so. `delete_tree_under` walks the path one segment at a time, opening each component from its parent's handle and refusing any segment that carries a `SymbolicLinkValue` of type `REG_LINK`. The leaf is exempt, because the walk ends there rather than stepping through it: `REG_OPTION_OPEN_LINK` gives a handle to the link key itself, never its target, which is what a caller that means to delete wants. Then it descends through handles (`clear_children`) and removes each key with `NtDeleteKey` on that key's own handle. A link nested inside the subtree loses its own entry, its target untouched, and the fact is carried back as a note.

Fail-closed, and bounded so a hostile hive cannot make it grind: `MAX_DEPTH = 512` (the registry's own nesting limit), `DELETE_ROUNDS = 3` per key against an owner re-creating subkeys, and `RETRY_BUDGET = 32` across a whole call, so a deep tree cannot cost `DELETE_ROUNDS^depth` attempts. An ordinary delete never touches the budget.

**Least privilege came out of the same review**, because a right we ask for and do not need is one more ACE the hive's owner can deny to stop the uninstall at the door. `DELETE_ACCESS` is `DELETE | KEY_ENUMERATE_SUB_KEYS | KEY_QUERY_VALUE` and not `KEY_ALL_ACCESS`; `TRAVERSE_ACCESS` is `KEY_QUERY_VALUE` alone, which is all a segment we merely pass through needs and all a root handle needs when the only thing done with it is opening children; `READ_ACCESS` is `KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS` and not `KEY_READ`, which would also ask for `READ_CONTROL` and `KEY_NOTIFY`. One Deny ACE on `KEY_NOTIFY` at `Directory\shell` would otherwise have blocked every delete below it; `regutil::tests::a_denied_key_notify_on_an_intermediate_does_not_block_the_delete` is that case.

**Cost:** the `Wdk_System_Registry` feature, for ntdll's `NtDeleteKey`.

### 2. A directory you hold can be turned into a reparse point without moving

**Measured** (`files::tests::a_held_directory_cannot_be_renamed_out_from_under_us`, `a_directory_we_hold_can_still_be_turned_into_a_junction`, `a_parent_whose_child_we_hold_cannot_be_converted`):

- Holding a directory open without `FILE_SHARE_DELETE` **does** stop a rename and stop a delete.
- It does **not** stop `FSCTL_SET_REPARSE_POINT`, which converts that directory into a junction **in place** — same object, same handle, no rename and no delete. It wants an empty directory and a handle with write access, and `FILE_WRITE_ATTRIBUTES` counts; that right takes no part in Windows' sharing check at all, so no share mode can refuse it.
- A directory with anything in it cannot be converted: `ERROR_DIR_NOT_EMPTY`.

So pinning by name is not pinning. The first version of `files.rs` held every ancestor open and reasoned that a name which cannot be renamed or deleted stays put. The leaf's parent is the one held ancestor its owner can empty — by deleting the leaf — after which the parent converts, and `remove_dir_all(path)` / `remove_file` / `remove_dir` re-resolve through the junction as SYSTEM. Name-constrained (folders called `kuvatin` or `VilleMattila.Kuvatin_*`, files called `kuvatin.log`, `kuvatin.log.1`, `crash.log`) but outside the profile, anywhere on the machine.

**The resolution, in two halves.** Hold the *leaf* itself, not only its ancestors, so the parent can never be empty while we work; and delete through the handle rather than through the path. Files, prunes and a junction standing where one of our folders should be all go through `SetFileInformationByHandle(FileDispositionInfo)`, so the object removed is exactly the object vetted, and the junction case never descends — the entry is what we delete and whatever it points at is not ours to look at. A tree is handed to `std::fs::remove_dir_all` while we hold its root without `FILE_SHARE_DELETE`: std empties the tree and then fails on the root with a sharing violation that is us (error 32), and the root goes through our own handle like everything else. The last measurement above is what makes that an induction rather than a hope — from the leaf upwards, every directory in the chain is permanently non-empty and so cannot be converted.

**Cost:** the `Win32_Storage_FileSystem` feature.

### 3. Reading the attributes again afterwards proves nothing

**Measured** (`files::tests::a_parent_converted_and_reverted_around_the_leaf_open_deletes_nothing` and `..._around_a_tree_deletes_nothing`; reproduced end to end before the fix):

The second version of `files.rs` added a second look: open the leaf, then re-read every ancestor through the handle held since the walk passed it, and refuse any that had become a reparse point. A point-in-time check is not a binding. The owner converts the parent, our leaf open resolves **by name** through the junction and lands on the victim's file, and then the owner puts the parent back with `FSCTL_DELETE_REPARSE_POINT`. The re-read sees an ordinary directory and waves it through, and the handle we are about to delete through is the victim's.

Two details make it practical rather than theoretical. The owner can see the exact instant to revert, because our open denies `FILE_SHARE_DELETE` and theirs starts failing with `ERROR_SHARING_VIOLATION`. And an oplock on the victim's file can hold our open open for as long as the owner likes, so being quick is no defence. The tree case is the same trick with a second conversion before std's root open. The regression tests need **three** moments — convert before the leaf open, revert after it, convert again before the tree delete; a two-moment version passed against the broken code and proved nothing.

**The resolution: never resolve a name below the profile root a second time.** The profile root is opened by path, once, because `profiles::vet_dir` is the one thing that vouched for it. Every component after it is opened **relative to its parent's own handle**:

```rust
fn open_relative(
    parent: &File,
    name: &OsStr,
    access: u32,
    share: u32,
    options: NTCREATEFILE_CREATE_OPTIONS,
) -> Result<File, NTSTATUS>
```

`NtOpenFile`, with the parent handle as `RootDirectory`, a single-component `UNICODE_STRING` as `ObjectName`, and `OBJ_DONT_REPARSE | OBJ_CASE_INSENSITIVE`; options `FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_SYNCHRONOUS_IO_NONALERT`, plus `FILE_DIRECTORY_FILE` for an ancestor; share `READ | WRITE` and never `DELETE`; and no retry without `OBJ_DONT_REPARSE`. There is no path for anything to redirect, because there is no path: the kernel looks the name up inside the object we are holding. That turns the whole class of attack from something to detect into something that cannot resolve — if the owner converts a parent we hold, a relative open inside it does not reach a victim, it fails.

**Also measured:** a refused reparse comes back as `STATUS_REPARSE_POINT_NOT_RESOLVED` (0xC0000280), *not* `STATUS_REPARSE_POINT_ENCOUNTERED` as the name suggests; all three plausible spellings map to the refusal.

**And verified in std 1.96**, since a tree delete leans on it: `remove_dir_all` opens the root with `FILE_FLAG_OPEN_REPARSE_POINT` and descends with `NtOpenFile(RootDirectory, OBJ_DONT_REPARSE, FILE_OPEN_REPARSE_POINT)` (`sys/fs/windows.rs:1387-1403`, `sys/pal/windows/remove_dir_all.rs:73-140`) — the same defence by the same means. `remove_file` on a junction where a file is expected gives error 5 with link and target intact; `remove_dir` on a junction unlinks the link; hard links destroy nothing. (`rustup component add rust-src` was installed on this machine to read that.)

`reach`'s ancestor re-read is still there and is now belt and braces over the induction, commented as not load-bearing: the tests that matter pass without it.

**Cost:** the `Wdk_Storage_FileSystem`, `Wdk_Foundation`, `Win32_System_IO` and `Win32_System_Kernel` features.

### The same lesson in the registry, one hive up

`hive.rs` has the file-side version of finding 1, and it is worth naming here because it is why `checked_usrclass_path` exists. `ProfileImagePath` comes from an admin-only key, but everything below the profile root belongs to the account, and `RegLoadKeyW` **creates** the hive file when it is missing — so a wrong path is not merely read, it is written. The guard walks `profile_dir` itself and then each of `AppData`, `Local`, `Microsoft`, `Windows`, `UsrClass.dat`, refusing a reparse point at any of them, confirms with `canonicalize` that the file resolves back inside the canonicalised profile root, and `clean_offline` looks once more, as late as it can, that the file is still a file. It stats the profile directory itself at the top of the walk and does not lean on `profiles::vet_dir` having done so, because a guard that is only correct when read together with another module's is not a guard. `under()` compares whole components and is correct only when both sides come from `canonicalize`; its doc says so and its test carries a `\\?\`-prefixed pair.

A fourth finding came later, and it is not of the same kind as the three above: those are escalation, this is availability. `std::fs::remove_dir_all`'s descent is unbounded, so an account that keeps creating subdirectories under its own `AppData\Local\Temp\kuvatin` while the sweep runs can hold that one call open for as long as it cares to, and because the accounts are visited one after another inside a deferred custom action, that holds up every account after it and the uninstall with them — an account denying the machine's uninstall, not reaching anywhere it could not already reach. `allusers.rs` now bounds each account's file work to a sixty-second `FILE_BUDGET` (Task 9), which is the fix: nowhere in `files.rs` itself needed to change.

---

## File structure

- **`crates/kuvatin/src/shell/regutil.rs`** (new) — registry helpers over an arbitrary `HKEY` root: link-refusing walks, handle-relative deletes, self-closing handles. Used by `verbs`, `hive`, `profiles`, `windows` and `test_support`.
- **`crates/kuvatin/src/shell/verbs.rs`** (new) — the single source of truth for the classic verb subkey list relative to a classes root, and the one deletion loop both the per-user and the all-users path run.
- **`crates/kuvatin/src/shell/profiles.rs`** (new) — `ProfileList` enumeration, the SID filter, and the one directory vetting that the file walk starts from.
- **`crates/kuvatin/src/shell/hive.rs`** (new) — privileges, elevation, the loaded-or-mounted classes hive, the `UsrClass.dat` path guard, and the elevated offline test.
- **`crates/kuvatin/src/shell/paths.rs`** (new) — the per-profile file plan (pure), the package data prefix, and the "never the whole Kuvatin folder" guard.
- **`crates/kuvatin/src/shell/files.rs`** (new) — the handle-relative, reparse-refusing delete that carries out a plan.
- **`crates/kuvatin/src/shell/allusers.rs`** (new) — the orchestrator `unregister_all_users()`, stdout only, with all the wording in pure functions.
- **`crates/kuvatin/src/shell/test_support.rs`** (new, `#[cfg(all(windows, test))]`) — the skip rule and the Deny-ACE harness shared by the modules' tests.
- **`crates/kuvatin/src/shell/package.rs`** — gained the all-users removal with its bounded wait.
- **`crates/kuvatin/src/shell/windows.rs`** — the verb constants became `pub(super)`, and both registration and unregistration now read one list.
- **`crates/kuvatin/src/shell/mod.rs`** — declares the new modules, exports `unregister_all_users`, and carries the non-windows stub.
- **`crates/kuvatin/src/cli.rs`** — `--unregister-all-users` and `Mode::UnregisterAllUsers`.
- **`crates/kuvatin/src/main.rs`** — dispatches the new mode before the panic hook and the engine setup.
- **`crates/kuvatin/Cargo.toml`** — the six `windows` features listed in the orientation.
- **`crates/kuvatin/wix/main.wxs`** — `SetProperty` + `CustomAction` (`WixQuietExec64`) + sequencing in both branches.
- **`.github/workflows/release.yml`** — throwaway signing key on non-tag packaging runs; the offline-hive test by name; the every-account uninstall step.
- **`crates/kuvatin/wix/README.md`** and **`CHANGELOG.md`** — docs.

---

## Task 1: The generic registry helpers (`regutil.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/regutil.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`, `crates/kuvatin/Cargo.toml`

- [x] **Step 1: Declare the module (compile scaffold)**

In `crates/kuvatin/src/shell/mod.rs`:

```rust
#[cfg(windows)]
mod regutil;
```

- [x] **Step 2: Write `regutil.rs` with its tests**

The module operates on an arbitrary open `HKEY` root, so the same code acts on `HKCU\Software\Classes` (the per-user unregister) and on a mounted `HKEY_USERS\<SID>_Classes` (the all-users uninstall). It assumes the hive is hostile throughout — see finding 1 above, which is what this module's shape is for. The surface as it shipped:

```rust
/// NUL-terminated UTF-16, for the `PCWSTR` registry APIs.
pub(super) fn wide(s: &str) -> Vec<u16>;

/// The only kind of key handle this module hands out: closes itself on drop.
pub(super) struct OwnedKey(HKEY);
impl OwnedKey {
    pub(super) fn get(&self) -> HKEY;
    pub(super) fn own(h: HKEY) -> Self;      // tests only; carries its own allow
}

/// What a read-only open found. The third arm is the one worth having.
pub(super) enum Found { Key(OwnedKey), Absent, Refused(String) }

/// Read-only openers. These FOLLOW a symbolic link at any segment, and are
/// therefore for reading only.
pub(super) fn open_owned(root: HKEY, subpath: &str) -> Option<OwnedKey>;
pub(super) fn open_owned_reporting(root: HKEY, subpath: &str) -> Found;
pub(super) fn enum_subkeys(root: HKEY, subpath: &str) -> Result<Vec<String>, String>;

/// The link-safe opener: the only one to use for a handle you will write or
/// delete through. Refuses a `REG_LINK` at any segment it passes THROUGH; the
/// leaf is exempt, because the walk ends there.
pub(super) fn open_owned_no_links(
    root: HKEY,
    subpath: &str,
    access: REG_SAM_FLAGS,
) -> Result<OwnedKey, String>;

/// Whether a key is a registry symbolic link. `Err` when it would not open, so
/// a refusal is never read as "not a link". Tests only; carries its own allow.
pub(super) fn is_reg_link(root: HKEY, subpath: &str) -> Result<bool, String>;

/// What `delete_tree_under` did, ready for the uninstall log.
pub(super) enum DeleteOutcome {
    Deleted { notes: Vec<String> },
    Absent,
    Refused { why: String, notes: Vec<String> },
}
impl DeleteOutcome { pub(super) fn notes(&self) -> &[String]; }

pub(super) fn delete_tree_under(root: HKEY, subpath: &str) -> DeleteOutcome;

pub(super) const DELETE_RIGHT: REG_SAM_FLAGS;      // the standard DELETE bit
pub(super) const TRAVERSE_ACCESS: REG_SAM_FLAGS;   // KEY_QUERY_VALUE alone
```

Private to the module: `close` (so nothing outside can double-close what an `OwnedKey` holds), `READ_ACCESS`, `DELETE_ACCESS`, `open_component`, `walk_no_links`, `Sweep` with `clear_children`/`clear_and_delete`, `explain_error`/`explain_status`, and the three bounds `MAX_DEPTH`/`DELETE_ROUNDS`/`RETRY_BUDGET`.

Two rules the rest of the plan leans on:

1. **Absence and refusal are never confused.** `enum_subkeys` returns `Ok(empty)` only for a key that is genuinely *not there*; a key that is there and will not open is `Err`. Reading a refusal as absence is how an uninstall concludes a hive is already clean and leaves the keys behind. `Found::Refused` is the same rule for a single key, and `enum_children` handles `ERROR_MORE_DATA` by asking again at the same index with a roomier heap buffer rather than truncating the list.
2. **Every reason is punctuated `<key>: <what happened>`,** with the key relative to whichever root was passed in, so a caller can put the hive in front of any of them and get one sentence. Exactly one reason breaks the rule and has to: `an empty path names no key` answers a call that named none.

Tests: 14, including the two that pin Windows' own behaviour (`reg_delete_key_ex_follows_a_link`, `reg_delete_tree_follows_a_nested_link`), positive link tests at the leaf and at an intermediate, a link nested three deep, a link to its own ancestor (does not hang), a planted `SymbolicLinkValue` on a plain key (does not save it), a key with thousands of children, and three Deny-ACE tests that self-skip under the rule in "How a test is allowed to skip". Scratch keys are named from the pid, a nanosecond clock and a `static AtomicU64`, and a `Scratch` `Drop` guard removes them however the test ends.

- [x] **Step 3: Run the tests**

Run: `cargo test -p kuvatin --release regutil`

- [x] **Step 4: Commit**

Three commits, in this order:

```
Registry helpers work on any classes hive, not just HKCU
Registry helpers refuse links at every segment and report why
Registry cleanup survives a hive owner who fights back
```

The second is the hardening from finding 1 (and the `Wdk_System_Registry` feature); the third is the review's fixes — the `DeleteOutcome`/`Found` shapes, least privilege, and `enum_subkeys` telling absence from refusal. A fourth, `The registry walk asks for less and gives up sooner`, landed after Task 2 and finished the least-privilege work.

---

## Task 2: The shared verb key list (`verbs.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/verbs.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [x] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod verbs;
```

- [x] **Step 2: Write the failing test first**

`static_list_covers_every_piece` went in first against an empty stub: it asserts the presence of a `.png` root, the `.exr` root, the `image` legacy root, both `Directory` roots and all three command stores, and then pins the length at `menu_extensions().len() + 6` — 17.

- [x] **Step 3: Run it to see it fail**

Run: `cargo test -p kuvatin --release verbs::` — the first `contains` fails against the stub.

- [x] **Step 4: Write the real implementation**

**Not one key is spelled out in this module.** Registration writes absolute `HKCU\Software\Classes\…` paths; this module names the same keys relative to whichever hive it is handed, by taking that prefix off the constants in `super::windows`. A verb moved or a store renamed there travels straight through to the uninstall. (The dependency runs this way round after Task 3; the first draft re-spelled six of the keys, which was the drift the shared list existed to prevent.)

```rust
/// `Software\Classes\Directory\shell\Kuvatin` -> `Directory\shell\Kuvatin`.
/// `None` for a path that is not under the classes root — a `debug_assert` for
/// whoever made the mistake, and a skip in release, because this runs from
/// `unregister()`, whose custom action is `Return='ignore'`: a panic there
/// would take the whole menu removal down without a word to anyone.
fn under_classes(absolute: &str) -> Option<String>;

/// The 17 verb subkeys relative to a classes root.
pub(super) fn classes_subkeys() -> Vec<String>;

/// Those, plus any `SystemFileAssociations\<assoc>\shell\Kuvatin` found by
/// enumeration. Returns (keys, troubles): the static list comes back whatever
/// happens, and the second Vec says in words everything that could not be read
/// in full. A candidate that refuses to open is KEPT on the list as well as
/// reported — the delete asks for different rights than the read did.
/// De-duplicated ASCII-case-insensitively.
pub(super) fn subkeys_to_delete(classes_root: HKEY) -> (Vec<String>, Vec<String>);

/// One line the sweep produced, kept apart by what it means.
pub(super) enum SweepLine { Trouble(String), Note(String), Refused(String) }

pub(super) struct VerbSweep {
    pub removed: usize,
    pub absent: usize,
    pub refused: usize,
    pub lines: Vec<SweepLine>,
}
impl VerbSweep {
    pub(super) fn troubles(&self) -> Vec<&str>;
    pub(super) fn notes(&self) -> Vec<&str>;
    pub(super) fn refusals(&self) -> Vec<&str>;
    /// One reason behind several refusals, said once and loudly: a single
    /// planted link at `SystemFileAssociations` refuses all twelve keys below
    /// it in the same words. First past the post on a tie, so it stays put
    /// between runs.
    pub(super) fn shared_obstacle(&self) -> Option<(&str, usize)>;
    /// The refusals `shared_obstacle` does not account for.
    pub(super) fn other_refusals(&self) -> Vec<&str>;
}

/// The one deletion loop the per-user unregister and the all-users uninstall
/// both run. Works relative to the handle it is given and never walks a path
/// down to it. Reports rather than prints.
pub(super) fn remove_verbs_under(classes_root: HKEY) -> VerbSweep;
```

Why `remove_verbs_under` takes a handle and not a path: `HKCU\Software\Classes` is itself a registry symbolic link to `HKEY_USERS\<SID>_Classes`, and `regutil` refuses to step through one — so a path-walking delete would refuse every verb key and leave the whole menu in place. The handle needs no more than `TRAVERSE_ACCESS`.

Why it reports rather than prints: `windows.rs` logs the lines through `applog`, and the all-users path must not log at all. One loop, two reporters.

- [x] **Step 5: Run the tests**

Run: `cargo test -p kuvatin --release verbs::` — 9 tests, three of which self-skip when a Deny ACE cannot be set.

- [x] **Step 6: Commit**

```
One list names every context-menu key Kuvatin writes
```

---

## Task 3: Registration and unregistration read one list

**Files:**
- Modify: `crates/kuvatin/src/shell/windows.rs`

- [x] **Step 1: Make `windows.rs` the owner of the names and `verbs.rs` the deriver**

The constants became `pub(super)`: `CLASSES_ROOT = r"Software\Classes"`, `LEGACY_ROOT`, `FOLDER_ROOT`, `BACKGROUND_ROOT`, `STORE_ITEM`/`STORE_BACKGROUND`/`STORE_FRAMES` and a `STORES` list. `verbs::classes_subkeys()` derives every relative name from them.

`register_quiet()` used to spell the root list and the store list out a second time, so a root added to registration could be written to every machine and uninstalled from none — and never hidden behind the Windows 11 menu either, so it would show up as a second, identical "Kuvatin". Both now read:

```rust
/// The one list of what the classic menu *is*: `register_quiet` writes exactly
/// these, `set_classic_verbs_hidden` hides exactly these, and `super::verbs`
/// deletes exactly these.
pub(super) fn classic_roots_with_stores() -> Vec<(String, &'static str)>;
pub(super) fn classic_roots() -> Vec<String>;
```

- [x] **Step 2: Delete through a handle, not through `HKEY_CURRENT_USER`**

`unregister()` now calls `remove_classic_verbs()`, which opens the classes root **once** —

```rust
fn user_classes_root() -> Option<super::regutil::OwnedKey>;
```

— with `regutil::open_owned`, the one deliberate link follow in the crate (`HKCU\Software\Classes` is Windows' own symbolic link, and following it is the point), and hands that handle to `verbs::remove_verbs_under`. It replays the sweep's lines to `applog` with `HKCU\Software\Classes\` in front of each, and logs `removed / already gone / would not go` told apart, because they mean different things. `RegDeleteTreeW(HKEY_CURRENT_USER, …)` is gone from this path, so the module-wide "never `RegDeleteTreeW`" claim holds and no later task can point the old code at another account's hive.

`unregister()` also now sweeps stray `SystemFileAssociations\*\shell\Kuvatin` keys, a small addition over the old fixed 17-key loop. `windows.rs` keeps its own `delete_tree`/`wide` for `register_quiet`, `write_store` and `create_key`.

- [x] **Step 3: Pin the two lists against each other**

`verbs::tests::the_shared_list_is_the_per_user_key_set` builds the absolute per-user list from the constants, strips the prefix, and asserts it equals `classes_subkeys()` exactly — count included, so a key silently dropped fails the build. `windows::tests::every_root_points_at_a_store_the_uninstall_removes` closes the other axis: the stores those roots reference are exactly `STORES`, and the three `write_store` calls are driven from one `(store, token, items)` list.

- [x] **Step 4: Verify nothing regressed**

Run: `cargo test -p kuvatin --release`

- [x] **Step 5: Commit**

```
The per-user unregister deletes from the one shared key list
Registration and uninstall read the verb roots off one list
```

---

## Task 4: The profile enumeration and SID filter (`profiles.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/profiles.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [x] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod profiles;
```

- [x] **Step 2: Write the failing test first**

`only_real_end_user_sids_are_cleaned` went in against a stub that always answered `false`: a local/domain SID and an Entra ID SID are cleanable; `S-1-5-18/19/20`, a `.bak` temp-profile marker, `.DEFAULT` and `S-1-5-80-…` are not.

- [x] **Step 3: Run it to see it fail**

Run: `cargo test -p kuvatin --release profiles::`

- [x] **Step 4: Write the real module**

```rust
pub(super) struct Profile { pub sid: String, pub dir: PathBuf }
pub(super) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// A real, cleanable end-user account SID.
pub(super) fn is_cleanup_sid(sid: &str) -> bool;

/// Read `ProfileImagePath` unexpanded, then expand it ourselves. `Err` carries
/// the status, so "value absent" and "read refused" do not read alike.
fn profile_dir(sid: &str) -> Result<PathBuf, String>;

/// Expand only `%SystemDrive%` and `%SystemRoot%` — an allowlist, never a
/// per-user variable — case-insensitively.
fn expand_env(s: &str) -> String;

/// Vet the one directory a file walk would start from.
fn vet_dir(dir: PathBuf) -> Result<PathBuf, String>;
fn cleanable_dir(sid: &str) -> Result<PathBuf, String>;

/// Every cleanable profile, everything that could not be read, and the SIDs
/// passed over because of it.
pub(super) fn all() -> (Vec<Profile>, Vec<String>, Vec<String>);
```

Four things here are load-bearing and were each a review finding:

- **An empty list is never the truth.** Windows keeps `S-1-5-18/19/20` in `ProfileList` on every machine there is, so `all()` reports an empty enumeration as trouble rather than as a clean machine.
- **A profile passed over says so.** `profile_dir`'s failures — a refused read, a wrong type, a missing value, an interior NUL, a value that does not fit — are surfaced as trouble with the SID, not dropped. The read is trimmed at the first NUL, and the "does not fit in 65536 characters" case is told apart from a no-progress `ERROR_MORE_DATA`.
- **Trouble comes back a line at a time, and the skipped accounts as SIDs.** Not one string joined with `"; "`: a reason can carry a semicolon of its own (`enum_children`'s `"…(error 5); read 6 before it"` does), so splitting it back apart cuts a sentence in half, and counting accounts by looking for a SID at the start of a line is guesswork about something this module already knows.
- **`vet_dir` uses `symlink_metadata`, and must not be "simplified" back to `Path::is_dir()`.** Measured on a real junction: `is_dir()` true, `symlink_metadata().is_symlink()` true, attributes `0x410`. A comment says so at the check. The refusal names the consequence — deleting through it would reach files outside the profile — and the doc records the division of labour: this vets the profile directory itself, and the file walk below vets every path it takes for itself.

Tests: 5. `a_junction_is_not_a_profile_directory` builds a real `mklink /J` junction in a temp dir through a `TempTree` guard that removes it however the test ends, and self-skips under the rule below. `enumeration_returns_only_real_user_profiles` asserts `!profiles.is_empty()`, which is machine-dependent by design — it would fail in a container holding only service SIDs — and its doc says so.

- [x] **Step 5: Run the tests**

Run: `cargo test -p kuvatin --release profiles::`

- [x] **Step 6: Commit**

```
The uninstaller can enumerate every real user profile
An account we cannot read in full is reported, not passed over
A junction where a profile directory should be is refused, and tested
```

---

## Task 5: The classes hive, loaded or signed out (`hive.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/hive.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

The runner cannot exercise the *signed-out* path with a live account — a logged-on account's hive stays mounted, and the pk-uninst probe confirmed it stayed mounted even after its task ended — so the test here builds a throwaway `UsrClass.dat`-shaped hive file, seeds Kuvatin verbs plus a bystander, runs the production `clean_offline` against that file, and re-mounts to check. It needs SeBackup/SeRestore, so it self-skips when the process is not elevated, and panics rather than skipping under `CI`.

- [x] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod hive;
```

- [x] **Step 2: Write `hive.rs` with its tests**

Signed in, Windows already has the hive mounted at `HKEY_USERS\<SID>_Classes` — a real key, not the symbolic link `HKCU\Software\Classes` is — and that mounted copy is the one Explorer reads, so it is the one we edit. Signed out, the hive is only a file; we mount it under a private name, delete through that, and unmount again.

```rust
/// Where a profile's classes hive file lives, named but not vouched for.
/// Tests only; production must not name one without vetting it.
pub(super) fn usrclass_path(profile_dir: &Path) -> PathBuf;

/// The vetted path: the profile directory and each of AppData, Local,
/// Microsoft, Windows, UsrClass.dat refused if it is a reparse point, then
/// `canonicalize` on both sides to confirm the file resolves back inside the
/// profile. See "The same lesson in the registry, one hive up".
pub(super) fn checked_usrclass_path(profile_dir: &Path) -> Result<PathBuf, String>;

pub(super) enum HiveAccess { Loaded, Mounted, None }
impl HiveAccess { pub(super) fn wording(&self) -> &'static str; }

pub(super) struct HiveOutcome {
    pub sid: String,
    pub access: HiveAccess,
    pub sweep: VerbSweep,
    pub trouble: Vec<String>,
}

pub(super) enum OfflineFailure { InUse(String), Failed(String) }
impl OfflineFailure { pub(super) fn why(&self) -> &str; }

type Cleaned = (VerbSweep, Vec<String>);

pub(super) fn clean_loaded(sid: &str) -> Result<Cleaned, String>;
pub(super) fn clean_offline(usrclass: &Path, mount_name: &str) -> Result<Cleaned, OfflineFailure>;
pub(super) fn clean_profile(profile: &Profile) -> HiveOutcome;
pub(super) fn unmount(mount_name: &str) -> Result<(), String>;
pub(super) fn enable_backup_restore() -> Result<(), String>;   // OnceLock
pub(super) fn is_elevated() -> bool;
```

`clean_profile` tries the loaded hive first, because for a signed-in account that is the copy Explorer reads and its file cannot be mounted twice anyway. Not being mounted is simply what signed out looks like, so that reason is only printed if the offline path cannot be taken either. If the mount is then refused for **any** reason the loaded path is tried once more: an account can sign in between the two steps, MSDN documents no error code for this, and `ERROR_SHARING_VIOLATION` is only what we observe — so the only safe reading of a refused mount is "the hive may be mounted now". `OfflineFailure::InUse` is wording, not a decision. Every reason gathered on the way is carried into `trouble`, because a `_Classes` key we were denied and an account that was signing in look identical from the outside and must not read alike.

Other details that came out of review and are easy to undo by accident: `clean_offline` re-checks the file exists just before the mount, because `RegLoadKeyW` *creates* a missing one; `Mounted` is an RAII guard so a failed assertion cannot leave a hive mounted, and it reports a swallowed unmount error from `Drop`; `unmount` retries `UNMOUNT_ATTEMPTS = 10` times a tenth of a second apart, because a key we have just closed can keep a hive busy for a moment; the two raw roots are opened with `open_owned_no_links` at `TRAVERSE_ACCESS`; the privileges stay enabled for the process's life, which is documented.

Tests: 10. The offline test seeds three Kuvatin keys and a bystander, mounts with a `Mount` RAII guard, and asserts the bystander survived. Also: a root held with only `KEY_QUERY_VALUE` still cleans; one obstacle at `SystemFileAssociations` refuses every key under it; a hive that is not mounted is reported and not swept; a junction on the way to the hive and a junction standing where the profile directory should be are both refused; a `UsrClass.dat` under a deny-all ACE is refused by name; an ordinary profile tree passes; and `under` compares whole components rather than string prefixes.

- [x] **Step 3: Run the test**

Run (elevated shell): `cargo test -p kuvatin --release -- --exact shell::hive::tests::offline_cleanup_removes_only_kuvatin_verbs --nocapture`

Unelevated it prints `skipping: not elevated` and passes; with `CI` set it fails instead. Task 14b's gate is the first real run unless the developer runs it elevated first.

- [x] **Step 4: Run the whole suite**

Run: `cargo test -p kuvatin --release`

- [x] **Step 5: Commit**

```
Another account's classes hive is cleaned, signed in or out
A hive is mounted only from a path the profile's owner cannot steer
The profile directory is vetted where the hive walk starts, not elsewhere
Every reason a key or a profile would not go is punctuated the same way
```

---

## Task 6: The per-profile file plan (`paths.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/paths.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [x] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod paths;
```

- [x] **Step 2: Write the failing tests first**

`plan_deletes_logs_and_temp_keeps_presets`, `never_deletes_the_whole_local_kuvatin_folder`, `protected_guard_rejects_data_folders` and `package_folder_names_match_by_prefix` went in against stubs.

- [x] **Step 3: Run them to see them fail**

Run: `cargo test -p kuvatin --release paths::`

- [x] **Step 4: Write the real module**

```rust
/// The family-name prefix of the sparse package's per-user data folder. Tied
/// to `package::PACKAGE_NAME` by a test rather than built from it, because a
/// `const` cannot be `format!`ed.
pub(super) const PACKAGE_DATA_PREFIX: &str = "VilleMattila.Kuvatin_";

pub(super) struct FilePlan {
    pub files: Vec<PathBuf>,          // kuvatin.log, kuvatin.log.1, crash.log
    pub trees: Vec<PathBuf>,          // %TEMP%\kuvatin, then the package data dirs
    pub prune_if_empty: Vec<PathBuf>, // AppData\Local\Kuvatin
}

/// Pure — it touches no disk, so the caller passes in what it globbed.
pub(super) fn plan(profile: &Path, package_data_dirs: &[PathBuf]) -> FilePlan;

/// The one thing here that reads the disk: the `Packages\VilleMattila.Kuvatin_*`
/// folders (usually zero or one), plus whatever went wrong on the way.
pub(super) fn package_data_dirs(profile: &Path) -> (Vec<PathBuf>, Vec<String>);

/// Belt and braces over paths THIS module minted, not a sanitizer.
pub(super) fn is_protected(path: &Path) -> bool;
```

**The module's own documentation is the important part, and it says what this module does not do.** Every path in a `FilePlan` is the profile root with fixed names joined onto it, and nothing here opens one or asks what it really is. Below the profile root every directory belongs to the account, and a junction needs no privilege, so its owner can aim `AppData`, `Local`, `Temp`, `Kuvatin`, `Packages` — or a `VilleMattila.Kuvatin_*` entry of their own making — anywhere on the machine, and can do it *while* the uninstall runs. Turning these names into deletions is `files.rs`'s job. **Nothing may take a path from here and hand it straight to a delete.**

`package_data_dirs` reads the disk by name like everything else, so what it finds is a *candidate* and not a target. It reports an unreadable `Packages` folder rather than answering "there is none", which would have the uninstall call the account clean. It returns directories only, matching the prefix ASCII-case-insensitively; `DirEntry::file_type` on Windows answers out of the directory listing and calls a junction a symbolic link rather than a directory, so a planted `VilleMattila.Kuvatin_*` junction is not returned at all. That is a convenience, not the defence — and it means such a junction is left in place rather than unlinked, which is accepted: it is the account's own link, pointing somewhere it already has access to.

`%TEMP%` is taken to be `AppData\Local\Temp`. An account that has redirected its TEMP keeps that cache, by decision: finding it would mean reading that account's own environment out of its hive, and a wrong answer there is a directory this uninstall would then delete as SYSTEM.

- [x] **Step 5: Run the tests**

Run: `cargo test -p kuvatin --release paths::` — 10 tests, none of which skip. They cover a tempdir holding `VilleMattila.Kuvatin_5jce0xfqz5w2a`, `VilleMattila.KuvatinPro_5jce…` and `Microsoft.WindowsStore_…` (exactly one hit), a profile with no `Packages` at all (empty, no trouble), another case spelling, a *file* named like the package folder, and a `Packages` folder that will not read.

- [x] **Step 6: Commit**

```
Each account's cleanup keeps presets and drops logs and cache
The file plan says what it does not vet
```

---

## Task 7: The handle-relative delete (`files.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/files.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`, `crates/kuvatin/Cargo.toml`

This is the task findings 2 and 3 are about. Two versions of this module were shipped and both were broken, by the same mistake in different clothes; read that section before this one.

- [x] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod files;
```

- [x] **Step 2: Write the failing junction tests first**

The junction cases went in first: a junction on the way to a tree, a junction standing where a tree should be, and a junction nested inside a tree.

- [x] **Step 3: Run them to see them fail**

Run: `cargo test -p kuvatin --release files::`

- [x] **Step 4: Write the real module**

```rust
pub(super) struct FileSweep {
    pub files_removed: usize,
    pub files_absent: usize,
    pub trees_removed: usize,   // an unlinked junction counts here: the entry went
    pub trees_absent: usize,
    pub pruned: usize,          // a folder left non-empty is neither counted nor trouble
    pub trouble: Vec<String>,   // each `<path>: <what happened>`, print as it stands
}

/// Carry out `plan` inside `profile`, which must be the exact `PathBuf`
/// `profiles::vet_dir` returned — the walk strips it off the front of every
/// planned path component by component. Files, then trees, then the prunes.
pub(super) fn remove_plan(profile: &Path, plan: &FilePlan) -> FileSweep;
```

Inside, the only thing worth memorising:

```rust
/// Open one component RELATIVE to its parent's handle. NtOpenFile with
/// `RootDirectory`, a single-component `UNICODE_STRING`, and
/// `OBJ_DONT_REPARSE | OBJ_CASE_INSENSITIVE`. Never `FILE_SHARE_DELETE`. No
/// retry without `OBJ_DONT_REPARSE`. There is no path for anything to
/// redirect, because there is no path.
fn open_relative(
    parent: &File,
    name: &OsStr,
    access: u32,
    share: u32,
    options: NTCREATEFILE_CREATE_OPTIONS,
) -> Result<File, NTSTATUS>;

/// The profile root, and the ONLY thing here opened by name.
fn open_root(profile: &Path) -> std::io::Result<File>;

/// Walk from the root to one planned path, holding every handle on the way.
fn reach(profile: &Path, target: &Path) -> Reached;
enum Reached { Leaf(Held), Absent, Refused(String) }

/// Delete the object a handle names: SetFileInformationByHandle with
/// FILE_DISPOSITION_INFO. No name is resolved, so the object removed is
/// exactly the object vetted.
fn dispose(handle: &File) -> std::io::Result<()>;
```

Ancestors are opened `FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE`, shared `READ | WRITE` and never `DELETE`, with `FILE_DIRECTORY_FILE`; the leaf is opened with `DELETE` as well. Files, prunes and the junction-unlink branch delete through the handle. A tree is handed to `std::fs::remove_dir_all` while we hold its root, which empties it and then fails on the root with error 32 that is us, and the root goes through our own handle. `reach`'s ancestor re-read at the end is belt and braces over the induction, and is commented as not load-bearing.

`reach` refuses a target that *is* the profile directory, and `kept_by_the_uninstall` refuses anything `paths::is_protected` names even if the plan asked for it. A `files` leaf that is a reparse point is refused; a `trees` leaf that is one is unlinked without descending. `remove_file` on a directory (error 5) and `remove_dir` on a file (267) get type-mismatch messages rather than the raw code, and a refusal on the way names the target it was blocking.

Tests: 21, and six of them exist only because of findings 2 and 3 — the three "convert" measurements, and the three regression tests that perform a real `FSCTL_SET_REPARSE_POINT` (and, for two of them, a real `FSCTL_DELETE_REPARSE_POINT`) at the exact moments, through a `#[cfg(test)]` `Meddle` seam in `reach` and a test-only `extern "system"` declaration of `DeviceIoControl`. Junction and reparse tests build their own artifacts in temp directories and clean up however the test ends: `Junction::drop` uses `symlink_metadata` (not `exists`, which follows the junction and answers `false` for a dangling one, exactly the link that most needs removing) and `remove_dir`, which unlinks a junction and never touches its target.

- [x] **Step 5: Run the tests**

Run: `cargo test -p kuvatin --release files::`

- [x] **Step 6: Commit**

```
Deleting an account's files walks down and never through a junction
A directory becomes a junction without moving, so the walk looks twice
Every component is opened through its parent, never by name again
```

The second is finding 2; the third is finding 3 and the rewrite that closed it.

---

## Task 8: Remove the sparse package for all users (`package.rs`)

**Files:**
- Modify: `crates/kuvatin/src/shell/package.rs`

- [x] **Step 1: Extend the imports**

`PackageManager`, `RemovalOptions`, `DeploymentResult`, `DeploymentProgress`, `PackageInstallState`, `PackageUserInformation`, and the `Foundation` async types.

- [x] **Step 2: Add the all-users removal**

```rust
pub(super) struct PackageUser { pub sid: String, pub state: String }
pub(super) struct Registration { pub full_name: String, pub users: Vec<PackageUser> }

pub(super) enum PackageReach { Swept, Unsupported, Unreachable }
impl PackageReach { pub(super) fn wording(&self) -> &'static str; }

pub(super) struct PackageSweep {
    pub reach: PackageReach,
    pub found: Vec<Registration>,
    pub removed: usize,
    pub remaining: Vec<Registration>,  // asked again, never inferred
    pub trouble: Vec<String>,          // each already prefixed with the full name
}

/// Remove the sparse package for EVERY account. `pub(super)`, so the caller
/// must live inside `shell`. Reports; never prints and never logs.
pub(super) fn unregister_all_users() -> PackageSweep;
```

Rules the orchestrator depends on:

- **"Clean" means `trouble.is_empty()`, not `remaining.is_empty()`.** A second pass that would not list leaves `remaining` empty too, and says so in `trouble`. The one case where the count is worth least is the one where it looks best.
- **`Unsupported` is not a failure.** A Windows that predates the package cannot have one registered; it carries no trouble and must not print as one.
- **Every wait is bounded.** `DEPLOYMENT_TIMEOUT` is 180 s and `DEPLOYMENT_POLL` 250 ms. It is **per operation** — one deprovisioning, then one removal per distinct full name, so N names is a worst case of (N + 1) × three minutes; N is 1 in practice. A wait that runs out **abandons** the operation rather than cancelling it: the deployment service carries on and a late removal still removes the package, whereas `Cancel()` risks an account left `Staged` and is itself a call that can block. A total budget across the run is a possible follow-up.
- **Enumerations are walked by hand** (`walk`, with an explicit `First()`), not through the crate's `IntoIterator` for `&IIterable`, whose `First().unwrap()` is the panic `walk` exists to avoid. A failed first enumeration is `Unreachable`. Deprovisioning is keyed on `FindProvisionedPackages()` itself. Trouble is chronological. `init_com()` returns the `HRESULT` so an apartment mismatch can be mentioned.

- [x] **Step 3: Compile and run the module's tests**

Run: `cargo test -p kuvatin --release package::` — 16 tests. The wording and the bounded wait are tested against hand-built sweeps; `enumerating_every_account_hands_back_only_our_package` needs an elevated token and a deployment service that starts, and self-skips otherwise. Nothing exercises the *removal* outside Task 14c's end-to-end step.

- [x] **Step 4: Commit**

```
The Windows 11 menu package goes for every account
An enumeration that refused is not a machine with nothing on it
```

---

## Task 9: The orchestrator (`allusers.rs`)

**Files:**
- Create: `crates/kuvatin/src/shell/allusers.rs`
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [x] **Step 1: Declare the module**

```rust
#[cfg(windows)]
mod allusers;
```

- [x] **Step 2: Write `allusers.rs`**

```rust
/// Clean every account, print what happened, and hand back the exit code.
pub fn unregister_all_users() -> i32 {
    let elevated = hive::is_elevated();
    print(&opening(elevated));
    let report = report(&gather(elevated));
    print(&report.lines);
    report.code
}
```

Thin on purpose. **`gather` is all the machine-touching; `opening` and `report` are all the wording**, pure functions over what the other modules handed back, and that split is what makes the whole thing testable: the tests build `Run`/`Account` by hand and read the lines, so the order, the counts, the cap on a hostile hive's hundred refusals and which cases are failures are pinned here rather than left to a real uninstall. `Run`, `Account` and `Report` are private to the module.

```rust
const EXIT_DONE: i32 = 0;          // the run happened; refusals do not change it
const EXIT_NOT_ELEVATED: i32 = 1;  // nothing was attempted
const EXIT_NO_ACCOUNTS: i32 = 2;   // no account could be listed
const CAP: usize = 3;              // lines of one kind of trouble before counting only
const PREFIX: &str = "Kuvatin: ";  // every line, so an MSI log can be grepped
```

`gather`'s order matters: **the package first**, while `kuvatin.exe` and `kuvatin_shellext.dll` are still on disk, because removing the registration is what lets go of the DLL before `RemoveFiles` comes for it. Then `hive::enable_backup_restore()` once for the whole run, then `profiles::all()` destructured into `(cleanable, trouble, skipped)`, then each account: the hive, then its files, with **nothing between the two halves** — `clean_profile` holds `UsrClass.dat` exclusively while it is mounted, and putting a disk walk in the middle would hold an account's hive open for no gain.

**One account cannot hold the whole machine's uninstall open.** `std::fs::remove_dir_all`'s descent is unbounded — it starts its enumeration again at every subdirectory it meets — so an account that keeps creating subdirectories under its own `AppData\Local\Temp\kuvatin` while the sweep is running can hold that loop open for as long as it cares to. It gains nothing by it, since the files are its own and SYSTEM reaches nowhere there it could not already reach, but the accounts are visited one after another inside a deferred custom action, so the account after it waits, and so does the uninstall: a denial of service against the uninstall, not an escalation. Each account's file work now gets a sixty-second `FILE_BUDGET`: `remove_within_budget(profile, plan) -> FileWork` spawns a thread running `files::remove_plan` and waits on it through an `mpsc` channel with `recv_timeout`, coming back as `enum FileWork { Done(FileSweep), OutOfTime, Lost(String) }`. A deadline threaded into `remove_plan` itself was considered and rejected: it would bound how many of the plan's entries are attempted, not the single `remove_dir_all` call that never returns, and it would thread a second reader through `files.rs`'s delicate argument about junctions, held handles and re-read attributes for no gain. A walk that runs out of time is left running deliberately — its handles are inside that one account's own profile, so it either finishes unwatched or ends with the process. `paths::package_data_dirs` stays outside the budget, being one directory listing that ends when it has read what was there.

What the report says, and why:

- A timed-out account is named in its own section ("files: still going after 60 seconds…"), contributes nothing to the footer totals — a walk that has not finished has nothing to count — and is counted in "N account(s) had something to report".

- The opening two lines are printed and flushed **before any work starts**, so an action that is killed, or that waits three minutes on the deployment service, still leaves a sign in the MSI log that it began.
- Lines go out through `writeln!` on a locked, flushed stdout, not `println!`: there is nothing to be done about a stdout that will not take a line, and panicking over one in a mode that exists to fail softly is the wrong end of the trade. Flushed as we go because the installer captures this through a pipe, where nothing is line-buffered.
- A run without a full token reports none of the steps: a step that did not run has nothing to say, and saying it anyway reads as a step that found nothing.
- `shared_obstacle()` is printed once and loudly, then `other_refusals()`, so each thing is said exactly once. A key refused at the read and again at the delete is two lines for one key — a `Trouble` then a `Refused` — and that is correct.
- Keys are named inside the account's own hive, never `HKCU\Software\Classes\`. A `Mounted` account's keys are reported under `HKEY_USERS\<SID>_Classes\…` even though the mount is gone by print time; that is documented rather than worked around.
- `access.wording()` and `reach.wording()` are used rather than re-derived. `HiveOutcome::trouble` is hive-level only; verb-level trouble lives in `sweep.lines`.
- "No account to clean" does not read as "nothing was attempted" when the package sweep above plainly was.
- The footer tallies accounts visited, verb keys removed and refused, files and trees removed, folders pruned, the package, and what is worth going back to — and it never reads "still registered" off a `remaining` list the verify pass could not fill.

- [x] **Step 3: Compile and run the tests**

Run: `cargo test -p kuvatin --release allusers::` — 19 tests, all pure, including `an_account_whose_files_ran_out_of_time_is_said_and_counted` (says so under that account, moves on to the next, and counts nothing from a walk that never finished).

- [x] **Step 4: Commit**

```
One pass cleans every account, and says what it could not clean
The log says it began, and never calls an unread list clean
```

A later commit added the file-work budget above: `One account cannot hold the whole machine's uninstall open`.

---

## Task 10: Export the entry point; non-windows stub

**Files:**
- Modify: `crates/kuvatin/src/shell/mod.rs`

- [x] **Step 1: Export the orchestrator**

```rust
#[cfg(windows)]
pub use allusers::unregister_all_users;
```

No test hooks are exported: every test lives inside its own module. `test_support` is declared `#[cfg(all(windows, test))]`.

- [x] **Step 2: Add the non-windows stub**

```rust
/// Remove the context menu, the menu package and the leftover files for every
/// account; no-op off Windows, where there is none of that to remove. The code
/// is the Windows path's "the run happened", because it did: there was nothing
/// to do.
#[cfg(not(windows))]
pub fn unregister_all_users() -> i32 {
    0
}
```

It returns `0` where the `register`/`unregister` stubs `bail!`, which is deliberate and documented — and never compiled, since this crate is Windows-only.

- [x] **Step 3: Build and lint**

Run: `cargo clippy -p kuvatin --all-targets -- -D warnings`

The interim `#![allow(dead_code)]` that each module needed while it had no caller came off here. Four narrow allows are left, each on one item and each saying why: `hive::usrclass_path`, `regutil::OwnedKey::own`, `regutil::Found::Key`'s handle (carried rather than read: holding it is what keeps the key open), `regutil::is_reg_link`, and `files::Held::ancestors` (never read; held open is all those handles are for).

- [x] **Step 4: Commit**

Folded into Tasks 11 and 12's commit, below.

---

## Task 11: The CLI flag and mode

**Files:**
- Modify: `crates/kuvatin/src/cli.rs`

- [x] **Step 1: Write the failing test first**

`cli::tests::unregister_all_users_flag`.

- [x] **Step 2: Run it to see it fail**

Run: `cargo test -p kuvatin --release cli::`

- [x] **Step 3: Add the flag, the variant and the dispatch**

```rust
/// Remove the Explorer context-menu entries for EVERY account and exit
/// (the installer runs this as SYSTEM during uninstall).
#[arg(
    long,
    conflicts_with_all = ["register", "unregister", "preset", "sequence_mp4", "print_extensions"]
)]
pub unregister_all_users: bool,
```

```rust
pub enum Mode {
    Register,
    Unregister,
    /// The uninstaller's SYSTEM pass: clean every account on the machine. It
    /// runs in no user's profile, so `main` dispatches it before anything that
    /// would write into one.
    UnregisterAllUsers,
    // …
}
```

`into_mode()` checks it after `unregister`. It combines with `--quiet` and with nothing else: every other headless mode wants a profile this one is not running in. A stray PATH is accepted and ignored, exactly as for `--register`/`--unregister` — the mode cleans every account, not something somebody selected.

- [x] **Step 4: Run the test**

Run: `cargo test -p kuvatin --release cli::`

- [x] **Step 5: Commit**

Folded into Task 12's commit, below.

---

## Task 12: Dispatch the new mode in `main.rs`, before anything touches a profile

**Files:**
- Modify: `crates/kuvatin/src/main.rs`

**What ran before the dispatch, and why it matters.** As SYSTEM, `%LOCALAPPDATA%` resolves into `C:\Windows\System32\config\systemprofile`, so anything that writes there is a brand-new leftover — the very thing this feature removes. Checked line by line:

- `shell::attach_parent_console()` — `AttachConsole` only. Safe, and harmless on the shipping path: the SYSTEM msiexec parent has no console.
- `applog::install_panic_hook()` — installing the hook writes nothing on the way in. It is the hook's own body that would: it resolves the log directory, and creates it, at panic time. A panic during the SYSTEM run would leave `…\systemprofile\AppData\Local\Kuvatin\{crash.log,kuvatin.log}`. So the new mode is dispatched **before** the hook is installed, and with no hook installed such a panic prints to stderr and leaves no files.
- `configure_bundled_gstreamer()` — reads `current_exe`, sets two process env vars. Safe, but unnecessary here.
- `configure_batch_memory()` — `GlobalMemoryStatusEx` into a static. Safe, but unnecessary here.
- The sequence-cache sweep is **not** before the dispatch: `kuvatin_video::sweep_sequence_cache` is called only inside the `Mode::SequenceMp4` arm and from `gui::run`.
- Settings are **not** loaded before the dispatch: `Settings::load()` has one caller, `gui/updates.rs`.
- No registry work happens before the dispatch: `ensure_registered()` is called only from `gui::run`.

- [x] **Step 1: Restructure `main()` so the new mode returns before the hook and the engine setup**

The mode is bound once, and the new arm returns before the three setup calls:

```rust
    let mode = cli.into_mode();
    // …see the comment in main.rs for the whole argument…
    if matches!(mode, Mode::UnregisterAllUsers) {
        std::process::exit(shell::unregister_all_users());
    }
    applog::install_panic_hook();
    configure_bundled_gstreamer();
    configure_batch_memory();
    match mode {
```

It is dispatched after `attach_parent_console()`, the argv repair, clap and `shell::set_quiet` (an atomic). **Trade-off, accepted:** a panic in the argv repair or in clap no longer reaches `crash.log`.

- [x] **Step 2: Make the `match` exhaustive**

```rust
        // Handled above, before the panic hook and the engine setup.
        Mode::UnregisterAllUsers => {
            unreachable!("--unregister-all-users is dispatched earlier")
        }
```

- [x] **Step 3: Build and run the suite**

Run: `cargo test -p kuvatin --release` and `cargo clippy -p kuvatin --all-targets -- -D warnings`

Two things verified by reading rather than by test: nothing reachable from the all-users path can raise a dialog (`notify_error`/`MessageBoxW` live only in `windows.rs`), because a message box in a deferred non-impersonated action would hang the uninstall invisibly in session 0; and std's `Stdout` is line-buffered unconditionally while `process::exit` runs `rt::cleanup()` → `io::cleanup()`, so no report line can be lost. The caveat on the second is that the release build is `windows_subsystem = "windows"`, so stdout reaches the log only because `WixQuietExec64` redirects the child's handles.

Note that `--quiet` does **not** quiet the stdout report. It is the installer's usual flag and it suppresses dialogs; the report is the whole point of the mode.

- [x] **Step 4: Commit**

Tasks 10, 11 and 12 landed together:

```
A switch asks for the all-users cleanup, and it runs before any profile
```

---

## Task 13: WiX — the SYSTEM custom action, sequencing and conditions

**Files:**
- Modify: `crates/kuvatin/wix/main.wxs`

**Context:** `WixUtilExtension`/`WixCA` are already linked (the file uses `<util:CloseApplication>`), so `WixQuietExec64` from the `WixCA` binary is available with no new `-ext`. For a **deferred** `WixQuietExec64` the command line is passed in a property whose Id equals the custom action's Id, set with `SetProperty` immediately before it. `[#exe0]` resolves to the installed path of the `File` with Id `exe0`.

- [x] **Step 1: Add the custom action and its command-line property**

Right after the `KuvatinUnregister` custom action and before the `<?ifdef SignCerPath?>` certificate block:

```xml
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

`Impersonate='no'` is not optional: `RegLoadKeyW` checks the effective token, and the backup/restore privileges are enabled on the process token. `Return='ignore'` so one stuck profile cannot abort the uninstall — which is also why the exit code is only for whoever reads the log.

- [x] **Step 2: Sequence it in both branches of `InstallExecuteSequence`**

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

**Why `NOT UPGRADINGPRODUCTCODE`:** a major upgrade removes the old product with `REMOVE="ALL"` too. Without the guard every upgrade would strip other users' menus, and a right-click-only user would lose the menu until they next opened the GUI. A real uninstall leaves `UPGRADINGPRODUCTCODE` unset, so the action runs then. `KuvatinUntrustCert` now runs after the package is gone — the package must be removed before its certificate is untrusted.

- [x] **Step 3: Know how to read the action's output when CI fails**

`WixQuietExec64` writes the command line and every line the process prints into the **verbose** MSI log, so the run must use `/l*v` (Task 14c does). Search that log for `KuvatinUnregisterAllUsers` (the action and its property), `WixQuietExec64` (the wrapper's own lines) and `Kuvatin: ` (the report). A missing `Kuvatin: ` block means the action never ran — check the condition and the sequencing. Remember the action does **not** fire on a major-upgrade removal, only on a genuine uninstall. Exit codes 1 and 2 are the ones worth grepping when a machine comes back dirty. Review this task together with Task 14.

- [x] **Step 4: Verify the MSI still builds**

`candle` compiles both variants: the signed one with `-dSignCerPath`/`-dSignCerThumbprint`, the unsigned one with `-dVersion -dCargoTargetBinDir -dMsixPath -dGstStageDir`.

- [x] **Step 5: Commit**

```
Uninstall cleans every account, not just the uninstalling one
```

---

## Task 14: CI — throwaway signing key, the offline test by name, the every-account uninstall test

**Files:**
- Modify: `.github/workflows/release.yml`

### 14a. Sign every packaging run, with a throwaway key when there is no release key

- [x] **Step 1: Replace the unsigned `else` branch**

Windows refuses an unsigned package that hosts a COM server, so an unsigned package never registers, and everything that depends on a registered package went unproven until a tag. **The `SIGNED=false` path is gone.** A tag run signs with the release key from the release-signing environment; every other packaging run — installer pull requests, manual dispatch — now makes a throwaway self-signed key inside the step, signs with it, hands the same certificate to WiX, and destroys the key before the step ends.

The recipe: `New-SelfSignedCertificate` with subject `CN=Ville Mattila` (it must equal the manifest Publisher exactly, so the package family name is the one a release produces), exported to a PFX in `RUNNER_TEMP` with a masked GUID password; the msix built with `-Publisher $throwaway.Subject`; signed with the newest SDK `signtool` (no `/tr` — this signature is not meant to outlive the run, and a timestamp server is one more thing that can be down); the `.cer` exported to the same path the real branch uses, so the `if ($cer)` block passes `SignCerPath`/`SignCerThumbprint` to WiX unchanged and `KuvatinUntrustCert` removes it again by thumbprint. A `finally` deletes the PFX and the store entry even when signing threw, and keeps the `.cer`, which the WiX build and the install test both need.

A tag with no key still throws.

- [x] **Step 2: Update the step's own comments**

Both the packaging-on-pull-requests comment and the signing block's comment now say the package is always signed and why.

### 14b. Run the offline-hive test by name, and fail if it skips

- [x] **Step 3: Add the step**

```yaml
      - name: Test (offline hive cleanup — gates the release)
        shell: pwsh
        run: |
          cargo test -p kuvatin --release -- --exact shell::hive::tests::offline_cleanup_removes_only_kuvatin_verbs --nocapture
          if ($LASTEXITCODE -ne 0) { throw "the offline hive test exited $LASTEXITCODE, so the signed-out cleanup path is unproven" }
```

**The exit code is the whole gate. Nothing greps the output.** The test panics rather than skipping when `CI` is set (GitHub sets `CI=true`, and the hosted runner is elevated), so a skip here is a failed step. `--nocapture` is only so the reason is readable when it does fail. `hive.rs`'s own comment says the same thing, having previously claimed the workflow looks for the skip line.

### 14c. Replace the single-user uninstall check with an every-account test

- [x] **Step 4: Delete the old uninstall tail**

The install test now leaves Kuvatin installed; the next step uninstalls it, after two more accounts have registered it.

- [x] **Step 5: Add the every-account uninstall step**

`Uninstall test (every account)`, gated on `PACKAGE == 'true'`. Two extra local accounts, `kvoff` and `kvon`, register Kuvatin the way a real first launch would: a password-logon scheduled task loads the user's profile, so HKCU is their own classes hive and the sparse package registers in their context. A fresh account has no "Log on as a batch job" right, so `secedit` grants it first — without that the tasks never start (0x41303) — adding to whatever the runner image already lists rather than replacing it, and writing the `.inf` back as Unicode, which is all `secedit` reads.

`kvon` stays signed in across the uninstall (a `kv-hold` task sleeping for 900 s), so its hive is the **loaded** case. `kvoff` signs out when its task ends, and the step waits up to 180 s for the profile service to release `HKEY_USERS\<SID>`, which is what makes it the **offline** case. Each account's task writes a `Start-Transcript` the runner echoes.

Preconditions first, so the test cannot pass vacuously: each account must have at least 16 of the 17 verb keys (the legacy `image` root is absent on a fresh install), a registered package, a log, a cache and presets. Then `msiexec /x … /l*v`. Then: no account keeps a verb key, a log, `%TEMP%\kuvatin` or package data; the package is registered for nobody; the certificate is gone from Trusted People; presets are kept; and the SYSTEM pass left nothing in `C:\Windows\System32\config\systemprofile\AppData\Local\Kuvatin`.

Details that are easy to break: key counting uses `reg.exe` only, because a PowerShell provider handle would block the hive unload; `$global:LASTEXITCODE` is reset where a missing key exits 1; `$PSNativeCommandUseErrorActionPreference = $false` because `net user`, `secedit` and `wevtutil` are chatty. `reg query … /f X /k` needs `/s` to search at all. On any failure a `catch` prints the MSI log's `KuvatinUnregisterAllUsers`/`Kuvatin: ` lines and the Task Scheduler events for the account tasks, which between them answer "did the SYSTEM action run, and did the accounts do their half?". Cost: about 2–5 minutes inside the 75-minute cap.

Two real defects were caught locally before CI by parsing every edited `run:` block with the PowerShell 5.1 parser: an em dash inside a `throw` string, and `"$LASTEXITCODE: …"` being read as a scope-qualified variable. Keep doing that.

- [x] **Step 6: Commit all three edits together**

```
CI signs the package it tests, so uninstall is proven for every account
```

### 14d. A gate that would prove nothing now says so instead of passing

- [x] **Step 7: Tighten nine assertions across both steps, so a gap fails loudly instead of passing quietly**

Three of these checks could pass while measuring less than they claimed. `cargo test -- --exact <name>` matching **zero** tests exits `0`, the same as matching and passing one, so a rename could have turned the offline-hive gate off in silence; the step now captures the harness's stdout (stderr, carrying cargo's own progress and any compile error, still streams straight into the log) and throws unless it contains `1 passed`, in addition to the exit-code check already there — verified by deliberately misspelling the test name locally and watching the new assertion fire where the old one would have stayed green. Nothing previously asserted that the all-users action had actually run or that the two accounts were in the states the step claims to exercise: `kvon`'s classes hive is now polled for up to 60 s and the step throws if it never mounts, so the loaded-hive path cannot go unproven unnoticed, and the uninstall log must carry a `Kuvatin: ` line afterwards or the step throws, since without one `KuvatinUnregisterAllUsers` never fired and every assertion below it would be measuring the ordinary per-user uninstall instead. When `kvoff`'s hive is still mounted after the 180 s wait — which the probe found is the usual outcome — that is a `::warning::` rather than a failure, but it says plainly that both accounts exercised the loaded path this run and that the offline path is proven by the unit test instead.

Every package assertion in both steps reads `SIGNED -eq true -and …`, so an unsigned run would not fail them, it would skip them. Both steps now throw unless `SIGNED` is `true`, and the install test's package assertions (build ≥ 22000, the certificate shipped, the package registered) are unconditional rather than gated behind `$build -ge 22000 -and $env:SIGNED -eq 'true'` — so a runner image older than build 22000 now fails the release instead of quietly skipping the package half, which is safe only because 14a means every packaging run signs and the hosted runner is always build ≥ 22000.

Smaller changes in the same spirit: a swallowed `reg unload` failure — which leaves a hive mounted that the SYSTEM action's own `RegLoadKeyW` then cannot open, reading back as a missing profile — now warns with the exit code instead of vanishing; the throwaway certificate is removed with `-DeleteKey`, or its CNG key container stays behind after the certificate is gone; `secedit`'s grant of the batch logon right is read back from the exported `.inf` and both accounts' SIDs must appear in it, because `secedit` reports success on stdout without necessarily granting anything; the as-user script throws on a non-zero `--register`/`--preset` exit, naming which one failed where it happened rather than leaving a task result of `1` for a precondition check to trip over three steps later; and the `kvoff` wait now polls `<sid>_Classes`, the key the code actually branches on, rather than the plain `HKEY_USERS\<SID>` mount.

The WiX comment for `KuvatinUnregisterAllUsers` (four lines, `crates/kuvatin/wix/main.wxs`) now records that the action has no rollback pair, same as `KuvatinUnregister` above it: a rolled-back uninstall brings the product back with every account's menu entries still removed, until each of those users next launches Kuvatin and it registers them again.

- [x] **Step 8: Commit**

```
A gate that would prove nothing now says so instead of passing
```

---

## Task 15: Docs — README and CHANGELOG

**Files:**
- Modify: `crates/kuvatin/wix/README.md`, `CHANGELOG.md`

- [x] **Step 1: Update the README's "Registration scope" section**

The "**Uninstall is best-effort:**" bullet became "**Uninstall cleans every account:**", which describes the two actions, the loading of a signed-out account's `UsrClass.dat`, that presets and settings are kept, that registry keys are deleted through handles that never follow a symbolic link and files through a walk that never follows a junction, the `NOT UPGRADINGPRODUCTCODE` skip, and where to read the report (`msiexec /x ... /l*v uninstall.log`, under the `KuvatinUnregisterAllUsers` action, every line prefixed `Kuvatin:`).

- [x] **Step 2: Update the CHANGELOG (CRLF)**

`CHANGELOG.md` is CRLF throughout and `core.autocrlf` is on. A bullet was added as the last item of the `### Fixed` list under `## [Unreleased]`:

```markdown
- Uninstalling now removes Kuvatin's right-click menu, its Windows 11 menu
  package and its leftover logs and cache for every account on the PC, not just
  the account that runs the uninstaller. Your saved presets are kept.
```

Run: `git diff --stat CHANGELOG.md` — `1 file changed, 3 insertions(+)`, no deletions (a deletion count would mean the line endings were rewritten).

- [x] **Step 3: Commit**

```
The changelog and the installer README say uninstall covers every account
```

A final pass over the comments landed after it: `Each of these comments now says what the code really does`.

---

## How a test is allowed to skip

Several tests here can only prove what they prove if the environment lets them build their attack — a junction, a Deny ACE, an elevated token. A test that cannot do that must not pass in silence, and on CI it must not skip at all, or the gate is not running the thing it gates. One rule, in one place:

```rust
/// Prints `skipping: <reason>` locally; panics when `CI` is set.
#[track_caller]
pub(super) fn skip_or_fail_on_ci(reason: &str);

/// The same line, for the one kind of skip that is honest everywhere: the
/// setup worked, and the test then MEASURED that the condition it needs cannot
/// hold in this process. Never where it merely failed to arrange it.
#[track_caller]
pub(super) fn skip_even_on_ci(reason: &str);
```

`CI` counts as set only when it is non-empty and not `false`: some local tooling exports `CI=false` to mean the opposite of what `var_os(..).is_some()` would read it as, and a developer whose shell does that would find every one of these tests failing for a reason nothing on screen explains. The line is `println!` rather than `eprintln!` so `--nocapture` shows it in order with the test names. **The release workflow relies on the exit code, not on the text** — no step greps for `skipping:`.

**The `SeBackupPrivilege`-versus-deny-ACE interaction**, which is why `skip_even_on_ci` exists at all. `hive::tests::offline_cleanup_removes_only_kuvatin_verbs` enables `SeBackupPrivilege` for the whole process, and it stays enabled. Two other tests deny themselves access to a file and expect to be refused — and under that privilege a handle opened with backup semantics is granted over the top of any DACL, so on an elevated runner they would pass or fail by thread timing. Measured precisely: `metadata` and `canonicalize` issue the same zero-rights backup-semantics open and fail with error 5 under a deny; `symlink_metadata` succeeds anyway, via std's `FindFirstFileExW` fallback on `ERROR_ACCESS_DENIED`; `canonicalize` has no such fallback. So each of those tests **probes first** — `canonicalize(..).is_ok()`, or `open_no_follow(..).is_ok() && is_elevated()` — and calls `skip_even_on_ci` when the deny cannot bite in this process. That is a fact about the process, not a shortcoming of the machine, so no runner could do anything about it and failing there would be noise. Not being able to *set* the ACE is a different thing and remains `skip_or_fail_on_ci`.

The registry has no such second failure mode: an ACE that is set always bites, because `SeBackupPrivilege` lifts a DACL only for a handle that asks for it and nothing in this crate passes `REG_OPTION_BACKUP_RESTORE`.

**Junction and reparse tests build their own artifacts in temp directories and clean up on failure.** `Junction` and `TempTree` are RAII guards; `Junction::drop` uses `symlink_metadata` rather than `exists` (which follows a junction and answers `false` for a dangling one) and `remove_dir`, which unlinks the link and never its target. Nothing depends on `remove_dir_all`'s behaviour against a junction — measured on rustc 1.96, it removes the link and leaves the target, but `remove_dir` says what we mean.

The twenty-two tests that can skip, and what each needs:

| Test | Needs |
| --- | --- |
| `regutil::tests::a_denied_key_notify_on_an_intermediate_does_not_block_the_delete` | a Deny ACE on a scratch key |
| `regutil::tests::a_key_we_may_not_read_is_not_reported_as_not_a_link` | a Deny ACE on a scratch key |
| `regutil::tests::a_key_we_may_not_open_is_not_reported_as_empty` | a Deny ACE on a scratch key |
| `verbs::tests::a_hive_we_cannot_read_still_lists_the_keys_we_know` | a Deny ACE on `scratch\SystemFileAssociations` |
| `verbs::tests::a_candidate_we_may_not_open_is_listed_and_reported` | a Deny ACE on a candidate key |
| `verbs::tests::a_candidate_refused_at_both_ends_is_counted_refused_not_absent` | a Deny ACE denying `DELETE_RIGHT` too |
| `profiles::tests::a_junction_is_not_a_profile_directory` | `mklink /J` |
| `hive::tests::offline_cleanup_removes_only_kuvatin_verbs` | an elevated token (SeBackup/SeRestore) |
| `hive::tests::a_junction_on_the_way_to_the_hive_is_refused` | `mklink /J` |
| `hive::tests::a_junction_where_the_profile_directory_should_be_is_refused` | `mklink /J` |
| `hive::tests::a_hive_file_we_may_not_read_is_refused_by_name` | `icacls /deny`, **and** the deny to bite in this process |
| `files::tests::a_junction_on_the_way_to_a_tree_is_refused_and_nothing_behind_it_is_touched` | `mklink /J` |
| `files::tests::a_junction_where_a_tree_should_be_is_unlinked_not_followed` | `mklink /J` |
| `files::tests::a_junction_nested_inside_a_tree_is_unlinked_with_the_tree` | `mklink /J` |
| `files::tests::a_junction_where_the_prune_target_should_be_is_refused` | `mklink /J` |
| `files::tests::a_reparse_point_where_a_file_should_be_is_left_alone` | `mklink /J` |
| `files::tests::a_directory_we_hold_can_still_be_turned_into_a_junction` | `FSCTL_SET_REPARSE_POINT` |
| `files::tests::a_parent_converted_after_the_walk_deletes_nothing` | `FSCTL_SET_REPARSE_POINT` |
| `files::tests::a_parent_converted_and_reverted_around_the_leaf_open_deletes_nothing` | `FSCTL_SET_REPARSE_POINT` and `FSCTL_DELETE_REPARSE_POINT` |
| `files::tests::a_parent_converted_and_reverted_around_a_tree_deletes_nothing` | `FSCTL_SET_REPARSE_POINT` and `FSCTL_DELETE_REPARSE_POINT` |
| `files::tests::a_file_we_may_not_delete_says_so_by_name` | `icacls /deny`, **and** the deny to bite in this process |
| `package::tests::enumerating_every_account_hands_back_only_our_package` | an elevated token and a deployment service that starts |

Under `CI=1` locally, exactly one of these fails — the offline hive test, because a developer shell is not elevated — and nothing skips silently.

---

## Where every test runs in CI (so nothing passes by skipping)

| Test | Task | CI location | Runs when |
| --- | --- | --- | --- |
| `shell::regutil::tests::*` (14) | 1 | `Test (core + gui — gates the release)` | every push/PR/tag/dispatch; 3 self-skip only if a Deny ACE cannot be set, which fails the build under `CI` |
| `shell::verbs::tests::*` (9) | 2, 3 | same gate | always; 3 self-skip on the same rule. Includes the drift test `the_shared_list_is_the_per_user_key_set` |
| `shell::windows::tests::*` (5) | 3 | same gate | always. Includes `every_root_points_at_a_store_the_uninstall_removes` |
| `shell::profiles::tests::*` (5) | 4 | same gate | always; the junction test self-skips only where `mklink /J` is refused |
| `shell::hive::tests::*` (10) | 5 | the gate **and** the dedicated `Test (offline hive cleanup — gates the release)` step | every push/PR/tag/dispatch. The dedicated step runs `offline_cleanup_removes_only_kuvatin_verbs` by exact name and gates on **exit code and `1 passed`**: the test panics instead of skipping when `CI` is set, so a skip is a failed build, and a rename that matched zero tests would still exit `0` — which is why the captured stdout must also show one test passing |
| `shell::paths::tests::*` (10) | 6 | the core gate | always — none of them skip |
| `shell::files::tests::*` (21) | 7 | the core gate | always; 10 self-skip only where a junction or a reparse conversion is refused, which fails the build under `CI` |
| `shell::package::tests::*` (16) | 8 | the core gate | always; `enumerating_every_account_hands_back_only_our_package` needs an elevated token |
| `shell::allusers::tests::*` (19) | 9 | the core gate | always — pure functions over hand-built structs, so nothing to skip |
| `cli::tests::unregister_all_users_flag` | 11 | the core gate | always |
| clippy over all targets | 10, 12 | `Clippy (warnings are errors)` | always |
| Every-account uninstall, end to end (verbs + package + files + presets kept + no system-profile leftover, for the runner, a signed-in account and a signed-out-or-still-loaded one) | 13, 14c, 14d | `Uninstall test (every account)` | when `PACKAGE == 'true'`: tags, manual dispatch, and PRs touching the installer. Throws unless `SIGNED == 'true'` (which 14a now makes true on those runs too), unless `kvon`'s hive is actually loaded, and unless the uninstall log carries a `Kuvatin: ` line; warns rather than fails when `kvoff`'s hive is still mounted, since the offline `UsrClass.dat` path is then proven by `shell::hive::tests::*` instead |

**The rule, in one line: a test that skips under `CI` fails the build.** That is enforced inside the tests, by `skip_or_fail_on_ci`, and not by any step reading their output — the one exception being `skip_even_on_ci`, which is used in exactly two places and only where the test has *measured* that the condition it needs cannot hold in this process.

The signed-out hive is the one thing a live account on the runner cannot reproduce, which is why Task 5's test exists and why 14b refuses to let it skip. Package removal for a signed-out account remains the accepted gap recorded in the orientation.

---

## Self-review notes (checked against the coordinator's brief)

1. Pure unit-tested parts — the SID filter (Task 4), the shared key list including the `SystemFileAssociations\*` enumeration (Tasks 2, 3, 5), the per-profile path plan with the "never the whole `Local\Kuvatin`" guard (Task 6), and all of the orchestrator's wording (Task 9). ✓
2. Registry part — loaded hive edited in place, otherwise `RegLoadKeyW` under a private name, delete, unload with retries; symbolic links refused at every segment and every delete made through a handle (Tasks 1, 5); in-crate offline test that CI runs by name and cannot silently skip (Tasks 5, 14b). ✓
3. Package part — `FindPackages` filtered by name, `RemovePackageWithOptionsAsync(RemoveForAllUsers)` with a bounded per-operation wait, deprovision only when provisioned, leftovers re-asked rather than inferred (Task 8). ✓
4. File part — a walk that opens every component relative to its parent's handle with `OBJ_DONT_REPARSE`, deletes through handles, includes `Packages\<family>` per profile, prunes parents only when empty, and is pinned by regression tests that perform the real conversion attack (Tasks 6, 7). ✓
5. `main.rs` switch, stdout only, never `applog`, dispatched before the panic hook with the pre-dispatch audit written down (Tasks 11, 12). ✓
6. WiX — `SetProperty` + `WixQuietExec64` deferred `Impersonate='no'` `Return='ignore'`, sequenced after `KuvatinUnregister` and before `KuvatinUntrustCert`, `NOT UPGRADINGPRODUCTCODE`, util extension already linked, plus how to read the action's output in the MSI log (Task 13). ✓
7. CI — every packaging run signed so the package paths are proven on a pull request rather than first on a tag; the offline-hive test gated by exit code and a passed-count check; the every-account uninstall with the probe's proven recipe (secedit batch right, password tasks, `reg.exe`-only hive checks, `$global:LASTEXITCODE` resets, native-exit-code guard), hard assertions that the all-users action ran and that the signed-in and signed-out accounts were in the states the step claims, `Packages` folder asserted gone, presets asserted kept (Task 14). ✓
8. Docs — README "Registration scope" and CHANGELOG Unreleased→Fixed in CRLF (Task 15). ✓
9. Residual risks written down rather than implied away — hard links, reparse points above the profile, the `RegLoadKeyW` window, files another process holds open, a planted `Packages` junction left in place, and a signed-out account's package removal resting on Windows' own `RemoveForAllUsers` (the orientation's "Known gaps, accepted", and each module's own documentation). ✓
