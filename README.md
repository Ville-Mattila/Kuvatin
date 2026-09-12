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
- **Colour profile and EXIF are carried to the output** (PNG, JPEG and WebP —
  BMP and GIF have nowhere to put them), so a Display P3 photo or an Adobe RGB
  scan keeps its colours and a photo keeps its capture date, camera and
  copyright. The orientation tag is reset to "normal" on the way out, because
  the rotation is already baked into the pixels
- **Explorer context menu**: right-click images **or folders** for **every
  preset in your store** (the built-ins and the ones you save — the submenu
  is rewritten whenever presets change), "Open in Kuvatin…",
  or **Render image sequence to MP4** (right-click any `frame_0001.png`-style
  frame — or a folder of them — and the whole numbered run becomes an H.264
  MP4 next to it, at native resolution) — a multi-selection runs as **one
  batch**, not one process per file, with a small **progress window** (and
  Cancel) for anything longer than an instant
- **Custom frameless window** with a native drag/resize titlebar and drag-and-drop

Outputs are written next to the originals with a token-pattern name
(default `{name}_{w}x{h}.{ext}`) and are never overwritten (collisions get `-1`, `-2`, …).

### Presets and keyboard

Presets can be renamed (type the new name, click **Rename**) and reordered
with the arrows next to the preset list; the Explorer submenu follows that
order. In Images mode: **Up/Down/Home/End** walk the file list, **Delete**
removes the selected file, **Ctrl+O** adds files, **Ctrl+S** saves the
preset, **Ctrl+Enter** converts, **Esc** closes the topmost dialog (and
cancels a running conversion). The quality slider takes focus and answers to
the arrows, **Home** and **End**.

In Videos mode: **Space** plays and pauses, **Left/Right** walk the clips on
the timeline, **Ctrl+Left/Right** slide the selected clip by a tenth of a
second, **Ctrl+Up/Down** move it to another track, **Shift+Left/Right** trim
its right edge, and **Delete** removes it. **Ctrl+S** saves the project (to
the file it came from, or asks the first time) and **Ctrl+O** opens one. The
scrubber and the inspector sliders take focus and answer to the arrows too. The timeline zooms with the
**- / Fit / +** buttons, **Ctrl+plus / Ctrl+minus / Ctrl+0**, or **Ctrl+wheel**
over the lane; a plain wheel scrolls it.

## Video features (new in 2.0)

