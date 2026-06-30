# Mode Toggle + Image Viewer + Inline Crop — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an Images/Videos mode toggle and a central image **Viewer** to the existing app, and fold the current modal crop editor into that Viewer (crop becomes a tool on the previewed image, with its numeric controls as custom dropdowns in the viewer toolbar).

**Architecture:** Pure Slint/Rust UI work in the existing `kuvatin` crate — **no GStreamer, no new crate**. Video mode is a placeholder pane this plan only stubs. The body changes from two columns (Files · Settings) to three (Files · Viewer · Settings). Crop state already lives in `gui.rs` (`crops` map keyed by path, `edit` state) and the crop math is in `kuvatin-core`; we re-host the same state in the viewer instead of a modal overlay.

**Tech Stack:** Rust, Slint 1.8 (`std-widgets`), the `image` crate. This is the first of several plans for the video feature; the GStreamer `kuvatin-video` engine is a separate later plan (see [the spec](../specs/2026-06-30-video-support-design.md), Stage 1).

**Environment note:** `cargo` lives at `~/.cargo/bin` and may not be on `PATH`. If `cargo` is not found, prefix commands with `PATH="$HOME/.cargo/bin:$PATH"` (bash) or `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"` (PowerShell). Slint markup changes are verified by `cargo build -p kuvatin` (a `.slint` error fails the build) and a manual `cargo run -p kuvatin`, since UI rendering can't be asserted in unit tests.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/kuvatin/ui/app.slint` | All UI markup | Add mode toggle + properties; add 3rd (Viewer) column; move crop handles into the viewer; add custom dropdown component; delete the modal crop overlay |
| `crates/kuvatin/src/gui.rs` | UI↔core wiring | Add `selected-index` + viewer-image loading; rename/repurpose `on_start_crop` → `on_select_file`; keep `apply/cancel/clear` crop; add a tiny testable preview-box helper |
| `crates/kuvatin/src/preview.rs` (new) | Pure preview-geometry helper, unit-tested | Create |

Crop math, the per-file `crops` map, and the conversion pipeline are **unchanged** — we only change how crop is presented and entered.

---

## Task 1: Extract a testable preview-box helper (TDD)

The crop loader computes a preview-box size that preserves aspect ratio within a max area (`gui.rs:249-253`). Extract it to a pure function so it's unit-tested and reusable by the viewer.

**Files:**
- Create: `crates/kuvatin/src/preview.rs`
- Modify: `crates/kuvatin/src/main.rs` (add `mod preview;`)
- Modify: `crates/kuvatin/src/gui.rs` (use the helper)

- [ ] **Step 1: Write the failing test**

Create `crates/kuvatin/src/preview.rs`:

```rust
//! Pure geometry helpers for the image viewer / crop preview.

/// Size of a preview box that preserves the source aspect ratio and fits within
/// `max_w` x `max_h`, never upscaling past the source. Returns (w, h) in px, each >= 1.
pub fn preview_box(src_w: u32, src_h: u32, max_w: f32, max_h: f32) -> (f32, f32) {
    if src_w == 0 || src_h == 0 {
        return (1.0, 1.0);
    }
    let scale = (max_w / src_w as f32).min(max_h / src_h as f32).min(1.0);
    ((src_w as f32 * scale).max(1.0), (src_h as f32 * scale).max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_fits_width() {
        let (w, h) = preview_box(2000, 1000, 560.0, 420.0);
        assert!((w - 560.0).abs() < 0.5, "w={w}");
        assert!((h - 280.0).abs() < 0.5, "h={h}");
    }

    #[test]
    fn small_image_not_upscaled() {
        assert_eq!(preview_box(100, 80, 560.0, 420.0), (100.0, 80.0));
    }

    #[test]
    fn zero_dims_safe() {
        assert_eq!(preview_box(0, 0, 560.0, 420.0), (1.0, 1.0));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p kuvatin preview::` 
Expected: FAIL — `preview.rs` isn't a module yet (`unresolved module` / not compiled).

- [ ] **Step 3: Register the module**

