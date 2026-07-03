# Kuvatin Hardening & Excellence Plan

**Status:** ready to execute · **Created:** 2026-07-02 · **Source:** full-codebase audit (4 parallel reviewers + manual verification), findings in `kuvatin-audit.html` at the repo root.

This plan is self-contained: a fresh Claude Code session can execute it top-to-bottom without other context. Execute phases in order; tasks within a phase are independent unless noted. Commit per task (or per small group) with `fix:`/`feat:`/`refactor:` prefixes. **Ask the user before starting Phase 7** (product leaps — bigger, opinionated features).

---

## 0. Ground rules & environment (read first, non-negotiable)

**The app:** Kuvatin — Windows-only Rust workspace. `crates/kuvatin-core` (image engine, pure Rust), `crates/kuvatin-video` (GStreamer/GES video engine), `crates/kuvatin` (Slint GUI + CLI + Win32 shell). UI markup: `crates/kuvatin/ui/app.slint`. Installer: `crates/kuvatin/wix/`. CI: `.github/workflows/release.yml`. Landing page: `docs/index.html` (GitHub Pages).

**Build/test commands:**
```powershell
cargo check --workspace                      # fast validation (also compiles app.slint via build.rs)
cargo test -p kuvatin-core -p kuvatin        # image + GUI-crate tests, parallel OK
# Video tests: GStreamer on PATH, fixtures via env, and SINGLE-THREADED (parallel GES pipelines deadlock):
$env:PATH = "C:\Program Files\gstreamer\1.0\msvc_x86_64\bin;$env:PATH"
$env:GST_TEST_FILE  = "<abs path to an audio+video fixture .webm>"
$env:GST_TEST_IMAGE = "<abs path to any .png>"
cargo test -p kuvatin-video --release -- --test-threads=1
```
Create fixtures if none exist (GStreamer bin on PATH):
```powershell
gst-launch-1.0 -e videotestsrc num-buffers=150 ! "video/x-raw,width=320,height=180" ! vp8enc ! webmmux name=m ! filesink location=fixture_av.webm audiotestsrc num-buffers=300 ! audioconvert ! vorbisenc ! m.
gst-launch-1.0 videotestsrc num-buffers=1 ! "video/x-raw,width=320,height=180" ! pngenc ! filesink location=fixture.png
```
MSI build (only when explicitly needed): WiX3 binaries at `C:\Users\ville\wix3-bin` (prepend to PATH), then FROM `crates\kuvatin`: `cargo wix -p kuvatin --nocapture --compiler-arg "-dGstStageDir=<repo>\target\gst-staging"` (stage first with `crates\kuvatin\wix\bundle-gstreamer.ps1 -StageDir <repo>\target\gst-staging -HeatExe C:\Users\ville\wix3-bin\heat.exe`).

**Hard-won gotchas — violating these reintroduces fixed bugs:**
1. GES layer index 0 is the TOPMOST composited layer (images/overlays go on LOW indices).
2. Never call `timeline.commit_sync()` from code reachable while the pipeline may be mid-async-state-change — use async `commit()`. (`commit_sync` is only safe on a cold, never-played pipeline.)
3. The encoding profile's video restriction MUST stay fully pinned (format+size+framerate+PAR) — unpinning re-breaks NVENC on gaps/overlays ("Internal data stream error", truncated no-moov MP4).
4. `mfaacenc` MUST stay effectively disabled for export — it corrupts GPU state and kills NVENC session-open (`NV_ENC_ERR_INVALID_VERSION`).
5. Slint: the drop-zone indicator and anything that must never be clipped belongs at Window level (sibling of the modals), not inside the timeline lane.
6. Slint drag pattern: TouchAreas stay pinned to model geometry; a separate visual element moves from the pointer delta.
7. Do not do per-pointer-event GES work — coalesce edits through the `dirty` flag / UI timer.
8. `windows_subsystem = "windows"` in release: stderr is invisible. Never "handle" an error with only `eprintln!`.

---

## Phase 1 — Stop the bleeding (crashes, data loss, bricked startup)

