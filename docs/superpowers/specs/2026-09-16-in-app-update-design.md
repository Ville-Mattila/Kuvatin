# Kuvatin — In-App Update Design

> **For agentic workers:** This is the validated design (spec) for updating
> Kuvatin from the badge in its own window. The next step is the
> `writing-plans` skill. It builds on the opt-in update check that already
> ships (`crates/kuvatin/src/update.rs`); that check's behaviour does not
> change here.

## Goal

Clicking the "Update x.y.z available" badge installs that version. The user
confirms once, watches a download, and the app closes, updates and reopens on
the new version. Nothing is downloaded before the confirmation, nothing is
installed that does not match its published checksum, and every failure leaves
a message that says what happened and offers the download page instead.

## Background — current state

- **The check.** `update::latest_version()` does one WinHTTP `HEAD` of
  `https://github.com/Ville-Mattila/Kuvatin/releases/latest` with redirects
  disabled and reads the tag out of the `Location` header. No crates beyond
  the `windows` bindings, no API quota, no JSON.
- **The wiring.** `crates/kuvatin/src/gui/updates.rs` owns the Settings
  toggle, "Check now", the status line, and the badge. The check runs on a
  worker thread at most once a day and stores `latest_seen` in settings.
- **The badge.** `crates/kuvatin/ui/app.slint:528` shows a pill in the top
  right when `update-available` is set. Its only action is
  `open-releases()`, which calls `ShellExecuteW` on the releases page.
- **What a release publishes.** For version `V`, the assets are
  `kuvatin-V-x86_64.msi`, `kuvatin-V-x86_64.msi.sha256`, a fixed-name copy
  `kuvatin-x86_64.msi` with its own checksum file, and the runtime bill of
  materials. The checksum file is one line, `<64 hex>  <file name>`, written
  with a single `\n`.
- **The installer.** Per-machine, so installing needs elevation. It is
  **unsigned**, by decision (backlog `pk-msi`). Its `MajorUpgrade` carries
  `AllowSameVersionUpgrades='yes'`, so a plain `/i` of a newer package
  upgrades in place. Installing while Kuvatin runs hits the files-in-use
  dialog (backlog `pk-cond`), so the app must be gone before `msiexec` starts.
- **Unsaved work.** Nothing in the app tracks it. `Project` keeps a `dirty`
  flag (`crates/kuvatin-video/src/project.rs:742`) that is set on every edit
  but never read outside the crate, and the Open Project path asks for
  confirmation using "are there any clips" as a stand-in.

## Guiding decisions (locked during brainstorming)

1. **Confirm before anything is fetched.** A click opens a dialog naming both
   versions, with a link to the release notes. A misclick costs nothing.
2. **Install immediately, not on next quit.** The app closes, the installer
   runs, the app reopens. No staged-for-later state to get wrong.
3. **The checksum is the only check available, and that is accepted.** The
   installer is unsigned and the checksum sits beside it on the same server,
   so this proves the download arrived intact, not that the release is
   genuine. Written down here so it is not mistaken for an authenticity check.
4. **No new crates.** The download uses WinHTTP, as the check already does,
   and the hash uses Windows CNG (`BCrypt*`), which needs only the
   `Win32_Security_Cryptography` feature added to the existing `windows`
   dependency.
5. **The updater is the same executable.** No second binary to build, ship or
   sign: a copy of `kuvatin.exe` run from the staging folder with a new flag
   does the waiting, installing and relaunching.

## Architecture

> **Superseded during implementation.** The section below assumed the updater
> could be a copy of `kuvatin.exe`. It cannot: the app statically imports seven
> GStreamer and GLib libraries that the installer puts beside it, so a lone copy
> in the staging folder will not start. Pointing it at the install folder for
> them is worse, because it would hold open the files the installer must
> replace. The updater is a separate small program instead; see Tasks 18 and 19
> of the plan. Everything else here stands.

### Why a copy of the executable

`msiexec` cannot replace `kuvatin.exe` while that file is running, and the
process driving the install must outlive the app it is replacing. Copying the
running executable into the staging folder and starting **that** copy solves
both: the installed path is free, and the copy is not touched by the upgrade.
The copy exits as soon as it has relaunched, and the staging folder is swept
on the next start.

### Module layout

`crates/kuvatin/src/update.rs` becomes `crates/kuvatin/src/update/` with four
files, each with one job:

| File | Holds |
| --- | --- |
| `mod.rs` | The existing check: `latest_version`, `parse_tag`, `is_newer`, `open_releases_page`, `RELEASES_URL`, `CURRENT`, `CHECK_INTERVAL_SECS`. Its public API does not change, and its existing tests move with it. Also `asset_urls`. |
| `fetch.rs` | WinHTTP `GET` with redirects followed: to a file with progress and cancellation, or to a short string. |
| `verify.rs` | SHA-256 of a file via CNG, and reading the expected hash out of a checksum file. |
| `apply.rs` | The staging folder, the download-and-verify step, launching the helper, the helper's own run, and the startup sweep. |