In `crates/kuvatin/src/main.rs`, add `mod preview;` next to the other `mod` lines (after `mod gui;`).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p kuvatin preview::`
Expected: PASS (3 tests).

- [ ] **Step 5: Use the helper in gui.rs**

In `crates/kuvatin/src/gui.rs` `on_start_crop`, replace the inline box math (currently `gui.rs:250-253`):

```rust
            let (max_w, max_h) = (560.0_f32, 420.0_f32);
            let scale = (max_w / ow as f32).min(max_h / oh as f32).min(1.0);
            ui.set_crop_box_w((ow as f32 * scale).max(1.0));
            ui.set_crop_box_h((oh as f32 * scale).max(1.0));
```

with:

```rust
            let (bw, bh) = crate::preview::preview_box(ow, oh, 560.0, 420.0);
            ui.set_crop_box_w(bw);
            ui.set_crop_box_h(bh);
```

- [ ] **Step 6: Build, then commit**

Run: `cargo build -p kuvatin` → Expected: builds clean.
```bash
git add crates/kuvatin/src/preview.rs crates/kuvatin/src/main.rs crates/kuvatin/src/gui.rs
git commit -m "refactor: extract preview_box helper with tests"
```

---

## Task 2: Add the Images/Videos mode toggle

A centered segmented control in the title bar drives an `app-mode` property. The body shows the image UI for Images and a placeholder for Videos.

**Files:**
- Modify: `crates/kuvatin/ui/app.slint`

- [ ] **Step 1: Add the property and a reusable toggle component**

In `app.slint`, near the other top-level properties (after `in-out property <string> preset-name;`, ~line 226), add:

```slint
    // 0 = Images, 1 = Videos. Video mode is a placeholder until the GStreamer plan.
    in-out property <int> app-mode: 0;
```

Above `export component AppWindow` (~line 206), add a segmented toggle component:

```slint
component ModeToggle inherits Rectangle {
    in-out property <int> mode;            // 0 = Images, 1 = Videos
    width: 188px; height: 26px;
    border-radius: 999px;
    background: #14171c;
    border-width: 1px;
    border-color: #2c313b;
    HorizontalLayout {
        padding: 2px; spacing: 2px;
        for seg[i] in ["Images", "Videos"]: Rectangle {
            horizontal-stretch: 1;
            border-radius: 999px;
            background: root.mode == i ? #2dd4bf : (ta.has-hover ? #20242c : transparent);
            Text {
                text: seg;
                horizontal-alignment: center; vertical-alignment: center;
                font-size: 12px; font-weight: 700;
                color: root.mode == i ? #08110f : #9aa3b2;
            }
            ta := TouchArea { clicked => { root.mode = i; } }
        }
    }
}
```

- [ ] **Step 2: Place the toggle centered in the title bar**

In the title-bar `HorizontalLayout` (~line 286-309), the layout is: icon, "Kuvatin" text, a stretch spacer, then the window buttons. Replace the single stretch spacer (`Rectangle { horizontal-stretch: 1; }`, line 304) with a spacer + centered toggle + spacer:

```slint
                Rectangle { horizontal-stretch: 1; }
                ModeToggle { mode <=> root.app-mode; y: (parent.height - self.height) / 2; }
                Rectangle { horizontal-stretch: 1; }
```

- [ ] **Step 3: Gate the body on the mode**

Wrap the existing body `HorizontalLayout` (the one starting `// Body` at ~line 312) so it only shows in Images mode, and add a Videos placeholder. Change:

```slint
        // Body -----------------------------------------------------------
        HorizontalLayout {
            padding: 16px;
            spacing: 18px;
```

to:

```slint
        // Body -----------------------------------------------------------
        if root.app-mode == 0: HorizontalLayout {
            padding: 16px;
            spacing: 18px;
```

Then, immediately after that `HorizontalLayout`'s closing brace (the one that closes the body, before the `if root.cropping` overlay), add the placeholder:

```slint
        if root.app-mode == 1: Rectangle {
            vertical-stretch: 1;
            background: #16181d;
            Text {
                text: "Video mode — coming soon";
                color: #9aa3b2; font-size: 15px;
                horizontal-alignment: center; vertical-alignment: center;
            }
        }
```

