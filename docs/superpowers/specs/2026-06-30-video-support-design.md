# Kuvatin 2.x — Video Support Design

> **For agentic workers:** This is the validated design (spec) for Kuvatin's video
> feature. It is implemented in **stages**; only **Stage 1** is specified in
> build-ready detail here. Later stages are captured as a roadmap and each gets
> its own spec → plan before implementation. The next step after this spec is the
> `writing-plans` skill, scoped to **Stage 1**.

## Goal

Add video support to Kuvatin, turning it from an image batch tool into a tool with
two modes: the existing **image** workflow and a new **video** workflow. The video
workflow grows, over several stages, from a media player into a small layered video
**compositor/editor** (timeline with per-layer trim, image overlays, and export).

The two modes share one central **Viewer** surface — building that viewer first in
the image world (where there is no media backend to fight) is what de-risks the
whole feature.

## Background — current state

- Cargo workspace: **`kuvatin-core`** (OS-agnostic image engine: `format`, `resize`,
  `crop`, `naming`, `pipeline`, `preset`, `batch`; built on the `image` crate) and
  **`kuvatin`** (Slint GUI + `clap` CLI + Windows-only `shell` registry integration).
- The GUI is a frameless Slint window: a custom 36px title bar over a body
  `HorizontalLayout` of a **Files** card and a **Settings/Output** card. There is
  **no large preview today** — only a thumbnail list. Cropping is a **separate
  editor** (`start-crop(idx)` → `cropping` state with `crop-image` + corner handles
  + numeric fields → `apply/cancel`).
- Windows-only. GPL-3.0-or-later (links libimagequant), so GPL-licensed media
  dependencies are fine.

## Guiding decisions (locked during brainstorming)

1. **Engine: GStreamer.** Slint ships an official `gstreamer-player` example
   (GStreamer → `slint::Image` via GL textures + YUV→RGB), so we follow the
   toolkit's supported path. `playbin` gives decode + **audio + A/V sync + broad
   formats** (mp4/h264, mov, mkv, webm) essentially for free; a `compositor`
   pipeline handles multi-layer rendering in later stages.
2. **Installer weight accepted.** Bundling the (trimmed) GStreamer runtime grows
   the installer from ~8 MB to roughly **40–70 MB**. This is an accepted, expected
   cost of "any format just works + audio."
3. **Media-player grade playback** is the bar: smooth playback, synced audio,
   frame-accurate scrubbing.
4. **Build in stages, no rush.** Ship a small, de-risked first release
   (single-clip playback), then grow the layered timeline/compositor, then export.
5. **Layout A + bottom strip.** Both modes use Files (left) · Viewer (center) ·
   Settings/Inspector (right). Video mode **also** has a bottom timeline band.
6. **Separate working set per mode.** Toggling Images/Videos swaps the visible UI
   and working set; it does not carry files across. Launch defaults to **Images**.

## Architecture

### Modes

- A top-of-window, **center** `Images | Videos` segmented toggle selects
  `AppMode { Images, Videos }`.
- Each mode owns an **independent** working set + selection + per-item edit state.
- File-type from the CLI / Explorer right-click **routes to the matching mode**
  (an image opens in Images, a video in Videos).

### Crate structure

- **`kuvatin-core`** — unchanged, **image-only**. No video code leaks in.
- **`kuvatin-video`** (new crate) — wraps GStreamer behind a clean Rust API the GUI
  consumes. The GUI never touches raw GStreamer. Exposes a `Player` controller
  (`load / play / pause / seek / step / position / duration / metadata`) and the
  frame path that feeds a `slint::Image`. Later stages add the timeline/compositor
  controller here. Keeps the gnarly media code isolated and unit-testable.
- **`kuvatin`** (GUI) — gains the mode toggle, the shared **Viewer** component, the
  video-mode UI, and orchestration of both engines.

### Shared Viewer component

A single Slint component renders the central surface for both modes:
- **Image mode:** the selected image, fit-to-view, with the **crop interaction
  inside it** (see below).
- **Video mode:** the GStreamer frame (Stage 1: one clip; later: the composited
  timeline output), with transport controls below.

The right column is **always** the settings/inspector, with mode-specific content.

## Image mode (Stage 1)

