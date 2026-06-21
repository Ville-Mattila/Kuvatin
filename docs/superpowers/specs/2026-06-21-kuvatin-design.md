# Kuvatin — Design Spec

**Date:** 2026-06-21
**Status:** Approved (design phase)

## 1. Summary

Kuvatin is a compact, native Windows batch image tool for **converting, resizing, and
cropping** images, with a simple but polished Slint UI and Windows Explorer
context-menu integration for quick one-click conversions. It is architected so a later
cross-platform port (macOS/Linux) is primarily a matter of swapping the OS
shell-integration layer; the core engine is OS-agnostic pure Rust.

"Kuvatin" derives from Finnish *kuva* (image).

## 2. Goals / Non-goals

### Goals (v1)
- Batch **format conversion** between PNG, JPEG, WebP, BMP, TIFF, GIF, with quality control.
- Batch **resize** by pixels, percentage, or fit-to-box, with aspect-ratio lock and
  high-quality resampling.
- Batch **crop** to a fixed size or aspect ratio (applied uniformly across the batch).
- **Batch processing + reusable presets** as the core workflow (folders / multi-select).
- **Explorer context menu**: a cascading submenu of quick preset actions plus
  "Open in Kuvatin…".
- Distributed as a Windows **`.msi` installer** that registers/unregisters the context menu.

### Non-goals (v1)
- Interactive per-image crop selection (deferred to **v1.1**).
- macOS/Linux GUI packaging (architecture allows it later; not built in v1).
- Modern Win11 top-level `IExplorerCommand` menu (classic registry verbs in v1; see §6).
- Image editing beyond convert/resize/crop (filters, color, etc.).

## 3. Architecture

Cargo **workspace** with two crates plus a Windows-only module:

```
kuvatin/
├─ Cargo.toml            (workspace)
├─ crates/
│  ├─ kuvatin-core/      (lib: OS-agnostic engine — no GUI/OS deps)
│  └─ kuvatin/           (bin: kuvatin.exe — Slint UI + CLI + shell module)
│     └─ src/shell/windows.rs   (#[cfg(windows)] registry registration)
└─ docs/
```

### 3.1 `kuvatin-core` (library)

OS-agnostic, GUI-free, fully unit-testable. Owns:

- **Format IO** via the `image` crate (decode all; encode PNG, JPEG, BMP, TIFF, GIF).
  Lossy WebP encode goes through the `webp` crate (libwebp) for real quality control.
- **Resampling** via a single `resample()` function (v1: `image::imageops` Lanczos3,
  pure-Rust and correct; `fast_image_resize` SIMD is a drop-in post-v1 optimization).
- **Job model:**
  - `Op` = `Resize` | `Crop` | `Convert` (with op-specific params).
  - `Job` = an ordered op pipeline `[Resize?, Crop?, Convert]` + `OutputPolicy`.
    Canonical order: resize → crop → convert/encode.
  - `Preset` = a named, serializable `Job`.
- **Output naming** (`OutputPolicy`): sibling file using a token pattern,
  default `{name}_{w}x{h}.{ext}`. Tokens: `{name}`, `{w}`, `{h}`, `{ext}`, `{preset}`.
  Collision-safe: never overwrites an existing file; appends `-1`, `-2`, … on collision.
- **Batch executor:** processes a list of input paths with `rayon` across cores;
  emits `Progress` events (per-file start / done / error, plus overall counts) over a
  bounded channel.
- **Errors:** typed via `thiserror` (`CoreError`). Per-file failures are isolated — a
  single failing file is recorded and the batch continues.

### 3.2 `kuvatin` (binary, `kuvatin.exe`)

Single executable with three entry behaviors selected by CLI args:

| Invocation                       | Behavior |
|----------------------------------|----------|
| `kuvatin [files/dirs…]` (or none)| Launch **GUI**, pre-queueing any passed paths. |
| `kuvatin --preset <name> <files…>`| Run a **quick batch** headlessly with a minimal progress window that auto-closes on success; on error the window stays open showing failures. Called by context-menu quick actions. |
| `kuvatin --register` / `--unregister` | Write / remove Explorer context-menu registry keys. Invoked by the installer (and available manually). |

Argument parsing via `clap`. The binary uses `anyhow` for top-level error context.

### 3.3 Presets storage

Presets serialize to TOML at `%APPDATA%\Kuvatin\presets.toml` (path resolved via the
`directories` crate). A small set of built-in presets ships on first run
(e.g. "Convert to WebP", "Resize to 1080p", "Resize to 50%").