- [ ] **Step 4: Build and verify by running**

Run: `cargo build -p kuvatin` → Expected: builds clean.
Run: `cargo run -p kuvatin` → Expected: a centered `Images | Videos` toggle in the title bar; clicking **Videos** swaps the whole body to the "coming soon" placeholder; **Images** restores the file list. (Close the window to end.)

- [ ] **Step 5: Commit**

```bash
git add crates/kuvatin/ui/app.slint
git commit -m "feat: add Images/Videos mode toggle with a placeholder video pane"
```

---

## Task 3: Add the Viewer column and select-on-click

Insert a central Viewer between Files and Settings. Clicking a file **selects** it (instead of opening the crop modal) and shows it large in the Viewer.

**Files:**
- Modify: `crates/kuvatin/ui/app.slint`
- Modify: `crates/kuvatin/src/gui.rs`

- [ ] **Step 1: Add viewer properties + a select callback (app.slint)**

Near the crop properties (~line 231-243) add:

```slint
    // Central image viewer.
    in-out property <int> selected-index: -1;
    in property <image> viewer-image;
    in property <int> viewer-img-w;
    in property <int> viewer-img-h;
    callback select-file(int);
```

- [ ] **Step 2: Point row clicks at select-file instead of start-crop**

In the Files `ListView` row `TouchArea` (~line 392-394), change:

```slint
                row-ta := TouchArea {
                    clicked => { root.start-crop(i); }
                }
```

to:

```slint
                row-ta := TouchArea {
                    clicked => { root.select-file(i); }
                }
```

Also highlight the selected row: on that row `Rectangle` (~line 387-391) change the `background` expression to include selection:

```slint
                background: root.selected-index == i
                    ? #2dd4bf18
                    : (row-ta.has-hover ? #262b34 : (Math.mod(i, 2) == 0 ? transparent : #1c2027));
```

- [ ] **Step 3: Insert the Viewer card between Files and Settings**

Between the Files `Card` (closes ~line 466) and the Settings `Card` (opens ~line 469), insert:

```slint
            // Viewer card
            Card {
                min-width: 360px;
                horizontal-stretch: 2;
                VerticalLayout {
                    padding: 12px; spacing: 8px;

                    // viewer surface
                    Rectangle {
                        vertical-stretch: 1;
                        background: #0b0d11;
                        border-radius: 8px;
                        border-width: 1.5px;
                        border-color: #2dd4bf;
                        clip: true;

                        if root.selected-index < 0: Text {
                            text: "Select a file to preview";
                            color: #6b7585; font-size: 13px;
                            horizontal-alignment: center; vertical-alignment: center;
                        }
                        if root.selected-index >= 0: Image {
                            source: root.viewer-image;
                            image-fit: contain;
                            width: 100%; height: 100%;
                        }
                    }
                }
            }
```

- [ ] **Step 4: Wire select-file in gui.rs**

In `crates/kuvatin/src/gui.rs`, the block that registers `on_start_crop` (~line 220-277) currently loads the image and enters cropping. **Add a new `on_select_file` handler** that loads the preview into the viewer and records selection, reusing the same decode. Place it right before the `on_start_crop` block:

