# Third-party notices

Kuvatin is free software under the **GNU General Public License, version 3 or
later** ([LICENSE](LICENSE)). It builds on, and its Windows installer
redistributes, software under other free licenses. This file is the
source-tree summary; the installer carries the full texts.

## Bundled GStreamer runtime (installer)

The `.msi` bundles a **subset of the official GStreamer 1.26.11 runtime for
Windows (MSVC x86_64)** — unmodified binaries from
<https://gstreamer.freedesktop.org/download/>: the shared libraries that
`kuvatin.exe` and the bundled plugins import, and an allow-list of plugins
(see `crates/kuvatin/wix/bundle-gstreamer.ps1` for the exact list and how the
closure is computed).

Those components are licensed under their own terms — mostly the **GNU LGPL
2.1 or later** (GStreamer, the FFmpeg build, GLib, …), some under the **GNU
GPL 2.0 or later** (for example x264), and others under BSD, MIT, MPL, Apache
or similar permissive licenses. The installer places:

- `licenses\<component>\…` — the complete license text and copyright notices
  of **every** component of the GStreamer distribution, one folder per
  component, exactly as upstream ships them in `share\licenses\`;
- `THIRD-PARTY-NOTICES.txt` — generated at build time, listing the plugins
  and shared libraries actually bundled in that build.

### Corresponding source

- GStreamer modules (gstreamer, gst-plugins-base/-good/-bad/-ugly, gst-libav,
  gst-editing-services, …) version 1.26.11:
  <https://gstreamer.freedesktop.org/src/>
- The Windows binaries are produced by GStreamer's Cerbero build system, whose
  recipes identify the exact upstream source of every third-party library:
  <https://gitlab.freedesktop.org/gstreamer/cerbero> (tag `1.26.11`)
- Kuvatin: <https://github.com/Ville-Mattila/Kuvatin>

Should any of the above be unavailable, corresponding source for the bundled
components will be provided on request via the Kuvatin issue tracker.

## Rust dependencies (compiled into `kuvatin.exe`)

Every crate and version is listed in [`Cargo.lock`](Cargo.lock); each crate's
license file is part of its published package on crates.io. Notably:

- **libimagequant** (`imagequant`) — GPL-3.0-or-later — the reason Kuvatin
  itself is GPL.
- **Slint** — GPL-3.0-only (via the GPL option of its licensing).
- GStreamer Rust bindings (`gstreamer*` crates) — MIT / Apache-2.0.
- The remainder — MIT, Apache-2.0, BSD or similarly permissive.

`cargo deny check licenses` (configured in [`deny.toml`](deny.toml)) verifies
that every crate's license is on the allowed list.