## 4. GUI (Slint)

Single main window:

- **Drop zone + file list:** drag-and-drop or add files/folders; per-row thumbnail,
  filename, and status (queued / done / error+reason).
- **Preset panel (right):** active preset editor — output **format + quality**,
  **resize** mode (pixels / percent / fit-to-box, aspect-lock toggle),
  **crop** (none / fixed size / aspect ratio), and **output policy** (token pattern preview).
- **Preset dropdown:** load / save / save-as / delete presets.
- **Convert button + overall progress bar.**

Threading: the batch runs on a worker (rayon) thread; progress events are marshalled
back to the UI thread via `slint::invoke_from_event_loop`. The GUI layer stays thin —
all real logic lives in `kuvatin-core` so it is testable without a display.

## 5. Output policy (default)

Sibling-with-suffix. Example: `photo.jpg` resized to 1920×1080 as WebP →
`photo_1920x1080.webp` written next to the original. Never overwrites; collision →
`photo_1920x1080-1.webp`. The token pattern is editable per preset.

## 6. Explorer context-menu integration (v1: classic registry verbs)

**Mechanism (v1):** classic registry shell verbs under
`HKCU\Software\Classes\SystemFileAssociations\image\shell\Kuvatin`, using a
`CommandStore` (`ExtendedSubCommandsKey` / `SubCommands`) to present a **cascading
submenu**. This requires no COM or packaging, works directly on Windows 10, and appears
under "Show more options" on Windows 11.

**Submenu contents:**
- `Convert to WebP` → `kuvatin --preset "Convert to WebP" <selected>`
- `Resize to 1080p` → `kuvatin --preset "Resize to 1080p" <selected>`
- `Resize to 50%`  → `kuvatin --preset "Resize to 50%" <selected>`
- `Open in Kuvatin…` → `kuvatin <selected>` (full GUI)

Registration is implemented in `src/shell/windows.rs` (`#[cfg(windows)]`) via the
`windows` crate (registry APIs), exposed through `--register` / `--unregister`. The
module is isolated so a future `IExplorerCommand` (MSIX/sparse-package) upgrade — for a
top-level Win11 menu — or a macOS Finder/Quick-Action equivalent can be added without
touching `kuvatin-core`.

Per-user `HKCU` registration is used (no elevation required for the keys themselves).

## 7. Installer

`cargo-wix` produces a **`.msi`** that:
- installs `kuvatin.exe` (and the bundled Slint assets),
- runs `kuvatin --register` on install,
- runs `kuvatin --unregister` on uninstall.

Code-signing is supported by the toolchain but not required to build; left as a
release-time concern.

## 8. Error handling

- **Core:** typed `CoreError` (via `thiserror`). Batch execution isolates per-file
  failures — each is captured as a result entry; the batch always runs to completion.
- **Binary/GUI:** `anyhow` for top-level context. The GUI surfaces per-file errors inline
  in the file list (status + reason). The quick-batch progress window keeps itself open
  when any file failed so the user sees what went wrong.

## 9. Testing strategy

- **Core unit tests:** output-name token expansion and collision handling; each `Op`
  (resize dimensions/aspect math, crop bounds, convert format round-trips).
- **Golden-image tests:** small fixture images processed and compared against expected
  outputs (dimensions + format; pixel tolerance where encoders are lossy).
- **Integration test:** run a full batch `Job` over a temp directory of fixtures and
  assert the produced files, names, and that one deliberately-corrupt input is reported
  as a failure without aborting the batch.
- GUI logic kept minimal; no display-dependent tests required for v1.

## 10. Key dependencies

| Concern            | Crate |
|--------------------|-------|
| Image IO           | `image` |
| Lossy WebP encode  | `webp` (libwebp bindings) |
| Resampling         | `image::imageops` (v1); `fast_image_resize` (post-v1) |
| Parallelism        | `rayon` |
| GUI                | `slint` |
| CLI args           | `clap` |
| Config paths       | `directories` |
| Serialization      | `serde`, `toml` |
| Errors             | `thiserror` (core), `anyhow` (bin) |
| Windows registry   | `windows` |
| Installer          | `cargo-wix` |

## 11. Future (post-v1)

- v1.1: interactive per-image crop selection in the GUI.
- Modern Win11 top-level context menu via `IExplorerCommand` + MSIX/sparse package.
- macOS/Linux GUI packaging and a macOS Finder shell-integration module.
- Optional higher-quality JPEG via `mozjpeg`.
