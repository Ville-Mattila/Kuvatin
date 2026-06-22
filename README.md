# Kuvatin

Compact native Windows batch image **compressor / converter / resizer / cropper**,
with Explorer context-menu integration. Built in Rust with a custom-framed
[Slint](https://slint.dev) UI.

*Kuvatin* — from Finnish *kuva* ("image").

🌐 **[Landing page](https://ville-mattila.github.io/Kuvatin/)** · 📦 **[Download the installer](https://github.com/Ville-Mattila/Kuvatin/releases/latest)**

## Features

- **Compress PNG** — TinyPNG-grade lossy compression via
  [libimagequant](https://pngquant.org/lib/) (the pngquant/TinyPNG engine) with a
  final [oxipng](https://github.com/shssoichiro/oxipng) pass, or fully **lossless**
  oxipng-only optimization. This is the default preset.
- **Convert** between PNG, JPEG, WebP, BMP, TIFF, GIF (quality control for the
  lossy formats; the quality slider hides itself when it doesn't apply)
- **Resize** by pixels, percent, or fit-to-box (aspect-ratio aware, Lanczos3 resampling)
- **Crop** to a fixed size or aspect ratio, with numeric width/height fields
- **Batch** whole folders / multi-selections in parallel, with reusable **presets**
  (stored in `%APPDATA%\Kuvatin\presets.toml`)
- **Explorer context menu**: right-click images for quick preset actions
  (Convert to WebP, Resize to 1080p, Resize to 50%) or "Open in Kuvatin…"
- **Custom frameless window** with a native drag/resize titlebar and drag-and-drop

Outputs are written next to the originals with a token-pattern name
(default `{name}_{w}x{h}.{ext}`) and are never overwritten (collisions get `-1`, `-2`, …).

## Install

Grab the latest `.msi` from the [releases page](https://github.com/Ville-Mattila/Kuvatin/releases/latest).
It adds a Start-menu shortcut, registers the Explorer context menu, and always
upgrades any previous version in place (no duplicate installs).

## Build & run

Requires the Rust toolchain (MSVC). The `webp` dependency compiles libwebp via the
MSVC C compiler, so the Visual Studio C++ Build Tools must be installed.

```powershell
cargo build --release      # build everything
cargo run -p kuvatin       # launch the GUI
cargo test                 # run the test suite
```

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
# install WiX v3, then:
cd crates/kuvatin
cargo wix -p kuvatin
# produces target/wix/kuvatin-<version>-x86_64.msi
```

## Architecture

- **`crates/kuvatin-core`** — OS-agnostic engine: formats, PNG optimization
  (oxipng + libimagequant), resize, crop, output naming, the Job/Preset model, and
  the parallel batch executor. Fully unit-tested. Portable to macOS/Linux later.
- **`crates/kuvatin`** — the `kuvatin.exe`: Slint GUI + CLI + the Windows shell
  (registry) integration. Runs in three modes — GUI, `--preset` quick batch, and
  `--register` / `--unregister`.

See [`docs/superpowers/specs/`](docs/superpowers/specs/) for the design and
[`docs/superpowers/plans/`](docs/superpowers/plans/) for the implementation plan.

## License

[GPL-3.0-or-later](LICENSE). Kuvatin links libimagequant, which is GPL-licensed for
this kind of use, so the whole application is distributed under the GPL.

## Status

Working today: compress / convert / resize / crop / batch / presets / context menu /
custom frameless UI / signed-off `.msi` installer with Start-menu shortcut.
Deferred to later: interactive per-image crop in the GUI, a top-level Windows 11 menu via
`IExplorerCommand`, and macOS/Linux packaging.