### `mod.rs` — addresses

```rust
/// The installer and its checksum file for a published version.
pub fn asset_urls(version: &str) -> (String, String);
/// `kuvatin-2.13.0-x86_64.msi`
pub fn asset_name(version: &str) -> String;
```

Both are built from
`https://github.com/Ville-Mattila/Kuvatin/releases/download/v<version>/`.
The versioned name is used, never the fixed-name copy: the fixed name would
make the checksum file ambiguous about which build it describes.

### `fetch.rs` — download

```rust
pub struct Progress { pub done: u64, pub total: Option<u64> }

/// GET `url` into `dest`, following redirects. Calls `on_progress` as bytes
/// land and stops early when `cancel` is set, removing the partial file.
pub fn get_to_file(
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<()>;

/// GET `url` as text, refusing anything past `limit` bytes.
pub fn get_to_string(url: &str, limit: usize) -> Result<String>;
```

- The URL is split with `WinHttpCrackUrl`, so the redirect to
  `objects.githubusercontent.com` needs no special handling.
- Redirects are followed; the check's `WINHTTP_DISABLE_REDIRECTS` is not set
  here.
- Anything but HTTP 200 is an error naming the status.
- Bytes are read in 64 KiB chunks with `WinHttpReadData`. `Content-Length`
  fills `Progress::total` when present; the bar is indeterminate when it is
  not.
- A hard ceiling of 200 MB stops a runaway response from filling the disk.
- Timeouts match the check: five seconds each for resolve, connect, send and
  receive.

### `verify.rs` — checksum

```rust
/// Lower-case hex SHA-256 of a file, hashed in chunks through CNG.
pub fn sha256_file(path: &Path) -> Result<String>;

/// The hash a `.sha256` file gives for `asset_name`, if it names it and the
/// digest is 64 hex characters.
pub fn expected_hash(checksum_file: &str, asset_name: &str) -> Option<String>;
```

`expected_hash` accepts the `<hex>  <name>` shape the pipeline writes, ignores
leading and trailing space, and returns `None` for a file that names something
else, carries a short or non-hex digest, or is empty. Comparison is
case-insensitive on the hex.

### `apply.rs` — staging, handing off, and the helper

```rust
pub struct Staged { pub msi: PathBuf, pub helper: PathBuf }

/// `%LOCALAPPDATA%\Temp\kuvatin\update`.
pub fn stage_dir() -> Result<PathBuf>;

/// Download the installer and its checksum, verify, and copy this executable
/// in beside it. Removes everything it wrote if any step fails.
pub fn stage(
    version: &str,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Staged>;

/// Start the staged copy as the updater. The caller quits immediately after.
pub fn hand_off(staged: &Staged) -> Result<()>;

/// The `--apply-update` mode: wait, install, relaunch. Returns an exit code.
pub fn run_helper(msi: &Path, after: u32, relaunch: Option<&Path>) -> i32;

/// Delete the staging folder; called once at startup.
pub fn sweep_stage();
```

The staging folder sits under `%TEMP%\kuvatin`, the tree the every-account
uninstall already deletes, so a machine that never runs Kuvatin again is still
left clean.

`hand_off` starts the copy with:

```
kuvatin.exe --apply-update <msi> --after <pid> --relaunch <exe>
```

`--relaunch` is optional; without it the helper installs and stops, which is
what the pipeline test uses.

`run_helper` in order:

1. Open `after` with `SYNCHRONIZE` and wait up to 30 seconds. A process that
   cannot be opened has already gone, which is success, not failure.
2. Run `msiexec /i "<msi>" /qb /norestart` and wait for it. `/qb` shows a
   small progress window, so an elevation prompt and a long install are not a
   silent freeze.
3. Treat exit 0 and 3010 (reboot needed later) as installed. 1602 and 1223
   are the user declining; 1603 and everything else are failures.
4. On success, start `--relaunch` through `ShellExecuteW` if it was given.
5. Delete the installer. The copy cannot delete itself, so the folder is left
   for `sweep_stage` on the next start.
6. Log every step to `kuvatin.log`. On failure, also show a message box
   naming the error and the releases page, because by then no window is left
   to show it in.

### Command line

`crates/kuvatin/src/cli.rs` gains `--apply-update <PATH>`, `--after <PID>` and
`--relaunch <PATH>`, and `Mode::ApplyUpdate { msi, after, relaunch }`.
`main.rs` dispatches it like the other non-interface modes, but after the
panic hook is installed, since this one runs as the user with a normal profile
(unlike `--unregister-all-users`, which must not touch the profile at all).

## Interface

One dialog, built on the existing `modal.slint` shell, driven by a phase:

