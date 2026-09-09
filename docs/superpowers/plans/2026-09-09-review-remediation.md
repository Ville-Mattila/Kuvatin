# Review remediation (from the 2026-09-08 codebase review)

> Executes the phased plan in `docs/superpowers/audits/2026-09-08-kuvatin-review.html`
> (66 findings at v2.6.0). Each phase is a self-contained, releasable step;
> finding ids (C1, H1, M9 …) refer to that report.

## §0 Comply & slim — C1, H11

- **Licenses in the MSI (C1).** `bundle-gstreamer.ps1` now copies upstream's
  `share\licenses\` (82 component folders — every LGPL/GPL/BSD text and
  copyright notice, exactly as GStreamer ships them) into the staging dir, so
  `heat` harvests them into `[APPLICATIONFOLDER]\licenses\`. The script also
  generates `THIRD-PARTY-NOTICES.txt` (what is bundled, the license families,
  and the corresponding-source URLs: GStreamer 1.26.11 modules at
  gstreamer.freedesktop.org/src, Cerbero tag 1.26.11 for every third-party
  library, plus an on-request offer via the issue tracker). The source tree
  gains `THIRD-PARTY-NOTICES.md` and a README section; `deny.toml` +
  `cargo deny check licenses` police the Rust side.
- **Trimmed bundle (H11).** Instead of every DLL, the script stages an
  **allow-list of plugins** and computes the **flat-DLL closure by walking PE
  import tables** (regular + delay-load; pure PowerShell, no dumpbin) from
  `kuvatin.exe` and the staged plugins, keeping only DLLs that exist in
  `bin\`. `-AllPlugins` restores the old behaviour.
  - The allow-list = the union of (a) every plugin actually loaded — captured
    with `GST_DEBUG=GST_PLUGIN_LOADING:4` and a private registry across the
    whole video test suite, the exe's sequence render, and discovery +
    playback of mp4/m4v/mov/mkv/webm/avi/wmv/hevc, mp3/wav/flac, png/jpg/bmp
    — and (b) elements GES/playbin request lazily or on other hardware
    (`gstx264` for non-NVIDIA machines, `gstwasapi`/`gstdirectsound`
    fallbacks, `gstogg`/`gstflv`/`gstdav1d`, tag readers).
  - Validation: with **only the staged directory on PATH** (no SDK), a private
    registry and `GST_PLUGIN_PATH` at the staged plugins, the video suite
    passes 26/26, the exe renders a sequence to MP4, and discovery succeeds
    for every fixture except TIFF — which fails identically with the full
    runtime (pre-existing; TIFF isn't a video-mode input anyway).
  - Result: 391 DLLs / ~270 MB staged → **125 DLLs / 60 MB** (69 plugins +
    56 libraries + licenses); the MSI shrinks from **106.8 MB to 32.8 MB**
    (verified by administrative extract: 82 license folders, 185 files,
    notices, 69 plugins, 56 libraries). Dropped: x265, a52dec, dts, amr, siren,
    openh264, SvtAv1, libsrt, libstdc++, gstpython, aws/elevenlabs/ndi/
    decklink/webrtc, …
  - CI builds the release exe before staging (the closure needs it).

## §1 Data safety — H1, H2, H7, M1–M4, L1, L3

- **H1** Folder-mode Convert plans every target through
  `pipeline::plan_unique_outputs` (in-batch + filesystem uniqueness) instead of
  per-file `ensure_unique`; same-stem inputs from different folders now get
  `-1`, `-2`, … `ensure_unique` is documented as single-output only.
- **H2** `decode_oriented` is public; the viewer preview and the thumbnails
  decode through it, so crops are drawn in the same (EXIF-oriented) pixel
  space the pipeline crops.
- **H7** An unreadable selection clears the viewer, drops the crop edit state
  and marks the row `unreadable`; thumbnails do the same instead of staying
  blank.
- **M1** `encode_png_lossy` borrows the pixels (`new_image_borrowed`) and on
  `QualityTooLow` retries with the floor dropped (`0..qmax`), then falls back
  to lossless — a noisy photo at quality 100 gets a file, not an error.
- **M2** `load_or_init` treats an unreadable file (UTF-16 re-save,
  permissions) like a corrupt one: back up, built-ins, warning — never `Err`.
- **M3** `Job` has struct-level `#[serde(default)]`; `parse_tolerant` runs a
  document-level `migrate_document` on the raw TOML BEFORE typed parsing.