### 1.1 Resilient preset store (fixes the app-won't-launch brick)
**Files:** `crates/kuvatin-core/src/preset.rs`, `crates/kuvatin/src/gui.rs` (~line 124), `crates/kuvatin/src/main.rs`.
**Change:** (a) `PresetStore::save` → atomic write: write to `presets.toml.tmp` then `fs::rename` over the target. (b) `load_or_init`: on read/parse failure do NOT propagate — back up the bad file to `presets.toml.bad`, log, and return built-in presets (in memory; don't overwrite the backup). Parse presets individually (deserialize `Vec<toml::Value>` then per-entry) so one bad preset doesn't drop the rest. (c) `main.rs`/`gui.rs`: any残 fatal startup error must show a native message box (use the `windows` crate `MessageBoxW`) before exiting — never a silent exit.
**Verify:** unit tests: corrupt file → builtins + `.bad` backup exists; partial file (one bad entry) → good entries survive; save is atomic (tmp file gone after save). `cargo test -p kuvatin-core`.
**Done when:** a truncated/corrupt `presets.toml` can never prevent launch, and no user preset is silently destroyed.

### 1.2 WebP encode must not panic; clamp quality at the core boundary
**Files:** `crates/kuvatin-core/src/pipeline.rs` (~86-91, and PNG-lossy quality path ~149).
**Change:** replace `encoder.encode(q)` with `encoder.encode_simple(false, q)` and map `Err` → `CoreError::Encode` (include dimensions in the message — libwebp rejects >16383px). Clamp `quality` to 0-100 inside the pipeline (single choke point) so hand-edited presets and `--preset` CLI can't bypass the GUI clamp.
**Verify:** new tests: 16384-px-wide 1-px-tall image → `Err` not panic; quality 150 → clamped, succeeds. `cargo test -p kuvatin-core`.
**Done when:** no input image or preset value can panic the encoder.

### 1.3 Race-proof output naming (parallel batches must never overwrite)
**Files:** `crates/kuvatin-core/src/naming.rs` (`ensure_unique` ~68), `crates/kuvatin-core/src/pipeline.rs` (write sites ~197, 207, and `process_file_to`).
**Change:** replace exists()-then-write with reserve-at-open: `OpenOptions::new().write(true).create_new(true)` in a loop that bumps the `-n` suffix on `AlreadyExists`, then write into the reserved handle. Add a batch-level planner used by the GUI's output-folder mode that dedupes same-stem targets *before* dispatch (A\photo.jpg + B\photo.jpg → photo.ext / photo-1.ext deterministically).
**Verify:** test: two rayon tasks targeting the same output stem → two distinct files, both contents intact. Test: same-stem inputs to one output folder → no clobber.
**Done when:** no combination of parallel inputs can silently lose an output.

### 1.4 Panic isolation in the batch executor
**Files:** `crates/kuvatin-core/src/batch.rs` (~47-74).
**Change:** wrap the per-file `process_file` call in `std::panic::catch_unwind(AssertUnwindSafe(...))`; convert panic payloads to `Err("internal error: <payload>")` so one file can't abort the batch or kill the GUI worker thread.
**Verify:** test with an injected panicking job (feature-gated test hook or a closure-based seam) → other files still complete.
**Done when:** the documented "a single failing file never aborts the batch" contract holds for panics too.

### 1.5 `begin_render` failure must not kill the preview
**Files:** `crates/kuvatin-video/src/project.rs` (`begin_render` ~794-811), `crates/kuvatin/src/gui.rs` (export start ~1119-1131).
**Change:** inside `begin_render`, on ANY fallible step failing after the sink was detached, restore state before returning `Err`: re-attach `self.appsink` via `preview_set_video_sink`, `set_mode(FULL_PREVIEW)`, `set_state(Paused)`. Belt-and-braces: in the GUI, if `begin_render` returns `Err`, call `end_render()` (it's idempotent-ish) and surface the error (Phase 2.1 dialog).
**Verify:** extend the video test suite: force a failure (render to an invalid path like `Z:\nope\x.mp4`) → subsequent preview still produces frames (assert via the frame callback). Run serialized as per §0.
**Done when:** a failed export start leaves preview/playback fully working.

### 1.6 Fix `trim_clip` integer-wrap edge cases
**Files:** `crates/kuvatin-video/src/project.rs` (`trim_clip` ~547-583).
**Change:** redo the clamp math in signed 128-bit: compute candidate `start/inpoint/duration` as `i128` nanoseconds, clamp each `>= 0`, apply the min-duration floor (0.2 s) as the LAST step, then cast. Both edges.
**Verify:** new pure-logic unit tests (no GStreamer needed — extract the math into a testable fn): clip shorter than min-duration trimmed left; `inpoint >= max-duration` trimmed right; no value wraps.
**Done when:** no trim input can produce a wrapped ClockTime.

---

## Phase 2 — Errors users can see; controls users need

### 2.1 A real error surface (single reusable dialog)
**Files:** `crates/kuvatin/ui/app.slint`, `crates/kuvatin/src/gui.rs`.
**Change:** add window-level error dialog state (`error-visible`, `error-title`, `error-detail`, OK button, Esc closes — see 2.6) + a Rust helper `show_error(&ui, title, detail)`. Route through it: export failure (with the actual `RenderStatus::Failed` text), export-start failure, per-file import failures, preset save failures, `make_project` failure (video engine unavailable), image-convert worker errors that currently only mark row status. Remove/keep `eprintln!`s as secondary.
**Verify:** `cargo check -p kuvatin`; manual: trigger an export to an invalid target → dialog appears with reason and stays until dismissed.
**Done when:** no failure path in the GUI ends in stderr-only reporting (grep `eprintln!` in gui.rs and justify each survivor).

### 2.2 Cancellable, honest export
**Files:** `crates/kuvatin/ui/app.slint` (export modal ~1826-1858), `crates/kuvatin/src/gui.rs` (export flow), `crates/kuvatin-video/src/project.rs`.
**Change:** (a) Cancel button on the export modal → new `export-cancel()` callback → `end_render()` + delete the partial output file + close modal. (b) Stall watchdog: if progress hasn't advanced for 20 s, show "Export appears stuck" with Cancel emphasized. (c) On `Failed`: delete the partial file, then `show_error` with the pipeline message (keep the modal-close AFTER the dialog is up). (d) Don't block the UI: run `begin_render` (it can wait ~3 s internally) on a worker thread / deferred timer tick so the modal paints first.
**Verify:** manual: cancel mid-export → app fully usable, no partial file; export to an unwritable path → visible error.
**Done when:** the user can always escape an export, and never finds a mystery partial file.

### 2.3 Cancellable import; no dead bin entries; dedup
**Files:** `crates/kuvatin/src/gui.rs` (import worker ~206-215, ~683-693), `crates/kuvatin/ui/app.slint` (import modal ~1720).
**Change:** (a) Cancel button on the import modal → drains the pending queue, closes modal (worker finishes current file only). (b) When `warm_asset`/thumbnail fails, do NOT add the file to the bin — record it and show one summary error ("2 files could not be imported: …") via 2.1. (c) Skip paths already in `bin_paths` (select the existing row instead).
**Verify:** import a text file renamed `.mp4` → not added + error shown; import the same video twice → one bin entry.
**Done when:** the bin only ever contains playable, unique entries and import can't lock the app. *(Full fix for a hung `request_sync` on the CURRENT file is accepted as out of scope here — the cancel must at least abandon the queue; note any file that hangs discovery in the error summary.)*

### 2.4 Deletion everywhere (timeline clips, bin items, image rows) — keyed by id, not index
**Files:** `crates/kuvatin-video/src/project.rs`, `crates/kuvatin/src/gui.rs`, `crates/kuvatin/ui/app.slint`.
**Change:** (a) Engine: `Project::remove_clip(&mut self, id) -> bool` — `clip.layer().remove_clip(&clip)`, `clips.remove`, async `commit()`, `dirty.set(true)`; prune empty TRAILING layers (never reindex populated ones). (b) GUI: `timeline-clip-removed(int)` handler → remove from GES + `tl_clips` model + clear selection if it pointed at the removed clip. (c) UI affordances: an "×" on the selected clip block + Delete key (2.6). (d) Media bin: per-row "×" → remove from `video_assets` + `bin_paths`. (e) Image mode: per-row remove on the file list (in addition to Clear). (f) While here: audit every index-based cross-reference touched by removal (`sel_idx`, `bin_paths`, thumbnail delivery) and re-key by clip id / path where indices can now shift.
**Verify:** video tests: add 3 clips, remove middle → other two play, duration updates, re-add works. GUI compile + manual pass.
**Done when:** anything addable is removable, with no index desync.

### 2.5 Mode switch discipline
**Files:** `crates/kuvatin/ui/app.slint` (mode toggle ~623), `crates/kuvatin/src/gui.rs` (playhead timer ~1189-1236).
**Change:** add `app-mode-changed(int)` callback: entering Images mode pauses the video project (`p.pause()`); gate the 100 ms preview timer body on video mode (and ideally stop/start the import/drain timers around activity).
**Verify:** manual: play video → switch to Images → audio stops; CPU idles.
**Done when:** background video work never runs outside Videos mode.

### 2.6 Keyboard: first-class basics
**Files:** `crates/kuvatin/ui/app.slint`.
**Change:** wrap the app body in a `FocusScope`: Space = play/pause (video mode), Delete = remove selected clip, Esc = close export-settings dialog / cancel-able modals / error dialog. Make sure typing in `LineEdit`s isn't hijacked (only handle keys when no text input has focus — check `FocusScope.has-focus` interplay; Slint propagates unhandled keys, verify behavior).
**Verify:** manual keyboard pass over both modes.
**Done when:** the three shortcuts work and never fire while editing text.

---

## Phase 3 — Video engine robustness

### 3.1 Programmatic encoder ranks (kill the env-var fragility)
**Files:** `crates/kuvatin-video/src/project.rs` (`ensure_encoder_ranks` ~207-224 and every `gst::init` call site), `crates/kuvatin/src/main.rs` (`configure_bundled_gstreamer`).
**Change:** after `gst::init()`, set ranks via the registry API: `gst::Registry::get().lookup_feature("mfaacenc")` → `set_rank(gst::Rank::NONE)`; `nvautogpuh264enc` → 512; `x264enc` → 256. Keep a single `ensure_ranks_post_init()` called right after every `gst::init()`. Delete the env-var writes (they're skipped whenever the user's environment already sets `GST_PLUGIN_FEATURE_RANK`, silently re-breaking export — and `set_var` from worker threads is UB-adjacent). **Important:** verify by test that registry-API ranks actually affect encodebin's choice (earlier finding: rank changes AFTER first init didn't re-apply for encodebin — the difference here is these run BEFORE any element instantiation; the regression tests below are the arbiter). If encodebin ignores them, fall back to env var set ONCE in `main()` before anything else, but MERGE with any existing value instead of skipping.
**Verify:** full video suite serialized (esp. `renders_after_preview_eos`, `renders_gapped_overlay_timeline`) with `GST_PLUGIN_FEATURE_RANK` UNSET **and** with it pre-set to an unrelated value (`videotestsrc:300`) — both must pass.
**Done when:** export works regardless of the user's pre-existing GStreamer environment.

### 3.2 Preview bus errors surfaced
**Files:** `crates/kuvatin-video/src/project.rs`, `crates/kuvatin/src/gui.rs`.
**Change:** add `Project::poll_preview_error() -> Option<String>` (non-blocking `bus.pop_filtered(Error)`); call it from the existing 100 ms playhead timer; on Some → `show_error` + `pause()`.
**Verify:** feed a clip whose file is deleted after adding → error dialog instead of frozen preview.
**Done when:** a dead pipeline is never silent.

### 3.3 Accurate seeks where it matters
**Files:** `crates/kuvatin-video/src/project.rs` (`seek` ~750, `refresh_preview` ~775), `player.rs` if retained (see 3.5).
**Change:** use `FLUSH | ACCURATE` for: the edit-repaint seek in `refresh_preview`, and the final seek when a scrub drag ENDS (add a `seek_accurate` variant; the GUI calls it on pointer-up, keeping KEY_UNIT during the drag for responsiveness).
**Verify:** manual: pause on a frame, nudge a clip's position slider — the repainted frame matches the playhead time; scrub release lands on the exact time shown.
**Done when:** paused editing is frame-truthful.

### 3.4 Export FPS setting
**Files:** `crates/kuvatin-video/src/project.rs` (`ExportSettings`, `encoding_profile`), `crates/kuvatin/ui/app.slint` (export dialog), `crates/kuvatin/src/gui.rs`.
**Change:** add `fps: u32` to `ExportSettings` (default 30); the profile restriction uses it (`gst::Fraction::new(fps as i32, 1)`) — the restriction stays fully pinned (gotcha #3). Export dialog: FPS field + 24/25/30/60 pills.
**Verify:** render test at 60 fps → probe output framerate (`gst-discoverer-1.0`).
**Done when:** users control output fps; default unchanged.

### 3.5 Delete dead code; fix rot
**Files:** `crates/kuvatin-video/src/player.rs`, `src/lib.rs`, `crates/kuvatin/src/gui.rs` (~18-20 stale comment), `app.slint` (dead `video-select`/`video-selected`/`win-drag` — verify unused first).
**Change:** remove `Player` (move `Frame` + `sample_to_frame` into lib.rs or a `frame.rs`; keep the stride-honoring fix from 3.6 in the shared copy); delete dead Slint callbacks/properties; fix the stale `make_project` doc comment.
**Verify:** `cargo check --workspace` + full test suite (drop `pulls_frames_from_fixture` with the code it tested).
**Done when:** one playback stack, no dead API.

### 3.6 Small engine fixes (bundle into one commit)
**Files:** `crates/kuvatin-video/src/project.rs`.
**Change:** (a) `thumbnail`: set pipeline to Null on the early-return path (~135). (b) `sample_to_frame`: map via `gst_video::VideoFrame` honoring plane stride. (c) `clip_layout`: drop the 1.0 upper clamp on scale read-back (symmetric with `set_clip_layout`). (d) treat empty GES clip name as an error, not `""` key. (e) `cancel_render(delete_partial: bool)` helper used by 2.2 — send EOS, wait briefly for finalize, then Null (so cancel yields a *playable* partial when possible), delete file if asked.
**Verify:** video suite serialized; new unit test for (c) round-trip at scale 1.4.
**Done when:** all five landed, suite green.

---

## Phase 4 — Image fidelity (a photo tool must not mangle photos)

### 4.1 EXIF orientation
**Files:** `crates/kuvatin-core/src/pipeline.rs` (decode sites ~191, 219).
**Change:** decode via `image::ImageReader`; read orientation (`decoder.orientation()`, image 0.25) and `DynamicImage::apply_orientation` before processing.
**Verify:** test with a fixture JPEG carrying orientation 6 → output pixels rotated correctly (construct via `img_parts` or a tiny checked-in fixture).
**Done when:** portrait phone photos convert upright.

### 4.2 JPEG alpha compositing
**Files:** `crates/kuvatin-core/src/pipeline.rs` (~78).
**Change:** before `to_rgb8()` for JPEG output, composite RGBA over white.
**Verify:** test: transparent-red PNG → JPEG → corner pixel is pink-on-white, not black.

### 4.3 Animated GIF honesty
**Files:** `crates/kuvatin-core/src/pipeline.rs`.
**Change:** detect multi-frame GIF inputs (`image::codecs::gif::GifDecoder::into_frames().take(2)`); return a clear `Err("animated GIF not supported (N frames) — only still images")` instead of silently flattening. (Full animation support is NOT in scope.)
**Verify:** test with a 2-frame GIF fixture → error, message readable in the GUI row status.

### 4.4 Input hardening sweep (one commit)
**Files:** `crates/kuvatin-core/src/{resize,crop,naming}.rs`.
**Change:** (a) clamp resize targets (total-pixel budget, e.g. 250 MP; `InvalidJob` beyond). (b) crop `AspectRatio`: saturate the u64→u32 casts. (c) zero-dimension early return in `process_image`/`apply_crop`. (d) sanitize `suffix`/`subfolder` (strip `/ \ : * ? " < > |`). (e) `uses_quality`: account for lossy-PNG (add `Job::uses_quality()`; check GUI slider gating uses it correctly afterward).
**Verify:** unit tests for each; `cargo test -p kuvatin-core`.

*(ICC profile preservation: acknowledged, deliberately deferred — record as a known limitation in README. Revisit if users report color shifts.)*

---

## Phase 5 — Installer & CI hardening

### 5.1 Checksum-pin third-party binaries in CI
**Files:** `.github/workflows/release.yml` (~24-48).
**Change:** hardcode SHA-256 for both GStreamer MSIs and `wix314-binaries.zip`; compute → compare → fail on mismatch before executing/extracting. (Obtain hashes by downloading once locally and recording `Get-FileHash`.)
**Verify:** workflow_dispatch run passes; temporarily flip one hash char in a branch run → fails at the check.

### 5.2 Test gate before release
**Files:** `.github/workflows/release.yml`.
**Change:** after the GStreamer install step (runtime is on PATH), add: generate the two fixtures (§0 commands) into `$env:TEMP`, then `cargo test --workspace --release` with `GST_TEST_FILE`/`GST_TEST_IMAGE` set, **video crate invoked separately with `--test-threads=1`** (mirror §0: test core+GUI crates normally, then `-p kuvatin-video -- --test-threads=1`). MSI build only after green.
**Verify:** dispatch run: tests execute (check logs show >0 video tests ran, not all skipped).

### 5.3 CI pinning + version consistency
**Files:** `.github/workflows/release.yml`.
**Change:** pin all `uses:` actions to full commit SHAs (comment the tag); pin `cargo install cargo-wix --version <current>`; add a step asserting the pushed tag equals `v$(workspace version)` (parse root Cargo.toml) — mismatch fails before building.

### 5.4 Stop polluting the system PATH
**Files:** `crates/kuvatin/wix/main.wxs` (Environment component ~111-121, feature ~179-186).
**Change:** remove the PATH Environment component + its feature (the exe's DLLs sit beside it; nothing needs PATH). If some workflow truly needed it, set the feature `Level='2'` (off by default) instead.
**Verify:** build MSI, install on a test VM/snapshot if available: app launches, context menu works, `gst-launch-1.0` from cmd does NOT resolve to Kuvatin's copies.

### 5.5 Registration scope sanity
**Files:** `crates/kuvatin/wix/main.wxs` (custom actions ~199-215, shortcut component ~137-152), `crates/kuvatin/src/shell/windows.rs`, `crates/kuvatin/src/gui.rs`.
**Change:** adopt **register-on-launch**: the GUI (per user, on startup) ensures the HKCU context-menu keys exist/point at the current exe (cheap idempotent check), and the MSI keeps `--unregister` on uninstall only for the installing user (best effort). Fix the ICE38 violation: key the Start-menu-shortcut component with `Root='HKMU'`. Document the per-machine/per-user tradeoff in `wix/README.md`.
**Verify:** fresh install → menu appears for a *second* user after they first launch the app; uninstall → no dead menu for the installing user.

### 5.6 GUID stability note
**Files:** `crates/kuvatin/wix/bundle-gstreamer.ps1`, `crates/kuvatin/wix/main.wxs`.
**Change:** add a load-bearing comment at BOTH sites: `heat -gg` (fresh GUIDs each build) is only safe because `MajorUpgrade Schedule='afterInstallInitialize'` removes the old product first — change either only together.

*(Authenticode signing: needs a certificate — surface to the user as a recommendation, do not implement.)*

---

## Phase 6 — Architecture & polish

### 6.1 Split the `gui.rs` monolith
**Files:** `crates/kuvatin/src/gui.rs` (~1300 lines) → `gui/mod.rs`, `gui/image_mode.rs`, `gui/video_mode.rs`, `gui/window.rs`, `gui/import.rs`, `gui/export.rs`.
**Change:** mechanical extraction — each module gets a `wire(ui, state)` fn; share state via small structs (`ImageState`, `VideoState`) instead of 25 ad-hoc closure captures. Deduplicate: job-building goes through one `current_job` path; preset→UI sync in one fn. NO behavior changes in this task.
**Verify:** `cargo test --workspace` (+ video serialized) green; app manually smoke-tested both modes.

### 6.2 Slint hygiene sweep (one commit)
**Files:** `crates/kuvatin/ui/app.slint`.
**Change:** (a) hoist `track-h: 30px` + a `band-height()` helper (replace ~10 magic sites). (b) handle `PointerEventKind.cancel` in every drag TouchArea (clip slide/trims/header reorder) — reset `dmode/ddx/ddy/clip-dragging`. (c) NumberDropdown editor → `PopupWindow` (fixes z-order/click-stealing in crop toolbar, export dialog, canvas fields). (d) replace 🔒 emoji with an SVG asset + colorize. (e) idle "+ New track" row: give it a real click → create track (or render it only while dragging). (f) Rust pushes canvas defaults at startup (kill the 1280/720 literal duplication).
**Verify:** compile; manual drag-cancel test (drag a clip, Alt-Tab away, release) → no stuck banner, no ghost reorder.

### 6.3 Perf niceties
**Files:** `crates/kuvatin/src/gui.rs`.
**Change:** (a) `on_select_file`: decode preview on a worker thread, deliver via `invoke_from_event_loop` (mirror the thumbnail path). (b) `spawn_thumbnails`: only decode newly-added paths. (c) freeze the image file list while a batch runs (disable Add/Clear) OR resolve result indices against current rows by path. (d) delete-preset with no selection: bail on `current < 0`.
**Verify:** `cargo test -p kuvatin`; manual: click a huge image → UI stays responsive.

### 6.4 Landing page follow-ups
**Files:** `docs/index.html`, `docs/og.html`, `docs/assets/og-image.png`.
**Change:** add SRI `integrity` + `crossorigin` to the GSAP/Lenis CDN tags (compute hashes from the exact pinned versions); regenerate `og-image.png` from the updated `og.html` at 1200×630 (headless Chrome: `chrome --headless --screenshot=... --window-size=1200,630 og.html` — or ask the user to screenshot).
**Verify:** page loads with SRI (DevTools console clean); share-card preview shows the 2.0 tagline.

---

## Phase 7 — Product leaps (ASK THE USER before starting; order by their priority)

Each of these is a feature project with its own mini-plan; they build on Phases 1-3 being done.

- **7.1 Project save/load** — persist the timeline (clips, trims, tracks, transforms, canvas) via GES's XML formatter (`timeline.save_to_uri`) or a small JSON model replayed through the existing `Project` API (more robust to GES quirks — earlier attempt found `load_from_uri` asserts unless the timeline was created empty-for-loading). File association `.kuvatin`. Autosave to `%APPDATA%` + "reopen last session" prompt.
- **7.2 Undo/redo** — command pattern over `Project`'s mutating ops (add/remove/slide/trim/move/layout/canvas); Ctrl+Z/Ctrl+Y; coalesce slider drags into one command.
- **7.3 Timeline zoom & niceties** — `timeline-pps` slider/Ctrl+wheel zoom, snapping on/off toggle, clip context menu (delete/duplicate/split-at-playhead — split = two clips with adjusted inpoint/duration, engine already supports it).
- **7.4 Audio tracks & master audio** — audio-only clips (music beds), per-track mute/solo; GES supports audio-only layers.
- **7.5 Transitions** — GES `GESTransitionClip` crossfades on overlapping clips (start with video crossfade + audio fade).
- **7.6 App icon refresh** — the user has a pending new .ico/PNG to embed (exe resource via `winresource`, window icon, MSI ARPPRODUCTICON, landing page favicon) — ask them for the file.

---

## Execution order & definition of done

1. Branch: `feat/hardening` off `master`. Phases 1→6 in order; commit per task; run the relevant test commands (§0) before each commit; full workspace suite (video serialized) at each phase boundary.
2. After Phase 2 and after Phase 6: build the app (`cargo build --release -p kuvatin`), copy to `target\Kuvatin-test\Kuvatin.exe` (folder already has the GStreamer DLLs) and ask the user for a manual smoke test before continuing.
3. Done = all Phase 1-6 tasks landed, suite green, user smoke-tested, then version bump to 2.1.0 + MSI + (user-approved) push master + tag per the release recipe in §0.