```slint
in property <int> update-phase;        // 0 none, 1 confirm, 2 working, 3 failed
in property <float> update-progress;   // 0..1, negative when the size is unknown
in property <string> update-step;      // "Downloading 12.4 of 34.4 MB", "Checking the download"
in property <string> update-error;
in property <bool> update-unsaved;     // the timeline has unsaved changes
callback update-start();
callback update-cancel();
callback update-dismiss();
callback update-save-first();
```

- **Phase 1, confirm.** "Kuvatin x.y.z is available. You have a.b.c." A link
  reading "What is new" calls the existing `open-releases()`. Buttons: update
  and restart, and not now. When `update-unsaved` is set, a line says the
  timeline has changes that closing will lose, and a third button saves first
  and closes the dialog, leaving the badge to be clicked again.
- **Phase 2, working.** `update-step` over a progress bar, with cancel.
  Cancel sets the flag the download watches and returns to no dialog.
- **Phase 3, failed.** `update-error` in plain words, a button that opens the
  download page, and a button that dismisses.
- After a successful hand-off the dialog reads "Closing to install" and the
  app quits, so there is no fourth phase to get stuck in.

The badge's click calls a new `update-open()` instead of `open-releases()`.
`accessible-label` changes to say it opens the update dialog rather than the
download page.

`update-unsaved` needs the flag that already exists to be readable:
`kuvatin-video` gains `pub fn is_dirty(&self) -> bool` over `Project::dirty`.
The Open Project path keeps its own stand-in; changing that is out of scope.

## Failure handling

| What happens | What the user sees | State afterwards |
| --- | --- | --- |
| No network, or GitHub unreachable | "Could not reach the download: \<reason\>" | App untouched, nothing written |
| Asset missing (404) | "Version x.y.z has no installer published" | App untouched |
| Checksum file missing or names another file | "Could not check the download" | Downloaded file deleted |
| Hash mismatch | "The download did not arrive intact" | Downloaded file deleted |
| Cancelled | Dialog closes | Partial file deleted |
| Disk full or staging not writable | "Could not write to \<folder\>: \<reason\>" | Whatever was written is removed |
| Elevation declined | Message box from the helper | Old version still installed, app relaunched |
| Install failed | Message box naming the msiexec exit code | Old version still installed |
| Relaunch failed | Message box | New version installed, start it from the Start menu |

Every message in this table is ASCII, for the same reason the uninstall report
is: it can end up somewhere that does not read UTF-8.

## Testing

**Unit, no network:**

- `asset_urls` and `asset_name` for a version, including that the versioned
  name is used rather than the fixed-name copy.
- `expected_hash`: the real shape, a file naming a different asset, a short
  digest, non-hex, empty, and trailing whitespace.
- `sha256_file` against known answers: the empty file, and a file whose digest
  is written into the test.
- Command line parsing of the three new flags, including `--relaunch` absent.
- `stage_dir` ends in `Temp\kuvatin\update`, so it stays inside the tree the
  uninstall sweep removes.
- `sweep_stage` removes the folder and tolerates it being absent.
- The msiexec exit-code rule: 0 and 3010 installed, 1602 and 1223 declined,
  anything else failed.

**Network, `#[ignore]` as the existing one is:** fetch the current release's
checksum file and assert `expected_hash` reads it.

**Pipeline:** the packaging job already installs and uninstalls the built
installer on the runner. A step after that install drives the real path
against that same installer: copy the installed executable to a temporary
folder, run it with `--apply-update <the built msi> --after <a process that
has exited>` and no `--relaunch`, then assert it exits 0 and the product is
still registered. That exercises the copy, the wait, the elevation-free
SYSTEM-less install and the exit codes without needing a newer release to
exist. The relaunch is left out on purpose so no window opens on the runner.

## Risks and open questions

- **The installer is unsigned.** The elevation prompt will say the publisher
  is unknown. Nothing in this design changes that; signing the installer is
  backlog `pk-msi` and was deliberately deferred.
- **Downgrade and sideways moves.** The badge only appears when `is_newer`
  says so, and `run_helper` does not second-guess it. A release that is pulled
  after the check ran ends as a 404, handled above.
- **Two Kuvatins running.** The helper waits for the process that started it,
  not for every instance. A second window open during the update hits the
  files-in-use dialog inside `msiexec`'s own UI. Acceptable: `/qb` shows it,
  and the fix is to close the other window.
- **Unsaved Images work.** The crop list is not covered by the dirty flag, so
  the warning is about the timeline only. Out of scope to widen here.

## Out of scope

- Signing the installer, and anything that depends on a signature.
- Background or silent updating without a click.
- Delta updates; the installer is downloaded whole.
- Changing the check itself: its schedule, its opt-in, or its settings.
- Teaching the Open Project path to use the dirty flag.
