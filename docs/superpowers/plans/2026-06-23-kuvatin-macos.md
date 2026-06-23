# Kuvatin macOS Port — Implementation Plan

> **For agentic workers:** Steps use checkbox (`- [ ]`) syntax for tracking. Because none of
> the macOS code compiles or runs on the Windows dev host, verification is driven by the
> GitHub Actions macOS runner: push the workflow, read the runner logs, fix, repeat.

**Goal:** Ship a macOS build of Kuvatin with a native window, drag-and-drop, and a Finder
right-click integration equivalent to the Windows Explorer context menu — built entirely in
GitHub Actions (no local Mac needed), distributed **unsigned / un-notarized**.

**Decisions (locked with the user):**
- macOS uses the **native window decorations** (drop the custom frameless titlebar there).
- Drag-and-drop is required.
- The Finder context-menu equivalent is a **core feature** → real effort, via **Automator
  Quick Actions** in `~/Library/Services/`.
- **No notarization** → accept the Gatekeeper "right-click → Open" first-run step; document it.
- **CI builds everything**: one `release.yml` produces both the macOS universal `.dmg` and the
  Windows `.msi` and attaches both to the GitHub Release.
- Quick Actions **auto-register idempotently on first GUI launch**, plus an **in-app on/off
  toggle**; `--register`/`--unregister` remain the underlying mechanism.

**Why Quick Actions (not NSServices):** Quick Actions are plain `.workflow` folders
(`Info.plist` + `document.wflow`) we write programmatically — no Cocoa handler, no signing
needed for the workflow itself, and they appear in Finder's right-click *Quick Actions*
section. Each runs `"<app>/Contents/MacOS/kuvatin" --preset "<name>" "$@"`, mapping 1:1 onto
the existing CLI (multi-file selections included). NSServices would need an objc2 service
handler and are less discoverable.

---

## Phase 0 — Cross-platform groundwork (Windows behavior unchanged)

- [ ] Make the dropped-paths **drain timer** in `gui.rs` platform-neutral (currently
      `#[cfg(windows)]`). Keep the OLE/`WM_DROPFILES` *producer* Windows-only; share the queue.
- [ ] Add `in property <bool> use-native-frame: false;` to `app.slint`; bind
      `no-frame: !root.use-native-frame;` and gate the custom titlebar `Rectangle` on
      `!root.use-native-frame`.
- [ ] **SPIKE (highest unknown):** confirm Slint 1.8 honors a *data-bound* `no-frame` set
      before `show()`. Fallback if not: a cargo feature compiling a native-frame window
      variant, or a second window component.

## Phase 1 — Mac native window + drag-and-drop

- [ ] On macOS, set `use-native-frame = true` before `ui.show()` (native traffic-lights handle
      min/close; existing `win-*` callbacks already no-op off-Windows).
- [ ] New `mac_drop` module (`#[cfg(target_os = "macos")]`): via `objc2` / `objc2-app-kit` +
      `raw-window-handle`, register the NSView as a dragging destination, read dropped file
      URLs, push into the shared queue the drain timer reads.
- [ ] Add deps under `[target.'cfg(target_os = "macos")'.dependencies]`: `objc2`,
      `objc2-app-kit`, `objc2-foundation`.

## Phase 2 — Finder Quick Actions

- [ ] Move the shared `ITEMS` preset list out of `shell/windows.rs` so both platforms use it.
- [ ] New `shell/macos.rs` implementing `register()` / `unregister()` (replaces the `bail!`
      stub in `shell/mod.rs`):
  - [ ] `register()` writes one `.workflow` per item to `~/Library/Services/`, resolves the
        binary via `current_exe()`, filters input to `public.image`, runs `pbs -flush`.
  - [ ] `unregister()` deletes the `.workflow` directories and refreshes.
- [ ] Auto-register idempotently on first GUI launch.
- [ ] In-app Finder-integration on/off toggle wired to register/unregister.

## Phase 3 — Bundling & icon

- [ ] Generate `Kuvatin.icns` from the SVG/PNG master (via `sips` / `iconutil` in CI).
- [ ] `cargo-bundle` config (`[package.metadata.bundle]`: identifier, icon,
      `LSMinimumSystemVersion`, `NSHighResolutionCapable`) → `.app`; then `hdiutil` /
      `create-dmg` → `.dmg`. (`build.rs` Windows icon embedding stays `#[cfg(windows)]`.)

## Phase 4 — GitHub Actions (`.github/workflows/release.yml`)

- [ ] Trigger on tag push / manual dispatch.
- [ ] macOS job (`macos-14`): build `aarch64` + `x86_64`, `lipo` → universal binary; bundle
      `.app` → unsigned `.dmg`; attach to the GitHub Release.
- [ ] Windows job: build + `cargo-wix` MSI; attach to the same Release.

## Phase 5 — Docs & landing page

- [ ] README: macOS install + Finder integration + Gatekeeper bypass
      (`xattr -dr com.apple.quarantine` / right-click → Open).
- [ ] `docs/index.html`: macOS `.dmg` download button mirroring the existing latest-`.msi`
      fetch logic, plus the Gatekeeper note.

---

## Risks to retire early
1. Slint bound `no-frame` (Phase 0 spike) — highest uncertainty.
2. objc2 dragging-destination wiring onto Slint's winit NSView.
3. Quick Action `.workflow` plist format + image filter + first-run quarantine on the unsigned binary.
4. Universal build of native C deps (`libimagequant`) on the runner.