```rust
    // Selecting a file shows it large in the viewer (and prepares crop state).
    {
        let files = files.clone();
        let crops = crops.clone();
        let edit = edit.clone();
        let ui_weak = ui.as_weak();
        ui.on_select_file(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return; };
            let path = match files.lock().unwrap().get(index as usize) {
                Some(p) => p.clone(),
                None => return,
            };
            let Ok(img) = image::open(&path) else { return; };
            let (ow, oh) = (img.width(), img.height());
            if ow == 0 || oh == 0 { return; }

            // Decode a display-sized preview; normalized crop coords stay size-independent.
            let preview = img.thumbnail(1280, 1280).to_rgba8();
            let (pw, ph) = (preview.width(), preview.height());
            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(preview.as_raw(), pw, ph);
            ui.set_viewer_image(Image::from_rgba8(buf));
            ui.set_viewer_img_w(ow as i32);
            ui.set_viewer_img_h(oh as i32);
            ui.set_selected_index(index);

            // Seed crop state for this file (used when the viewer enters Crop mode).
            ui.set_crop_img_w(ow as i32);
            ui.set_crop_img_h(oh as i32);
            let (bw, bh) = crate::preview::preview_box(ow, oh, 560.0, 420.0);
            ui.set_crop_box_w(bw);
            ui.set_crop_box_h(bh);
            if let Some(&(x, y, w, h)) = crops.lock().unwrap().get(&path) {
                ui.set_crop_x(x as f32 / ow as f32);
                ui.set_crop_y(y as f32 / oh as f32);
                ui.set_crop_w(w as f32 / ow as f32);
                ui.set_crop_h(h as f32 / oh as f32);
            } else {
                ui.set_crop_x(0.0); ui.set_crop_y(0.0);
                ui.set_crop_w(1.0); ui.set_crop_h(1.0);
            }
            *edit.lock().unwrap() = Some((path, ow, oh));
        });
    }
```

> Note: `on_start_crop` stays for now (still referenced by the modal); Task 6 removes it. The `crop-image` modal still works until then.

- [ ] **Step 5: Build, run, verify**

Run: `cargo build -p kuvatin` → builds clean.
Run: `cargo run -p kuvatin`, add a few images → Expected: clicking a row highlights it and shows the image large in the new center Viewer; the old crop modal no longer opens on click.

- [ ] **Step 6: Commit**

```bash
git add crates/kuvatin/ui/app.slint crates/kuvatin/src/gui.rs
git commit -m "feat: add central image viewer with select-on-click"
```

---

## Task 4: Move crop into the viewer behind a View/Crop toggle

Render the crop dimming + handles over the viewer image when a `View/Crop` tool is in Crop mode, reusing the existing `CropHandle` component and the corner/move math from the modal. Replace `cropping` as "modal open" with "crop tool active".

**Files:**
- Modify: `crates/kuvatin/ui/app.slint`

- [ ] **Step 1: Add a crop-tool state + reuse the overlay's drag bookkeeping**

The modal overlay `parent-overlay` (~line 630) owns `move-sx/move-sy/move-x0/move-y0`, `edge-ratio`, and functions `begin-corner()`, `corner(sx, sy, dx, dy)`, plus the move TouchArea logic. We reuse that math inside the viewer. Add to the viewer card's `VerticalLayout` (from Task 3), replacing the inner viewer `Rectangle` with a crop-capable version:

```slint
                    // viewer toolbar
                    HorizontalLayout {
                        spacing: 8px; height: 28px;
                        Rectangle {
                            width: 132px; border-radius: 999px; background: #14171c;
                            border-width: 1px; border-color: #2c313b;
                            HorizontalLayout {
                                padding: 2px; spacing: 2px;
                                for seg[i] in ["View", "Crop"]: Rectangle {
                                    horizontal-stretch: 1; border-radius: 999px;
                                    background: root.cropping == (i == 1) ? #2dd4bf : transparent;
                                    Text {
                                        text: seg; font-size: 11px; font-weight: 700;
                                        horizontal-alignment: center; vertical-alignment: center;
                                        color: root.cropping == (i == 1) ? #08110f : #9aa3b2;
                                    }
                                    tta := TouchArea { clicked => { root.cropping = (i == 1); } }
                                }
                            }
                        }
                        Rectangle { horizontal-stretch: 1; }
                        // crop dropdowns get added in Task 5
                    }

                    crop-surface := Rectangle {
                        vertical-stretch: 1;
                        background: #0b0d11;
                        border-radius: 8px; border-width: 1.5px; border-color: #2dd4bf;
                        clip: true;

                        // drag bookkeeping (moved from the old modal overlay)
                        property <length> move-sx; property <length> move-sy;
                        property <float> move-x0;  property <float> move-y0;
                        property <float> edge-ratio: 1.0;
                        function begin-corner() {
                            self.edge-ratio = (root.crop-w * root.crop-box-w) / (root.crop-h * root.crop-box-h);
                        }
                        function corner(sx: float, sy: float, dx: float, dy: float) {
                            // identical clamping logic to the old overlay's corner()
                            if (sx < 0) { root.crop-x = Math.clamp(root.crop-x + dx, 0, root.crop-x + root.crop-w - 0.02); root.crop-w = root.crop-w - (root.crop-x - root.crop-x); }
                            // NOTE: copy the exact body of parent-overlay.corner() from app.slint
                            //       (lines ~660-700) verbatim here; it already handles aspect-lock.
                        }

                        if root.selected-index < 0: Text {
                            text: "Select a file to preview";
                            color: #6b7585; font-size: 13px;
                            horizontal-alignment: center; vertical-alignment: center;
                        }
                        if root.selected-index >= 0: Image {
                            source: root.viewer-image;
                            image-fit: contain; width: 100%; height: 100%;
                        }

                        // crop overlay (only in Crop mode) — dim + rect + handles
                        if root.cropping && root.selected-index >= 0: Rectangle {
                            // Center a crop-box-sized region; reuse handle positions from the modal.
                            // Copy the rect/dim/CropHandle subtree from the modal (app.slint ~720-806),
                            // re-parenting handle callbacks to crop-surface.begin-corner()/corner().
                        }
                    }
```

