# Image-sequence import (Videos mode)

> **Goal:** point Kuvatin at the *first file* of a numbered image sequence
> (`.png` / `.jpg` / `.exr`); the app finds the sequentially named files in the
> same directory, puts the sequence on the video timeline as a clip (one file =
> one frame at a chosen fps), and the existing Export renders it to video.

## Validated spikes (2026-08-27)

- GStreamer's `imagesequencesrc` (in `gstmultifile.dll`, gst-plugins-good, SDK
  1.26.11) registers the **`imagesequence://` URI scheme**. `gst-discoverer` on
  `imagesequence:///C:/dir/frame_%2504d.png?start-index=10&framerate=30/1`
  reports the correct duration/framerate and `Seekable: yes` — so GES
  `UriClipAsset::request_sync` + `UriClip` (the app's normal clip path) work on
  it unchanged, including trims, transforms, compositing and render.
- The `%` in the printf pattern must be percent-encoded (`%25`) in the URI;
  UTF-8 (`Työt`) and spaces in paths work percent-encoded — `glib`'s
  `filename_to_uri` produces exactly this encoding.
- The MSI bundles **all** plugins (`bundle-gstreamer.ps1` copies
  `lib\gstreamer-1.0\*.dll`), so `gstmultifile` + `gstpng` + `gstjpeg` already
  ship. **No packaging change.**
- The official MSVC GStreamer binaries have **no OpenEXR plugin**, so `.exr`
  cannot go through GStreamer. EXR sequences are pre-converted to PNG in Rust
  (`image` crate decodes EXR; linear-light → sRGB tonemap) into a temp cache,
  then imported as a PNG sequence.

## Changes

### `kuvatin-video` — new `sequence` module

- `SequenceSpec { dir, prefix, suffix, pad, start, count, fps }` (+ `Clone`,
  `Send`): describes `prefix<NUMBER>suffix` files in `dir`.
- `detect_sequence(first_file) -> Result<SequenceSpec>`: parse the **last** run
  of digits in the stem (zero-padded iff it has a leading zero), then count
  consecutive existing files forward from the picked index.
- `SequenceSpec::uri()`: pattern path → `filename_to_uri` (handles all
  percent-encoding) → swap scheme to `imagesequence://` + append
  `?start-index=&framerate=`.
- `convert_exr_sequence(spec, progress, cancel) -> Result<SequenceSpec>`:
  rayon-parallel decode EXR → sRGB-encode → PNG under
  `%TEMP%/kuvatin/seq-cache/<content-hash>/f%06d.png`; completeness marker file;
  cache hit = instant reuse (keyed on path/pattern/count + first/last mtimes,
  fps-independent). `sweep_sequence_cache(max_age)` clears stale entries at
  startup.
- `warm_asset_uri` / `thumbnail_uri` / `Project::append_clip_uri` — URI
  generalizations of the existing path-based functions (which now delegate).
- New deps: `image`, `rayon` (workspace versions).

### `kuvatin` GUI

- Media-bin card: **“Import sequence…”** button → pick first frame (filter
  png/jpg/jpeg/exr) → `detect_sequence` → confirm dialog (`seq-config`) showing
  `first → last`, frame count, duration, and a FRAME RATE row (default 30,
  pills 24/25/30/60) → **Add to timeline**.
- Import runs on a worker thread (EXR conversion progress feeds the existing
  import modal; Cancel aborts conversion), then `warm_asset_uri` + thumbnail;
  the import timer drains the result on the UI thread and appends the clip to
  the **base video track** (track 1). Bin entry keyed by the first file's path;
  a `path → SequenceSpec` map routes bin re-adds.
- `TimelineClip.kind = 2` (sequence): violet clip gradient, no audio in the
  inspector. Everything else (slide/trim/transform/export) is the normal clip
  machinery.

## Tests

- Pure: pattern detection (padded/unpadded/mid-pick/gap/no-digits/last-run),
  URI encoding.
- EXR: generate Rgb32F EXRs → convert → assert PNGs + marker + sRGB mapping +
  cache reuse.
- Gated runtime (needs GStreamer, `--test-threads=1`): generate PNGs, detect,
  `append_clip_uri` into a `Project`, assert timeline duration = count/fps and
  preview frames arrive; render to WebM and assert a non-trivial file.