- **Save and reopen a project** — the timeline, the tracks, every clip's trim
  and transform, and the canvas size go into a readable `.kuvatin` file
  (TOML). Media is referenced, not copied; a source that has moved is named
  when the project opens and the rest still opens

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
the installer bundles a trimmed subset of the GStreamer runtime (only what the app
actually loads, with every component's license text), so nothing needs to be
installed separately.

## Install

Grab the latest `.msi` from the [releases page](https://github.com/Ville-Mattila/Kuvatin/releases/latest)
(direct link: [kuvatin-x86_64.msi](https://github.com/Ville-Mattila/Kuvatin/releases/latest/download/kuvatin-x86_64.msi)).
It adds a Start-menu shortcut, registers the Explorer context menu, and always
upgrades any previous version in place (no duplicate installs). The context-menu
registration is per-user and **self-heals at app launch**, so other Windows users
on the same machine get the menu the first time they open Kuvatin.

The installer is **not code-signed** (no certificate yet), so Windows
SmartScreen shows an "unknown publisher" prompt — choose *More info → Run
anyway*. Every release ships a `.sha256` file next to the `.msi`; compare it
with `Get-FileHash` before running the installer if you want to be sure of
what you downloaded. The release job also installs the built MSI on a clean
runner and converts an image with the installed copy before publishing.

## Licensing & third-party notices

Kuvatin is GPL-3.0-or-later. The installer bundles a trimmed subset of the
official GStreamer 1.26.11 runtime (LGPL/GPL/BSD components); it installs the
full license text of every component under `licenses\` next to the exe plus a
generated `THIRD-PARTY-NOTICES.txt`, and the corresponding source is linked
from [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

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

On Windows 11 the "Kuvatin" entry sits in the top-level context menu: `--register`
also registers `Kuvatin.msix`, a sparse package (identity only, no files of its own)
whose external location is the install folder, which lets Explorer load the
`IExplorerCommand` handler in `kuvatin_shellext.dll` (crate `kuvatin-shellext`,
hosted in a COM surrogate, never inside Explorer). The package is registered per
user, like the classic menu, and reads the preset store when the menu opens, so
it never needs re-syncing. The classic registry verbs are written too (Windows 10
shows them directly, Windows 11 under "Show more options"). The submenu is
attached to every accepted image extension, to folders, and to a folder's
background (right-click inside an open folder).

### Signing the menu package

Windows refuses an unsigned package that hosts a COM server (`0x80073D2B`), so
the Windows 11 entry only appears when `Kuvatin.msix` is signed with a
certificate the machine trusts. The release pipeline signs it when the
repository has two secrets, `KUVATIN_SIGN_PFX_BASE64` (a code-signing PFX,
base64) and `KUVATIN_SIGN_PFX_PASSWORD`; the manifest's Publisher is taken from
the certificate's Subject. The public certificate is installed with the app and
the installer adds it to the machine's **Trusted People** store before
registering (and removes it on uninstall); a CA-issued certificate needs no
such trust but is not hurt by it. Without the secrets the package ships
unsigned, `--register` logs `Windows 11 menu: not registered (...)`, and the
classic menu is all there is. A self-signed key is enough:

```powershell
$c = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Ville Mattila' `
  -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddYears(5) -KeyUsage DigitalSignature `
  -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
$pw = Read-Host -AsSecureString 'PFX password'
Export-PfxCertificate -Cert $c -FilePath kuvatin-sign.pfx -Password $pw | Out-Null
gh secret set KUVATIN_SIGN_PFX_BASE64 --body ([Convert]::ToBase64String([IO.File]::ReadAllBytes('kuvatin-sign.pfx')))
gh secret set KUVATIN_SIGN_PFX_PASSWORD   # paste the password
```

Keep the `.pfx` out of the repository. For local testing, build the package
(`crates\kuvatin\msix\build-msix.ps1`), sign it with `signtool sign /fd SHA256
/sha1 <thumbprint> Kuvatin.msix`, import the `.cer` into
`Cert:\LocalMachine\TrustedPeople` (elevated), put the `.msix` next to the exe
and run `--register`.
Registration is explicit: a debug build never touches the menu, and a release
build only self-registers when nothing owns the menu yet or the registered exe
no longer exists — so a portable or test copy can't hijack an installed one.
Run `--register` from the copy you want the menu to use. Explorer launches a
classic verb once per selected item; Kuvatin folds those launches into a single
batch at startup (a short rendezvous in `%TEMP%\kuvatin\rendezvous`), so
selecting fifty files runs one conversion with one summary — and "Open in
Kuvatin…" opens one window with all of them.

## Headless quick conversion (used by the context menu)

```powershell
kuvatin --preset "Convert to WebP" image1.png image2.jpg
kuvatin --sequence-mp4 --fps 24 render\frame_0001.png   # → render\frame.mp4 (default 30 fps)
```

## Installer (.msi)

Builds a Windows installer that registers/unregisters the context menu and installs
the Start-menu shortcut automatically. Requires
[cargo-wix](https://volks73.github.io/cargo-wix/) and the WiX Toolset v3 — see
[`crates/kuvatin/wix/README.md`](crates/kuvatin/wix/README.md) for the exact setup and the
correct build command.

```powershell
cargo install cargo-wix
# install WiX v3, build the release exe (its imports seed the DLL closure), then stage + build:
cargo build --release -p kuvatin
crates\kuvatin\wix\bundle-gstreamer.ps1 -StageDir target\gst-staging
# the Windows 11 menu package; main.wxs always references it
cargo build --release -p kuvatin-shellext
crates\kuvatin\msix\build-msix.ps1 -Version 0.0.0 -Out target\msix\Kuvatin.msix
cd crates\kuvatin
cargo wix -p kuvatin --compiler-arg "-dGstStageDir=..\..\target\gst-staging" --compiler-arg "-dMsixPath=..\..\target\msix\Kuvatin.msix"
# produces target/wix/kuvatin-<version>-x86_64.msi (~33 MB with the trimmed bundled runtime + licenses)
```

The package built this way is unsigned, so the installer will carry it but the
Windows 11 top-level menu stays inactive; see "Signing the menu package".

## Cutting a release

```powershell
scripts\release.ps1 2.8.0 -Push
```

Bumps the version in `Cargo.toml`, `Cargo.lock` and the landing page's
JSON-LD (CI cross-checks all three against the tag), commits, tags `v2.8.0`
and pushes. The tag run tests, builds the MSI, installs it on the runner and
exercises the installed exe, then publishes the release; add notes with
`gh release edit v2.8.0 --notes-file notes.md`.

## Diagnostics

Headless runs (the Explorer menu) append to `%LOCALAPPDATA%\Kuvatin\kuvatin.log`
(1 MB, one older generation kept); a crash writes `crash.log` next to it and,
in the GUI, shows the error dialog.

**Update check (opt-in).** Settings > Updates > *Check for updates once a day*
is off by default; Kuvatin makes no network request unless you turn it on.
When on, it sends one `HEAD` request a day to
`github.com/Ville-Mattila/Kuvatin/releases/latest` (User-Agent
`Kuvatin/<version>`) and reads the redirect target for the latest tag. Nothing
else is sent. A newer version shows as a badge that opens the releases page.
The setting lives in `%APPDATA%\Kuvatin\settings.toml`.

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
  (registry) integration. Runs in four modes — GUI, `--preset` quick batch,
  `--sequence-mp4` headless render, and `--register` / `--unregister` (`--quiet`
  suppresses the progress window and dialogs for installers and scripts).

See [`docs/superpowers/specs/`](docs/superpowers/specs/) for the design and
[`docs/superpowers/plans/`](docs/superpowers/plans/) for the implementation plans.

## Contributing

Issues and pull requests are welcome: [what to include and how to build](.github/CONTRIBUTING.md).
Something security-sensitive goes [here](.github/SECURITY.md) instead of into a
public issue. What changed in each version is in [CHANGELOG.md](CHANGELOG.md).

## License

[GPL-3.0-or-later](LICENSE). Kuvatin links libimagequant, which is GPL-licensed for
this kind of use, so the whole application is distributed under the GPL.

## Status

Working today: compress / convert / resize / crop / batch / presets / context menu /
image-sequence rendering / video timeline editing / hardware video export /
custom frameless UI / `.msi` installer with a trimmed, license-complete
GStreamer runtime and a Start-menu shortcut. What changed in each version is in
[CHANGELOG.md](CHANGELOG.md), and at greater length on the
[releases page](https://github.com/Ville-Mattila/Kuvatin/releases).

Every master push runs `cargo fmt --check`, `clippy -D warnings`, `cargo deny`
(licenses + advisories), the deterministic test suites and a headless smoke
render of the video engine. A tag, and any pull request touching the installer,
additionally builds the MSI, installs it on the runner, converts an image and
renders an MP4 with the installed copy, and checks that the Explorer entries
are registered and then removed again. A tag publishes the result with a
SHA-256 file and a CycloneDX SBOM.

Deferred to later: audio-only tracks and transitions in the video editor,
project save and load, undo, timeline zoom, and macOS/Linux packaging. The
top-level Windows 11 menu and signing of the menu package shipped in 2.9.1;
the installer itself is still unsigned, so SmartScreen still prompts once.