- Selecting a file shows it **large in the Viewer** (the app's first real preview).
- **Crop moves into the Viewer.** A `View / Crop` tool toggle sits at the
  viewer's top-left. In Crop mode, corner **handles are drawn on the image**, with
  a live `W × H` readout and aspect-lock.
- The crop's numeric controls (**X / Y / W / H + aspect**) live in the **viewer
  toolbar** as polished **custom dropdown editors** (click a value → small popover
  to view/edit it), keeping crop self-contained rather than scattering raw fields
  into the Settings pane.
- Crop is stored **per file** (as today, keyed by input path) and feeds that file's
  conversion job. A small **`crop` badge** marks cropped files in the list.
- All current batch-conversion functionality (formats, quality, PNG optimization,
  suffix, subfolder, presets, the Explorer context menu) is **retained unchanged**.
- The old separate crop editor is **removed** in favor of this inline interaction.

## Video mode (end-state vision)

This is the full target. **Stage 1 ships only the subset in the Stage 1 section
below.** The end state:

- **Media panel (left):** mixed **videos + images**. Images are first-class assets
  used as **overlays** (logos, titles, watermarks) and elements, not just videos.
- **Viewer (center):** a **live multi-layer compositor preview** — it decodes
  several sources at once and composites them (video base, image/video overlays on
  top) at the playhead, in real time, with a media transport below
  (play/pause, frame-step, scrubber, time, volume, fullscreen).
- **Inspector (right):** a real, **context-sensitive settings panel** (not a
  readout). Selecting a layer shows its **Transform (scale, position), Opacity,
  Volume, Speed, Trim in/out**; an always-available **Export** section holds output
  Format / Resolution / Quality and the **Render** action.
- **Timeline (bottom):** a **slim, resizable band**. Each clip or image is on its
  **own layer**; a layer **slides** horizontally to reposition in time and its
  **edges drag to trim**. **Higher layers composite on top** (that is how an image
  becomes an overlay). A red **playhead** drives the Viewer. The band is
  collapsible so it never dominates the window.

## Engine — GStreamer integration

- Follow the Slint **`gstreamer-player`** pattern: GStreamer produces frames that
  become a `slint::Image` (GL textures where available, CPU-accessible buffers
  otherwise, with YUV→RGB conversion). Crop/overlay UI draws **on top** of that
  image in Slint, so it composites naturally.
- **Stage 1:** a single-clip `playbin`, which provides audio, A/V sync, seeking,
  and frame-stepping out of the box. The `Player` controller owns the pipeline and
  surfaces state (position, duration, playing/paused, metadata) to the GUI.
- **Later stages:** a `compositor` / `glvideomixer` pipeline driven by the timeline
  model (N decoded sources → composite → preview sink; a separate encode branch for
  export). Performance with multiple HD layers is a known consideration to validate
  early in that stage.
- **Windows packaging:** bundle the GStreamer runtime into the MSI, **trimmed** to
  the needed plugins via merge modules / a silently-run runtime installer
  (documented by GStreamer). This is the main installer-size contributor.

## Data model

- Top-level `AppMode { Images, Videos }`; each mode holds its own working set.
- **Images:** the existing per-file model (paths + per-file crop + the shared
  conversion job/preset), now surfaced through the Viewer.
- **Videos (grows by stage):**
  - *Stage 1:* a list of opened clips + the currently selected clip; player state.
  - *Later:* a **Project / Timeline** model — an ordered stack of **Layers**, each
    holding one **Clip** (a media asset reference + in/out trim + start offset +
    transform + opacity + audio). The compositor and inspector read/write this.

## Staged build order

Each stage is independently shippable. Only Stage 1 is build-ready in this spec;
Stages 2–3 get their own spec → plan when their turn comes.

### Stage 1 — "Playback foundation" (the first 2.x release; what `writing-plans` will plan)

In scope:
- The shared **Viewer** component.
- **Image mode:** Viewer preview + **inline crop** (custom-dropdown controls in the
  viewer toolbar) replacing the old crop editor; all existing conversion features
  retained.
- The **Images / Videos toggle**, separate working set per mode, launch in Images,
  file-type routing from CLI/right-click.
- **`kuvatin-video` crate** with the single-clip **`playbin` Player**.
- **Video mode** UI: left media panel · Viewer playing a **single selected clip** ·
  full **transport** (play/pause, frame-step, scrubber, time, volume, fullscreen) ·
  the right **Inspector** column showing read-only **clip info** (resolution,
  duration, fps, codec, audio) — the *editable* per-layer inspector and the
  **Export** section arrive in Stages 2–3, because Stage 1 has nothing to edit yet ·
  a **single-layer** bottom strip (a clip tray; no layering/trim yet).
- GStreamer runtime **bundled in the MSI**; installer size validated.

Explicitly **not** in Stage 1: multi-layer timeline, compositing, image overlays,
per-layer trim/transform, and export/render.

### Stage 2 — "Compositor & timeline"

The layered timeline (one asset per layer, slide + edge-trim, top-layer overlays),
the real-time **composited** Viewer preview, the mixed media panel, and the
per-layer inspector (transform/opacity/volume/speed/trim). Introduces the
Project/Timeline data model and the `compositor` pipeline.

### Stage 3 — "Render / export"

Flatten the timeline to an output file (Export section: format/resolution/quality +
Render), via a GStreamer encode pipeline.

## Licensing

GStreamer core is LGPL; some plugins are GPL. Kuvatin is already **GPL-3.0-or-later**
(libimagequant), so bundling GPL GStreamer plugins is compatible. No license change.

## Testing

- **`kuvatin-video`**: unit/integration tests for the `Player` controller against
  short fixture clips — load, duration/metadata, seek accuracy, play/pause state,
  frame-step. This is the testable core of the media work.
- **`kuvatin-core`**: unchanged; existing tests keep passing.
- **GUI**: mode toggle and inline-crop logic exercised where feasible; manual
  verification for the playback surface (rendering is hard to assert in CI).

## Risks / open questions

- **GStreamer ↔ Slint frame path on Windows** (GL vs CPU buffers) — validate with a
  spike against Slint's `gstreamer-player` example before building the Stage 1 UI.
- **MSI bundling of the GStreamer runtime** — trimming plugins to keep size to the
  40–70 MB target; the CI Windows job must install the GStreamer dev/runtime.
- **Compositor performance** with multiple HD layers (Stage 2) — validate early.
- **Frame-accurate scrubbing** feel with `playbin` seeking — confirm in Stage 1.

## Out of scope (for the whole feature, unless raised later)

- macOS/Linux video (Kuvatin is Windows-only by choice).
- Audio editing/mixing beyond per-clip volume/mute.
- Transitions/effects/keyframing (could be a post-3.0 consideration).