- **M4** The GUI builds output names with `naming::output_file_name`
  (sanitized suffix) for both the Save dialog default and folder mode.
- **L1** `write_unique` removes a half-written file on a write error.
- **L3** Preset `save` renames over the target (no delete-first window) with a
  per-process temp name.
- Tests: quality-floor fallback on xorshift noise; `Job` from a TOML missing
  fields; unreadable presets file → built-ins + warning; missing job fields in
  a preset entry parse cleanly; save twice leaves no temp files.

## §2 Engine guards — H3, H4, M7–M10, M14, L14, L15

- **H3** `Project` carries a `rendering` flag (set by `begin_render`, cleared
  by `end_render`); play/pause/seek/refresh and every edit (add, remove,
  slide, trim, move, layout, canvas) are inert while it is set, and the
  Slint key scope ignores Space/Delete while `exporting`. Found while
  testing it: `end_render` returned while the preview was still prerolling
  asynchronously, and an edit landing in that window (Delete right after an
  export) made GES dereference a freed source asset — an access violation.
  `end_render` now waits for the preroll before clearing the flag.
- **H4** `pattern_name` escapes `%` as `%%` (printf directive → literal).
- **M7** `ExportSettings::normalized()` / `normalize_render_size` (even
  dimensions, NVENC floor with aspect kept, fps 1..=240) applied inside
  `encoding_profile`, so the GUI export and the headless render share it.
