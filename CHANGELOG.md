# Changelog

Every released version, newest first. The full notes for a release — with
screenshots and the installer checksum — live on its
[releases page](https://github.com/Ville-Mattila/Kuvatin/releases); this file
is the record that does not depend on one.

The release pipeline reads the section matching the tag and publishes it as the
release body, so **a tag with no section here fails the release**. Write the
section first, then tag.

Dates are the release dates. Versions follow [semantic versioning](https://semver.org/):
the minor number moves when something new appears, the patch number when
something is fixed.

## [Unreleased]

## [2.11.0] - 2026-09-13
A smaller release, mostly about images. WebP can now be lossless, for the
screenshots and logos where the lossy encoder smears hard edges and saves
little doing it. A large batch no longer runs the machine out of memory:
files start only as fast as memory allows, so a folder of big photos saved as
optimised PNG stays within half the machine's RAM instead of taking all of it.
And the release's bill of materials now lists the media runtime the installer
bundles, file by file.

### Added
- **Lossless WebP.** A WEBP OPTIMIZATION control next to the format, the same
  shape as the PNG one: lossy (the default, driven by the quality slider) or
  lossless. Lossless returns every pixel unchanged — including the colour
  under fully transparent ones, which libwebp is otherwise free to rewrite —
  and hides the quality slider, which it does not use.
- **The release SBOM names the bundled media runtime.** It used to list only
  the Rust crates, while most of what the installer redistributes is GStreamer.
  Every bundled plugin and library is now in it with its SHA-256 and where it
  installs, under the runtime's version, its upstream download and source, and
  the license texts that ship with it.

### Fixed
- **A large batch can no longer run the machine out of memory.** Every core
  converted a file at once, whatever the files cost: sixteen 12-megapixel
  photos saved as optimised PNG peaked at 6.8 GB. Each file is now priced from
  its header before it starts, against what its encoder was measured to need,
  and a batch runs only as much at once as fits in half the machine's RAM. A
  file bigger than that still converts, on its own.

Checksum: see `kuvatin-2.11.0-x86_64.msi.sha256` (and
`kuvatin-x86_64.msi.sha256` for the fixed-name copy the site links). The
installer itself is unsigned, so SmartScreen will ask once — More info, then
Run anyway. The Windows 11 menu package inside it *is* signed, which is what
puts "Kuvatin" in the top-level right-click menu.

## [2.10.0] - 2026-09-12
The video editor learns to remember. A project — the timeline, the tracks,
every clip's trim and transform — saves to a file and comes back. Neither end
of an export freezes the window any more, a hardware encoder that fails no
longer loses the export, and the timeline zooms. On the image side, a photo's
colour profile and EXIF now survive the conversion. And most of the app can be
driven from the keyboard: the file list, the sliders, the dialogs, and the
clips on the timeline.

### Added
- **The export encoder is a choice, and a failing one is no longer fatal.**
  Automatic, hardware only, or software only; under automatic, a hardware
  encoder that fails mid-export is retried in software rather than failing the
  export with an engine message. The finished dialog names the encoder that
  produced the file.
- **A video project can be saved and reopened.** The timeline, the tracks,
  every clip's trim and transform and the canvas go into a readable `.kuvatin`
  file; media is referenced rather than copied, and a source that has moved is
  named on opening while everything else still opens. Ctrl+S and Ctrl+O, or
  the buttons under the media bin.
- The Convert button becomes **Cancel** while a batch is running, and Esc stops
  it too. The file being written finishes; the rest of the queue is reported as
  cancelled rather than converted.
- A finished export says so: a dialog naming the file, how long it took, and a
  **Show in folder** button. The image batch summary gets the same button.
- During a long export, elapsed time and an estimate of what is left, instead of
  a bare percentage.
- A weekly scheduled CI run, so an advisory published in a quiet week is still
  found.
- Keyboard control of the sliders: the quality setting, the video scrubber and
  every inspector transform take focus and answer to the arrows, Home and End.
- Dialogs take the keyboard when they open and run their primary action on
  Enter.
- Timeline clips can be selected, moved, re-tracked and trimmed from the
  keyboard, and they announce themselves to assistive technology.
- The timeline zooms: buttons, Ctrl+plus/minus/0, or Ctrl+wheel over the lane,
  with a Fit that scales the whole timeline to the window. A plain wheel
  scrolls it, which the scrollbar used to be the only way to do.

### Changed
- One memory budget for the whole image path. Every decode runs under an
  explicit 1 GiB ceiling — including the GIF path, which had no limits at all —
  and a resize target is scaled down to the same budget instead of being allowed
  an allocation that would abort the process.
- The colour profile and EXIF of an input now travel to the output wherever the
  container can carry them (PNG, JPEG, WebP, TIFF), with the orientation tag
  neutralised so nothing rotates twice.
- The installer refuses to run below Windows 8.1, asks a running Kuvatin to
  close before replacing its files, and never asks for a reboot.
- `scripts/release.ps1` runs format, lint and tests before it commits or tags.

### Fixed
- Starting and finishing an export no longer freeze the window for seconds.
  Both ends wait for a pipeline transition, and both waits used to run on the
  interface thread; they are now polled from the progress modal, which says
  "Starting…" and "Finishing…" while it waits.
- A dragged clip can no longer be buried under another on the same layer: it
  stops against its neighbour instead of hiding it.
- A large Explorer selection on a loaded machine no longer splits into two
  batches with two progress windows.
- Asset discovery gives up after twenty seconds instead of wedging the import
  worker for the rest of the session on one unreachable file.
- Dropping a folder, or picking the first frame of a sequence, no longer freezes
  the window while the filesystem is walked.
- The progress window could hang a fast right-click run: the quit posted before
  the event loop started was dropped, leaving the process waiting on a window
  that was never shown.
- Transparent edges no longer fringe in greyscale or 16-bit images (the
  premultiplied resample path covered only RGBA8).

Checksum: see `kuvatin-2.10.0-x86_64.msi.sha256` (and
`kuvatin-x86_64.msi.sha256` for the fixed-name copy the site links). The
installer itself is unsigned, so SmartScreen will ask once — More info, then
Run anyway. The Windows 11 menu package inside it *is* signed, which is what
puts "Kuvatin" in the top-level right-click menu.

## [2.9.2] - 2026-09-11

- Only one "Kuvatin" entry in the right-click menu again. Windows 11 lists a
  packaged handler in its own menu *and* under "Show more options", so the
  classic entries now step aside while the Windows 11 entry is registered — and
  come back automatically where it is not (Windows 10, or a machine where the
  package could not register).

## [2.9.1] - 2026-09-10

- **The Windows 11 context menu goes live.** "Kuvatin" sits in the top-level
  right-click menu, with the same submenu as before, read from your presets when
  the menu opens. A multi-selection is one command, one batch, one summary.
- The installer registers a small signed package that gives the install folder
  package identity (what Windows 11 requires of top-level entries), trusts its
  certificate machine-wide, and removes both on uninstall. Registration is per
  user and heals itself when Kuvatin starts.

## [2.9.0] - 2026-09-10

- **Presets can be renamed and reordered**; the Explorer submenu follows the
  order, and a duplicate name is refused rather than silently merged.
- **Keyboard control of the image list**: Up/Down/Home/End, Delete, Ctrl+O,
  Ctrl+S, Ctrl+Enter, Esc.
- **A duration field for stills** in the video inspector.
- **An opt-in update check**, off by default: one HTTPS HEAD a day, nothing else.
- The Windows 11 menu handler and package ship, inactive, awaiting a signed
  build.

## [2.8.1] - 2026-09-10

- Keyboard shortcuts kept working after a mouse click (focus returned to the
  window instead of being swallowed by the clicked control).
- Firmer diagnostics for right-click runs.

## [2.8.0] - 2026-09-09

- A result summary for batch conversions: how many, how much smaller, and which
  files failed.
- Sturdier presets, and diagnostics for a machine you cannot see.

## [2.7.0] - 2026-09-09

- A hardening release: every finding of the September 2026 code review
  addressed. The same app, more robust, smaller and faster.

## [2.6.0] - 2026-09-07

- Right-click runs report what they are doing: a progress window for anything
  that outlives a short grace period, with cancellation.

## [2.5.0] - 2026-09-07

- Rendered frames to finished video straight from the right-click menu.

## [2.4.0] - 2026-09-07

- The right-click menu works on **folders**, and a multi-selection converts as
  **one batch**.

## [2.3.0] - 2026-08-27

- **Image sequence to video**: point Kuvatin at the first frame and it finds the
  rest.

## [2.2.0] - 2026-07-19

- An animation pass over the whole app, event-driven, so idle CPU stays at zero.

## [2.1.0] - 2026-07-04

- Flip through images with the mouse wheel.

## [2.0.1] - 2026-07-03

- Post-2.0 hardening: crashes, data-loss risks and silent failures found by a
  full-codebase audit. No new features.

## [2.0.0] - 2026-07-02

- **The video editor**: a timeline, a preview, and hardware-accelerated export.

## [1.5.0] - 2026-06-23

- The last image-only release.

## [1.4.0] - 2026-06-22

- **Choose where to save**: a Save dialog for one file, a folder picker for a
  batch.
- A file-name suffix field, saved with the preset, and an optional subfolder.
- Explorer drag-and-drop works again.

## [1.3.1] - 2026-06-22

- Patch release.

## [1.3.0] - 2026-06-22

- The app icon everywhere: taskbar, window, Add/Remove Programs, Start menu.
- **Compress PNG** becomes the default preset (libimagequant plus a lossless
  oxipng pass).
- A bigger default window.

## [1.2.0] - 2026-06-22

- **PNG size optimization**: normal, lossless (oxipng) or lossy
  (libimagequant), transparency preserved in all three.
- The installer always replaces the old version rather than leaving a second
  copy.

## [1.1.1] - 2026-06-22

- Window interaction fixed in the custom frameless window.

## [1.1.0] - 2026-06-22

- First public release: a compact native Windows batch image converter,
  resizer and cropper with Explorer context-menu integration.

[Unreleased]: https://github.com/Ville-Mattila/Kuvatin/compare/v2.11.0...HEAD
[2.11.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.11.0
[2.10.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.10.0
[2.9.2]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.9.2
[2.9.1]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.9.1
[2.9.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.9.0
[2.8.1]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.8.1
[2.8.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.8.0
[2.7.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.7.0
[2.6.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.6.0
[2.5.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.5.0
[2.4.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.4.0
[2.3.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.3.0
[2.2.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.2.0
[2.1.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.1.0
[2.0.1]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.0.1
[2.0.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.0.0
[1.5.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v1.5.0
[1.4.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v1.4.0
[1.3.1]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v1.3.1
[1.3.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v1.3.0
[1.2.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v1.2.0
[1.1.1]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v1.1.1
[1.1.0]: https://github.com/Ville-Mattila/Kuvatin/releases/tag/v1.1.0
