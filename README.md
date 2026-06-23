# Kuvatin

Compact native batch image **compressor / converter / resizer / cropper** for
**Windows and macOS**, with a right-click menu integration (Explorer context menu
on Windows, Finder Quick Actions on macOS). Built in Rust with a
[Slint](https://slint.dev) UI.

*Kuvatin* — from Finnish *kuva* ("image").

🌐 **[Landing page](https://ville-mattila.github.io/Kuvatin/)** · 📦 **[Download the installer](https://github.com/Ville-Mattila/Kuvatin/releases/latest)**

## Features

- **Compress PNG** — lossy compression via
  [libimagequant](https://pngquant.org/lib/) (the pngquant engine) finished with an
  [oxipng](https://github.com/shssoichiro/oxipng) pass, or fully **lossless**
  oxipng-only optimization. This is the default preset.
- **Convert** between PNG, JPEG, WebP, BMP, TIFF, GIF (quality control for the
  lossy formats; the quality slider hides itself when it doesn't apply)
- **Resize** by pixels, percent, or fit-to-box (aspect-ratio aware, Lanczos3 resampling)
- **Crop** to a fixed size or aspect ratio, with numeric width/height fields
- **Batch** whole folders / multi-selections in parallel, with reusable **presets**
  (stored in `%APPDATA%\Kuvatin\presets.toml`)
- **Right-click menu**: quick preset actions on selected images
  (Convert to WebP, Resize to 1080p, Resize to 50%) or "Open in Kuvatin…" —
  an Explorer context menu on Windows, Finder Quick Actions on macOS
- **Drag-and-drop** images straight onto the window (Windows and macOS)
- **Custom frameless window** with a native drag/resize titlebar on Windows;
  native window decorations on macOS

Outputs are written next to the originals with a token-pattern name
(default `{name}_{w}x{h}.{ext}`) and are never overwritten (collisions get `-1`, `-2`, …).

## Install

**Windows** — grab the latest `.msi` from the
[releases page](https://github.com/Ville-Mattila/Kuvatin/releases/latest).
It adds a Start-menu shortcut, registers the Explorer context menu, and always
upgrades any previous version in place (no duplicate installs).

**macOS** — grab the latest universal `.dmg` (Apple Silicon + Intel) from the
[releases page](https://github.com/Ville-Mattila/Kuvatin/releases/latest) and drag
**Kuvatin** into Applications. On first launch it installs the Finder Quick Actions.

> The macOS build is **not notarized**, so Gatekeeper blocks it on first open.
> Either right-click the app → **Open** → **Open**, or clear the quarantine flag:
>
> ```sh
> xattr -dr com.apple.quarantine /Applications/Kuvatin.app
> ```

## Build & run

Requires the Rust toolchain and a C compiler (the `webp` / libimagequant
dependencies compile C): the Visual Studio C++ Build Tools on Windows, or the
Xcode Command Line Tools (`xcode-select --install`) on macOS.

```sh
cargo build --release      # build everything
cargo run -p kuvatin       # launch the GUI
cargo test                 # run the test suite
```

Release artifacts (the Windows `.msi` and the macOS universal `.dmg`) are built in
GitHub Actions — see [`.github/workflows/release.yml`](.github/workflows/release.yml).
The macOS `.dmg` is assembled by
[`scripts/build-macos-dmg.sh`](scripts/build-macos-dmg.sh).

## Right-click menu (without the installer)

```sh
kuvatin --register     # add the menu entries (per-user)
kuvatin --unregister   # remove them
```

- **Windows**: registers a Kuvatin submenu under HKCU. On Windows 11 the entries
  appear under "Show more options"; on Windows 10 directly in the context menu.
- **macOS**: writes Automator Quick Actions into `~/Library/Services`, which show
  up in Finder's right-click **Quick Actions** section for image files. The app
  also installs/updates these automatically on launch, and the **Settings** panel
  has a toggle to enable or disable them.

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
  the parallel batch executor. Fully unit-tested.
- **`crates/kuvatin`** — the `kuvatin` binary: Slint GUI + CLI + the shell
  integration (Windows registry / macOS Finder Quick Actions, behind a per-OS
  `shell` module). Runs in three modes — GUI, `--preset` quick batch, and
  `--register` / `--unregister`. Native drag-and-drop is wired per-OS (Win32
  `WM_DROPFILES` / an AppKit dragging-destination overlay) into a shared queue.

See [`docs/superpowers/specs/`](docs/superpowers/specs/) for the design and
[`docs/superpowers/plans/`](docs/superpowers/plans/) for the implementation plan.

## License

[GPL-3.0-or-later](LICENSE). Kuvatin links libimagequant, which is GPL-licensed for
this kind of use, so the whole application is distributed under the GPL.

## Status

Working today: compress / convert / resize / crop / batch / presets / right-click
menu / drag-and-drop on Windows and macOS / `.msi` installer with Start-menu
shortcut / universal macOS `.dmg`, all built in GitHub Actions.
Deferred to later: interactive per-image crop in the GUI, a top-level Windows 11 menu via
`IExplorerCommand`, macOS code signing / notarization, and Linux packaging.
