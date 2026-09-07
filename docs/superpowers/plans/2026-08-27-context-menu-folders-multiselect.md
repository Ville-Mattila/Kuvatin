# Explorer context menu: folders + multi-selection as one batch

> **Goal:** the right-click "Kuvatin" submenu works on folders, and selecting
> many files runs **one** conversion (one process, one parallel batch, one
> summary) instead of one process per file.

## Why it was broken

- The verb was registered only under `SystemFileAssociations\image` — folders
  never showed it.
- A classic static verb (`shell\…\command "%1"`) is invoked by Explorer **once
  per selected item**. Only COM handlers (`IDropTarget` / `IExecuteCommand`)
  receive the whole selection in one call. So 20 selected files meant 20
  processes, each converting one file (no cross-file parallelism) and each
  popping its own error box — and Explorer hides a verb entirely above 15 items
  unless it declares `MultiSelectModel`.

## Design

### Registry (`shell/windows.rs`)

- The cascading verb is attached in three places, all pointing at command
  stores via `ExtendedSubCommandsKey`:
  - `SystemFileAssociations\image\shell\Kuvatin` → `Kuvatin.CommandStore` (`%1`)
  - `Directory\shell\Kuvatin` → `Kuvatin.CommandStore` (`%1`)
  - `Directory\Background\shell\Kuvatin` → `Kuvatin.CommandStore.Background`
    (`%V` — a background verb has no `%1`, only the folder itself)
- `MultiSelectModel = Player` on every verb (parent and store items) lifts the
  15-item cap.
- A `Schema` sentinel (`"2"`) is written next to the existing `Icon` sentinel;
  `ensure_registered` requires both, so installs that already point at the
  current exe still pick up the new keys on next launch.

### Runtime rendezvous (`rendezvous.rs`)

Rather than ship a COM server, the N processes coordinate through the
filesystem under `%TEMP%\kuvatin\rendezvous\<group>` (group = `preset:<name>`
or `open`, so different presets never merge):

1. Every process spools its path(s) as one entry (temp name → atomic rename).
2. It races for `leader.lock` with `create_new`. Loser → **follower**, exits.
3. The **leader** waits until no new entry has landed for 600 ms (Explorer
   launches the burst quickly; the clock restarts per arrival).
4. It claims every entry by atomic rename into a private claim dir, releases
   the lock, then sweeps once more (an entry spooled just before the release
   saw the lock and left — it's ours; anything after belongs to the next
   leader). Rename atomicity guarantees no entry is claimed twice; a leader
   that ends up with zero paths exits quietly.
5. Locks/entries older than 30 s are debris from a crashed run: the lock is
   taken over, entries are deleted rather than batched.

`main.rs` runs the rendezvous for `--preset` quick-runs and for GUI launches
that carry paths ("Open in Kuvatin…"); a bare launch skips it. Folders need no
new handling — `collect_images` already expands a directory one level deep.

## Tests

- `rendezvous`: a staggered 8-thread burst yields exactly one non-empty leader
  holding every path once, spool drained, lock released; sequential runs are
  independent; groups are isolated; stale lock taken over / live lock
  respected; stale debris deleted; non-ASCII paths round-trip.
- `shell`: command-line composition for `%1` / `%V` and the GUI item.
- Manual: launch the staged build once (self-heals to schema 2), then
  right-click a folder, a 20+ file selection, and inside a folder's background.

## Addendum: "Render image sequence to MP4" (2026-09-07)

A fifth store item, `--sequence-mp4 "%1"` (schema 3). Right-click a numbered
frame — or a folder of frames, or a multi-selection of frames — and the run
becomes an H.264 MP4 next to the frames.

- `kuvatin-video::sequence::parse_frame_path` splits a name without touching
  the disk (`detect_sequence` = parse + forward scan); `SequenceSpec::
  same_sequence` matches frames of one run.
- `kuvatin-video::sequence::render_to_mp4(spec, out, fps)`: native frame size
  (read from the first frame, rounded to even for NV12) as the canvas AND the
  export size, ~0.12 bit/px/frame bitrate (4–40 Mbit/s), EXR converted first,
  blocking poll with a 60 s stall watchdog, partial file removed on failure.
  **Gotcha found by the test:** NVENC refuses tiny frames (floor ≈145×49) with
  an opaque "general stream error", and the encoder rank is locked at init so
  there is no software fallback — `render_size` upscales anything below
  160×96 (aspect kept) instead of failing.
- `kuvatin::sequence_render`: the selection resolves to DISTINCT runs — frames
  of one run merge and start from the lowest selected frame (so selecting all
  240 frames renders once, from the first); a folder contributes every run in
  it; lone frames and unnumbered files are reported. Output
  `<dir>/<prefix-trimmed>.mp4` (folder name for bare-number runs), never
  overwriting. Default 30 fps; `--fps` on the CLI.
- Coalesced through the rendezvous (group `sequence-mp4`) like the presets.

## Addendum: progress window for headless runs (2026-09-07)

`kuvatin::progress_ui::run_with_progress(heading, work)` runs `work` on a
worker thread while the Slint event loop owns a small always-on-top
`ProgressWindow` (heading · bar · status · Cancel):

- The window is only **shown after a 350 ms grace period** if the work is
  still running — an instant one-file job never flashes a dialog.
- The worker publishes `(fraction, status)` into a plain `Mutex` (`ProgressSink`
  is `Send + Sync` with no UI handles, so rayon workers can call it); a 50 ms UI
  timer mirrors the latest value into the window (coalesced).
- Cancel (button or close box) only **raises a flag** and shows "Cancelling…";
  the window stays until the work returns, so partial output is cleaned up.
  `kuvatin-core::batch::run_batch_until` stops starting new inputs (in-flight
  ones finish; the rest come back `CANCELLED`, not as failures);
  `render_to_mp4` polls the flag and tears the render down via `cancel_render`
  (partial MP4 deleted). Cancelled runs exit silently (no error dialog).
- A panicking worker still releases the event loop (`catch_unwind`) and
  surfaces as an error instead of a hung window.

## Follow-ups (not in scope)

- Folders are scanned one level deep (no recursion), as before.
