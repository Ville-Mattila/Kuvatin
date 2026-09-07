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

## Follow-ups (not in scope)

- A headless quick-run has no progress UI; a folder of hundreds of images runs
  silently until done. A small progress window would help.
- Folders are scanned one level deep (no recursion), as before.