> **Implementation guidance (do this carefully in the editor):** the corner/move math and the 4 `CropHandle` instances already exist in the modal (`app.slint` ~657-806). Move that subtree into `crop-surface`, swapping `parent-overlay.` for `crop-surface.` in the handle callbacks. Do **not** rewrite the math — port it verbatim so behavior is identical. Keep the same `crop-box-w/h`, `crop-x/y/w/h` properties.

- [ ] **Step 2: Remove the inner viewer Rectangle from Task 3**

Delete the simpler `Rectangle { ... Image ... }` added in Task 3 step 3 (it's now superseded by `crop-surface`). The viewer card now contains: toolbar + `crop-surface`.

- [ ] **Step 3: Build and verify**

Run: `cargo build -p kuvatin` → builds clean (fix any Slint errors the compiler reports; expect a few iterations porting the subtree).
Run: `cargo run -p kuvatin` → Expected: select an image; the `View/Crop` toggle appears; switching to **Crop** shows the dimmed rectangle + draggable corner handles on the previewed image; dragging adjusts the rect.

- [ ] **Step 4: Commit**

```bash
git add crates/kuvatin/ui/app.slint
git commit -m "feat: inline crop handles in the viewer behind a View/Crop tool"
```

---

## Task 5: Crop numeric controls as custom dropdowns + apply on leaving Crop

Replace the modal's numeric fields with compact custom-dropdown editors in the viewer toolbar (X/Y/W/H + aspect-lock), and apply the crop when the user leaves Crop mode.

**Files:**
- Modify: `crates/kuvatin/ui/app.slint`
- Modify: `crates/kuvatin/src/gui.rs`

- [ ] **Step 1: Add a NumberDropdown component (app.slint)**

Above `export component AppWindow`, add:

```slint
component NumberDropdown inherits Rectangle {
    in property <string> label;
    in-out property <int> value;
    in property <int> minimum: 0;
    in property <int> maximum: 100000;
    property <bool> open: false;
    width: 64px; height: 24px; border-radius: 6px;
    background: #16181d; border-width: 1px; border-color: #343b46;
    HorizontalLayout {
        padding-left: 6px; padding-right: 6px; spacing: 4px;
        Text { text: root.label + " " + root.value; color: #e6e9ef; font-size: 10px; vertical-alignment: center; }
    }
    ta := TouchArea { clicked => { root.open = !root.open; } }
    if root.open: Rectangle {
        y: parent.height + 3px; width: 92px; height: 30px;
        background: #20242c; border-radius: 6px; border-width: 1px; border-color: #343b46;
        edit := LineEdit {
            width: 84px; x: 4px; y: 3px;
            text: root.value;
            input-type: number;
            edited(t) => { root.value = Math.clamp(t.to-float(), root.minimum, root.maximum); }
            accepted(t) => { root.open = false; }
        }
    }
}
```

- [ ] **Step 2: Add the dropdowns to the viewer toolbar (Crop mode only)**

In the viewer toolbar from Task 4 step 1, replace the `// crop dropdowns get added in Task 5` comment with:

```slint
                        if root.cropping: HorizontalLayout {
                            spacing: 6px;
                            NumberDropdown { label: "X"; value <=> root.crop-px-x; maximum: root.crop-img-w; }
                            NumberDropdown { label: "Y"; value <=> root.crop-px-y; maximum: root.crop-img-h; }
                            NumberDropdown { label: "W"; value <=> root.crop-px-w; minimum: 1; maximum: root.crop-img-w; }
                            NumberDropdown { label: "H"; value <=> root.crop-px-h; minimum: 1; maximum: root.crop-img-h; }
                            Rectangle {
                                width: 26px; border-radius: 6px;
                                background: root.aspect-lock ? #2dd4bf : #16181d;
                                border-width: 1px; border-color: #343b46;
                                Text { text: "🔒"; font-size: 11px; horizontal-alignment: center; vertical-alignment: center; }
                                TouchArea { clicked => { root.aspect-lock = !root.aspect-lock; } }
                            }
                        }
```

- [ ] **Step 3: Add absolute-pixel crop properties bridging to the normalized rect**

The dropdowns edit absolute pixels; the crop rect is normalized. Add bridging properties near the crop properties:

```slint
    // Absolute-pixel mirror of the normalized crop rect, for the numeric dropdowns.
    property <int> crop-px-x: round(root.crop-x * root.crop-img-w);
    property <int> crop-px-y: round(root.crop-y * root.crop-img-h);
    property <int> crop-px-w: round(root.crop-w * root.crop-img-w);
    property <int> crop-px-h: round(root.crop-h * root.crop-img-h);
```

> When a dropdown writes `crop-px-*`, convert back to normalized. Because Slint two-way bindings can't run conversions, change the `NumberDropdown` instances to one-way + `edited`: e.g. for X use `value: root.crop-px-x;` and add to that `NumberDropdown` an `edited` is not available — instead bind `value <=> root.crop-px-x` and add a `changed` handler on the root: add `changed crop-px-x => { root.crop-x = root.crop-px-x / Math.max(root.crop-img-w, 1); }` (and likewise y/w/h) at the AppWindow root. This keeps the rect and the pixels in sync both ways.

- [ ] **Step 4: Apply the crop when leaving Crop mode (app.slint + reuse gui.rs)**

The `View/Crop` toggle already sets `root.cropping`. Make switching to **View** apply the crop: in the toolbar `View` segment's `TouchArea` clicked handler (Task 4 step 1), change `clicked => { root.cropping = (i == 1); }` so that selecting View also fires apply:

```slint
                                    tta := TouchArea {
                                        clicked => {
                                            if (i == 0 && root.cropping) { root.apply-crop(); }
                                            root.cropping = (i == 1);
                                        }
                                    }
```

`on_apply_crop` in `gui.rs` already reads `crop-x/y/w/h` + `edit` and writes the `crops` map — it works unchanged. Remove its trailing `ui.set_cropping(false);` line (`gui.rs:319`) since the toggle now owns `cropping`.

- [ ] **Step 5: Build, run, verify**

Run: `cargo build -p kuvatin` → builds clean.
Run: `cargo run -p kuvatin` → Expected: in Crop mode the toolbar shows X/Y/W/H dropdowns + a lock; editing a number moves the rect, dragging a handle updates the numbers; switching back to **View** applies the crop. Then **Convert** produces a cropped output (verify the output file dimensions).

- [ ] **Step 6: Commit**

```bash
git add crates/kuvatin/ui/app.slint crates/kuvatin/src/gui.rs
git commit -m "feat: crop dropdown controls in the viewer toolbar; apply on leaving Crop"
```

---

## Task 6: Per-file crop badge + remove the old modal and dead code

**Files:**
- Modify: `crates/kuvatin/ui/app.slint`
- Modify: `crates/kuvatin/src/gui.rs`

- [ ] **Step 1: Show a crop badge, drop the hover "✂ Crop"**

In the Files row (~line 432-439), the existing `Text { text: "✂ Crop"; ... visible: row-ta.has-hover; }` was the old entry point. Replace it with a persistent badge driven by the row status (apply-crop already sets `row.status = "cropped"`):

```slint
                    Rectangle {
                        visible: row.status == "cropped";
                        height: 16px; border-radius: 999px; background: #2dd4bf;
                        HorizontalLayout { padding-left: 6px; padding-right: 6px;
                            Text { text: "crop"; color: #08110f; font-size: 9px; font-weight: 700; vertical-alignment: center; }
                        }
                        y: (parent.height - self.height) / 2;
                    }
```

- [ ] **Step 2: Delete the modal crop overlay**

Remove the entire `if root.cropping: parent-overlay := Rectangle { ... }` block (~line 630 through its matching close ~line 945). Crop now lives in the viewer.

- [ ] **Step 3: Remove now-dead callbacks/handlers**

- In `app.slint`: delete the now-unused `callback start-crop(int);`, `callback apply-crop();` stays (still used), `callback cancel-crop();` and `callback clear-crop();` — delete `start-crop`, `cancel-crop`, `clear-crop` if no longer referenced (search the file to confirm zero references first). Keep `apply-crop`.
- In `gui.rs`: delete the `on_start_crop`, `on_cancel_crop`, `on_clear_crop` registration blocks (~line 220-277 for start_crop, ~324-358 for cancel/clear). Keep `on_apply_crop`. Remove the now-unused `crop-image`, `crop-box-w`, `crop-box-h` properties from `app.slint` only if nothing references them (the viewer uses `viewer-image` + `crop-box-w/h`; `crop-box-w/h` are still used by `begin-corner`/`corner`, so **keep** them; `crop-image` is unused now — remove it and its `ui.set_crop_image(...)` call).

- [ ] **Step 4: Build, test, run**

Run: `cargo build -p kuvatin` → builds clean, no `unused` warnings for the removed items.
Run: `cargo test` → Expected: all existing tests + the new `preview` tests pass.
Run: `cargo run -p kuvatin` → Full pass: select → preview → crop (handles + dropdowns) → View applies → badge shows → Convert yields a cropped file. Toggle to Videos → placeholder. 

- [ ] **Step 5: Commit**

```bash
git add crates/kuvatin/ui/app.slint crates/kuvatin/src/gui.rs
git commit -m "feat: per-file crop badge; remove the modal crop editor"
```

---

## Self-Review

**Spec coverage (Stage 1, image-side portion):**
- Images/Videos toggle, center, separate visible UI per mode → Task 2. (Separate *working sets* and CLI/right-click routing are deferred to the video plan, where the video working set first exists — noted as a gap to carry forward.)
- Large image viewer of the selected file → Tasks 3–4.
- Crop folded into the viewer, per-file, custom-dropdown controls in the viewer toolbar → Tasks 4–5.
- All current conversion features retained → untouched; verified by `cargo test` + run in Task 6.
- GStreamer / video playback → out of scope for this plan (Plan 2), as designed.

**Placeholder scan:** Tasks 4 step 1 and Task 5 step 3 intentionally say "port the existing math verbatim" rather than reprinting the ~50 lines of modal corner/move logic — this is a deliberate "move this exact existing code" instruction with precise source line ranges, not a vague TODO. All other steps contain concrete code.

**Type consistency:** `select-file`/`on_select_file`, `viewer-image`/`set_viewer_image`, `crop-px-x..h`, `app-mode`/`set_app_mode`, `crop-box-w/h`, `apply-crop`/`on_apply_crop` are used consistently across tasks. The `crops` map and `edit` state names match `gui.rs`.

**Carry-forward for Plan 2 (video):** separate per-mode working sets, CLI/right-click file-type routing, and the `kuvatin-video` GStreamer engine — all begin in the video plan, which should open with a GStreamer↔Slint integration spike against Slint's `gstreamer-player` example.