- **M8** The EXR cache key hashes every frame's `(mtime, len)`.
- **M9** Conversions build in `<key>.tmp-<pid>` and publish by one rename
  (losing a race keeps the other process' complete entry); the sweep gained a
  byte cap (`CACHE_MAX_BYTES`, LRU eviction) and crashed-temp cleanup, and
  runs from the headless path too.
- **M10** `thumbnail_uri` treats `Ok(Async)` as failure and pulls the preroll
  with a 5 s timeout — one stalled file can no longer block the import worker.
- **M14** `Cancelled` is a typed error; callers use `err.is::<Cancelled>()`.
- **L14** The headless render drops its `Project` instead of `end_render`.
- **L15** `end_render` failures surface via the error dialog.
- Tests: size/fps normalization; transport + edits inert while rendering
  (and working again after); `%` escaping incl. the URI form; cache key
  changes when a middle frame changes; sweep enforces age + size (LRU) and
  keeps a fresh temp dir; cancels are typed and leave no temp dir.

## §3 Explorer & CLI — H8–H10, M12, M13, M15–M17, M34, L7–L10

- **H8** `ensure_registered` no longer re-points the menu at whatever exe is
  running. Debug builds never touch the registry; a release build
  (re)registers only when nothing owns the menu, the registered exe no longer
  exists, or the menu is ours with an older key set (`Schema`). Seen live on
  the dev machine before the fix: the installed 2.6.0's menu had been
  hijacked by a `target\release` run.
- **H9** The submenu is generated from the `PresetStore` — every preset in
  store order (Explorer sorts subcommands by key name, so the key carries the
  position), then a separator and the fixed items; a name containing `"`
  stays GUI-only. Save/delete in the GUI call `sync_menu`, which rewrites the
  stores only when the menu is ours. README no longer claims a fixed trio.
- **H10** The verb is registered per extension
  (`SystemFileAssociations\.<ext>`) for every canonical input plus `.exr`,
  which gets a sequence-only store (the presets can't read it); the
  perceived-type root (`…\image`, which carried `.dib/.ico/.wmf` the engine
  rejected and lacked `.exr`) is deleted on every (un)register.
- **M12 / M17** Every fatal path in `main` goes through `fail` →
  `notify_error` → exit 1: a failed `--register`, a progress window that
  could not open, a GUI that could not start. `notify_error` prints to
  stderr when there is a console or a stderr handle (a windowed exe run from
  a terminal now attaches to that console, so `--register` / `--version`
  print where the user is looking; a script with redirected output gets text
  instead of a dialog it can't dismiss), pops a dialog otherwise, and is
  muted by `--quiet`, which the MSI custom actions pass.
- **M13** `%V` for a drive root reaches the process as `C:"` (MSVC argv
  rules); `repair_drive_root` fixes exactly that shape at argv level.
- **M15** A stale lock is retired by atomic rename; only the rename winner
  recreates it, the loser finds a fresh lock and follows.
- **M16** clap: `--version`; the modes conflict with each other; `--fps`
  requires `--sequence-mp4`; a headless mode without a PATH is an explicit
  error instead of a silent no-op.
- **M34** One `INPUT_EXTENSIONS` list in `kuvatin-core` (now with `.tif`,
  `.jfif`, `.jpe`) feeds the folder scanner, both file dialogs, the
  video-mode still classifier and the registration; `FRAME_EXTENSIONS` in
  `kuvatin-video` feeds the sequence dialog, the headless resolver (a `.tif`
  frame is rejected up front instead of failing inside GStreamer) and the
  `.exr` registration.
- **L7** A lone arrival waits 250 ms, not the full 600 ms quiet window; the
  window still restarts on every further arrival.
- **L8** `MB_SETFOREGROUND` dropped (topmost, but no focus steal).
- **L9** Spool encoding is lossless: a non-Unicode NTFS name is hex-encoded
  UTF-16 instead of a `U+FFFD` look-alike. Claimed entries are still deleted
  when claimed, deliberately: keeping them for the run's duration would need
  a guard threaded through `Role` and a sweep for crashed leaders' claim
  dirs, with nothing ever reading them back — a retry is one right-click away.
- **L10** Folder scans skip hidden/system files and dot-files (`._foo.png`
  sidecars, thumbnail caches); an explicitly selected hidden file still runs.
- Tests: menu mirrors the store (order, separator, unquotable name); verb
  roots and their stores follow the canonical lists; command lines; CLI
  conflicts/dangling flags/missing PATH; drive-root repair; lone-arrival
  timing; stale lock retired by exactly one of four; non-Unicode spool round
  trip; hidden/dot-file skipping; `.tif`/`.jfif` accepted; `.tif` frame
  rejected up front. Live-checked on the dev machine: the new build's
  `--register` writes the per-extension roots + three stores and deletes the
  legacy root, `--unregister` removes everything, `--version` prints.

## §4 Editor UX — H5, H6, M11, M18–M26, L11

- **H5** A video-engine init failure shows "Video engine unavailable" with
  the cause and sets `video-engine-down`, which disables Open media / Import
  sequence / Export instead of leaving every click a silent no-op.
- **H6** "Export…" shows the progress modal first and starts `begin_render`
  (which waits for the preview to reach NULL, up to 3 s, on the UI thread)
  one tick later; `export_pending` blocks the preview tick and re-entrancy
  meanwhile and lets Cancel abort before the start. `cancel_render` skips the
  2 s EOS wait when the partial file is deleted anyway.
- **M11** The stall watchdog's baseline is the last fraction that actually
  advanced, so a slow-but-healthy render (software x264 at 1080p on a long
  timeline) is no longer called stuck after 20 s.
- **M18** Imports are stamped with a generation; Cancel bumps it, and the
  worker and the drain discard older items — the file mid-discovery at
  cancel no longer lands in the bin, counters no longer overshoot, and a
  fresh drop no longer revives the cancelled queue.
- **M19** End of timeline without repeat pauses and clears `video-playing`
  (Play from the end restarts); deleting a clip recomputes
  `timeline-duration`.
- **M20** Scrubbing stashes the target and the UI tick issues one keyframe
  seek per tick; releasing the scrubber (transport or lane) lands with
  `seek_accurate`, so the paused picture matches the playhead.
- **M21** `current_job` is the ONE recipe for Convert and Save preset and
  folds the resolution fields into `job.resize`; `sync_controls` mirrors a
  preset (incl. its pixel resize, or 0/0 for percent/fit) into the controls
  on selection, save and delete.
- **M22** `sync_rows` diffs the file list by path (insert/remove only what
  changed — kept rows keep status, thumbnail and dimensions, no fade replay),
  the selection follows its file across the re-sort, and every row has a
  hover-revealed × (`remove-file`) that also drops its crop.
- **M23** The error dialog sizes to its content up to most of the window and
  scrolls beyond that; the import summary is capped at 10 names + "…and K
  more" like the headless path. `preview_box`/`preview.rs` are gone.
- **M24** The crop box is computed in Slint from the crop surface and the
  image aspect (largest fitting box, small images scale up), so a maximised
  window gets a full-size editor.
- **M25** File imports finishing no longer close the modal while a sequence
  import still runs; Esc closes the sequence dialog and cancels an import;
  Export is disabled on an empty timeline (toolbar and dialog).
- **M26** Shared button components (`SecondaryButton`, new `DialogButton`,
  `RemoveButton`) carry `accessible-role`/label/enabled and a `FocusScope`
  (Tab focus ring, Enter/Space); sliders, scrubbers, toggles, list rows and
  the window buttons are labelled; hit targets: row/bin × 24 px, clip × 24×22
  px, trim grips 12 px, Export 22 px. Every modal button is a `DialogButton`.
- **L11** `collect_media` expands folders and keeps videos + image inputs
  (junk dropped silently); explicitly dropped `.exr` frames get a pointer at
  "Import sequence…"; `SeenSet` keys by canonical path, so `C:\x.mp4` and
  `c:\x.mp4` are one file. `VIDEO_EXTENSIONS` lives in `kuvatin-video`.
- Tests: media collection (folders, filtering, EXR flag); `SeenSet`
  canonicalisation (case, `..`); dialog name cap. The debug GUI was
  smoke-started; the Slint changes compiled cleanly on the first build.

## §5 Ship pipeline — H12, H13, M28–M33, L16, L17

- **H12** No certificate exists, so signing is documented rather than done:
  the README's Install section and the landing page explain the SmartScreen
  prompt, and every release now carries a `.sha256` next to the `.msi`.
  Signing (Azure Trusted Signing / SignPath) stays a follow-up that needs an
  account the maintainer must create.
- **H13** The video engine gates the release: the self-contained video tests
  (generated frames, real pipelines — incl. a new mid-render cancel test that
  tears down a LIVE render, 600 frames so it outlives the first poll) run
  blocking, and a headless smoke render (`--sequence-mp4 --quiet`, 48
  generated PNGs → H.264, verified with gst-discoverer) runs with the release
  exe on every push. Test scratch files moved from `%TEMP%` (an 8.3 short
  path on the runner) to `target/test-tmp/<tag>-<pid>`, so parallel runs
  don't collide. The live-media (fixture webm) tests stay advisory.
- **M28** `rust-toolchain.toml` pins 1.96.0 with rustfmt + clippy; CI runs
  `cargo fmt --check`, `clippy --all-targets -D warnings` (the 8 findings
  fixed; the workspace was rustfmt'ed — 161 hunks, one style commit) and
  `cargo deny` (pinned action) on every push; a CycloneDX SBOM is generated
  and attached to releases; Dependabot watches Cargo and the pinned actions.
- **M29** The workflow runs with `contents: read`; only a separate `publish`
  job (tags, Ubuntu, downloads the artifact) has `contents: write`.
- **M30** On tags/dispatch the built MSI is installed with `msiexec /qn`, the
  runner's GStreamer is removed from PATH, and the INSTALLED exe converts an
  image (`--preset`) and renders a sequence (`--sequence-mp4`) from the bundled
  runtime alone, then uninstalls. A plugin missing from the allow-list fails
  the release.
- **M31** Every release ships a fixed-name `kuvatin-x86_64.msi` (also added to
  the existing 2.6.0 release), so the landing page's download links are
  static; the per-visitor GitHub API call is gone (the version label comes
  from the JSON-LD stamp, which CI checks against the tag); Inter and Space
  Grotesk are self-hosted (`docs/fonts/`, variable latin subsets, OFL texts).
- **M32** `forced-colors` fallbacks paint the gradient headings in
  `CanvasText`; the install snippet's comment colour is ≥4.5:1; the release
  note is 13 px.
- **M33** The GitHub link stays in the nav on phones; the blurred glows, grid
  and noise layers are off under 860 px; the card-tilt loops and the cursor
  glow run only while scrolling/moving (plus a settle tail) instead of every
  frame.
- **L16** Product name "Kuvatin", ARP about/help/update links, `ARPNOMODIFY`,
  `WixUI_InstallDir` instead of a one-box feature tree, an HKLM keypath for
  the per-machine shortcut component. Built locally (ICE-clean).
- **L17** README: Status points at the releases page and describes the CI
  gates; the exe's four modes; the video crate's description; workspace
  `repository`/`homepage`/`authors` metadata; the landing page's "cleans it
  back up" is qualified. `--quiet` now also skips the progress window
  (`run_headless`), which the smoke and install tests rely on.
- Tests: `cancelling_mid_render_stops_and_removes_the_partial` (video 32);
  the stale-lock rendezvous test exposed a real race (a fresh lock deleted by
  a second retirer, or a delete-pending `PermissionDenied` taken as "lead
  alone") — fixed with an exclusive `takeover.lock` and a brief retry; 6/6
  stable now.

## §6 Structure — M5, M6, M27, M35, L2, L4–L6, L12, L13

(Phases §1–§6 are filled in as they are executed.)
