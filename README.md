# Kuvatin

Compact native Windows batch image **compressor / converter / resizer / cropper** —
and, since 2.0, a lightweight **video editor** with a layered timeline and
hardware-accelerated export. Explorer context-menu integration included. Built in
Rust with a custom-framed [Slint](https://slint.dev) UI on a
[GStreamer](https://gstreamer.freedesktop.org/) video engine.

*Kuvatin* — from Finnish *kuva* ("image").

🌐 **[Landing page](https://ville-mattila.github.io/Kuvatin/)** · 📦 **[Download the installer](https://github.com/Ville-Mattila/Kuvatin/releases/latest)**

## Image features

- **Compress PNG** — lossy compression via
  [libimagequant](https://pngquant.org/lib/) (the pngquant engine) finished with an
  [oxipng](https://github.com/shssoichiro/oxipng) pass, or fully **lossless**
  oxipng-only optimization. This is the default preset.
- **Convert** between PNG, JPEG, WebP, BMP, TIFF, GIF (quality control for the
  lossy formats; the quality slider hides itself when it doesn't apply)
- **Resize** by pixels, percent, or fit-to-box (aspect-ratio aware, Lanczos3 resampling)
- **Crop** to a fixed size or aspect ratio, inline in the viewer or with numeric fields
- **Batch** whole folders / multi-selections in parallel, with reusable **presets**
  (stored in `%APPDATA%\Kuvatin\presets.toml`) — one bad file can't take down a
  run, and **EXIF orientation** is applied automatically on decode
- **Explorer context menu**: right-click images for quick preset actions
  (Convert to WebP, Resize to 1080p, Resize to 50%) or "Open in Kuvatin…"
- **Custom frameless window** with a native drag/resize titlebar and drag-and-drop

Outputs are written next to the originals with a token-pattern name
(default `{name}_{w}x{h}.{ext}`) and are never overwritten (collisions get `-1`, `-2`, …).

## Video features (new in 2.0)

- **Layered timeline editor** — drag files straight onto the timeline; slide,
  edge-trim, and move clips across tracks with magnetic snapping; reorder tracks;
  drop below the last track to create a new one
- **Overlays & transforms** — stack videos and still images; position, scale,
  opacity and per-clip volume via the inspector or by dragging/resizing the clip
  right in the preview
- **Live composited preview** with scrubbing, repeat, and a master volume
- **Configurable canvas** — pick the project resolution (16:9, vertical, square,
  4K, or custom) independently of the export size
- **Export** to MP4 (H.264, **hardware NVENC** on NVIDIA GPUs with automatic
  software x264 fallback), WebM VP9 or VP8, with resolution, frame-rate and
  bitrate control — cancellable mid-render (the partial file is cleaned up)
- **Fast imports** — files load on a background thread with progress and
  per-clip thumbnails; imports are cancellable, de-duplicated, and unreadable
  files are reported instead of silently added
- **Keyboard & cleanup** — Space play/pause, Delete removes the selected clip
  (also via the × on clips and media-bin rows), Esc closes dialogs

The video engine is [GStreamer Editing Services](https://gstreamer.freedesktop.org/documentation/gst-editing-services/);
the installer bundles the full GStreamer runtime, so nothing needs to be
installed separately.

## Install

Grab the latest `.msi` from the [releases page](https://github.com/Ville-Mattila/Kuvatin/releases/latest).
It adds a Start-menu shortcut, registers the Explorer context menu, and always
upgrades any previous version in place (no duplicate installs). The context-menu
registration is per-user and **self-heals at app launch**, so other Windows users
on the same machine get the menu the first time they open Kuvatin.

## Build & run

Requires the Rust toolchain (MSVC). The `webp` dependency compiles libwebp via the
MSVC C compiler, so the Visual Studio C++ Build Tools must be installed. Video
support needs the **GStreamer MSVC dev SDK** (runtime + devel MSIs, default
install path) — [`.cargo/config.toml`](.cargo/config.toml) points the build at it.

```powershell
cargo build --release      # build everything
cargo run -p kuvatin       # launch the GUI
cargo test                 # run the test suite
```

The GStreamer-backed tests in `kuvatin-video` self-skip unless `GST_TEST_FILE`
points at a media file (and `GST_TEST_IMAGE` at a still image for the overlay
tests), and must run single-threaded
(`cargo test -p kuvatin-video -- --test-threads=1`) — concurrent GStreamer
pipelines deadlock.

## Explorer context menu (without the installer)

```powershell
cargo run -p kuvatin -- --register     # add the Kuvatin submenu (per-user, HKCU)
cargo run -p kuvatin -- --unregister   # remove it
```

On Windows 11 the entries appear under "Show more options"; on Windows 10 directly in
the context menu.

## Headless quick conversion (used by the context menu)

```powershell
kuvatin --preset "Convert to WebP" image1.png image2.jpg
```

## Installer (.msi)

Builds a Windows installer that registers/unregisters the context menu and installs
the Start-menu shortcut automatically. Requires
[cargo-wix](https://volks73.github.io/cargo-wix/) and the WiX Toolset v3 — see
[`crates/kuvatin/wix/README.md`](crates/kuvatin/wix/README.md) for the exact setup and the
correct build command.

```powershell
cargo install cargo-wix
# install WiX v3, then stage + harvest the GStreamer runtime and build:
crates\kuvatin\wix\bundle-gstreamer.ps1 -StageDir target\gst-staging
cd crates\kuvatin
cargo wix -p kuvatin --compiler-arg "-dGstStageDir=..\..\target\gst-staging"
# produces target/wix/kuvatin-<version>-x86_64.msi (~110 MB with the bundled runtime)
```

## Architecture

- **`crates/kuvatin-core`** — OS-agnostic image engine: formats, PNG optimization
  (oxipng + libimagequant), resize, crop, output naming, the Job/Preset model, and
  the parallel batch executor. Fully unit-tested. Portable to macOS/Linux later.
- **`crates/kuvatin-video`** — the GStreamer video engine: the GES-backed
  editing `Project` (layered timeline, per-clip transforms, composited preview,
  cancellable render-to-file with per-codec encoding profiles) plus asset
  utilities (off-thread discovery, thumbnails). Headless-tested against real
  pipelines.
- **`crates/kuvatin`** — the `kuvatin.exe`: Slint GUI + CLI + the Windows shell
  (registry) integration. Runs in three modes — GUI, `--preset` quick batch, and
  `--register` / `--unregister`.

See [`docs/superpowers/specs/`](docs/superpowers/specs/) for the design and
[`docs/superpowers/plans/`](docs/superpowers/plans/) for the implementation plans.

## License

[GPL-3.0-or-later](LICENSE). Kuvatin links libimagequant, which is GPL-licensed for
this kind of use, so the whole application is distributed under the GPL.

## Status

Working today: compress / convert / resize / crop / batch / presets / context menu /
video timeline editing / hardware video export / custom frameless UI / `.msi`
installer with bundled GStreamer runtime and Start-menu shortcut.

**2.0.1** is a hardening release from a full-codebase audit: crash-proof batch
encoding, collision-proof outputs, corruption-tolerant + versioned presets,
visible error dialogs (the windowed build has no console), cancellable
export/import, EXIF-aware decode, and a release pipeline that pins every
third-party download by SHA-256 and gates on the test suite.

Deferred to later: audio-only tracks & transitions in the video editor, a top-level
Windows 11 menu via `IExplorerCommand`, and macOS/Linux packaging.
