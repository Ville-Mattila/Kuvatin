//! Export: the settings dialog's "Export…" → deferred `begin_render`, the
//! progress modal (with a stall watchdog) and Cancel.

use super::VideoState;
use crate::gui::{show_error, show_info_at, AppWindow};
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

/// The running (or about-to-start) export.
#[derive(Default)]
pub(crate) struct ExportState {
    /// True while an export/render is running (pauses the preview timer, whose
    /// seeks/commits would corrupt the render).
    pub(super) active: Rc<Cell<bool>>,
    /// True between "Export…" and the deferred `begin_render` (one tick
    /// later, so the modal paints before the engine blocks the UI thread).
    pub(super) pending: Rc<Cell<bool>>,
    /// The output path of the running export, so Cancel/failure can delete
    /// the partial file.
    pub(super) path: Rc<RefCell<Option<PathBuf>>>,
    /// When the render actually started, for the elapsed/remaining line and
    /// the "took 2:14" in the finished dialog.
    pub(super) started: Rc<Cell<Option<std::time::Instant>>>,
}

/// "1:15", "59:59", "1:01:01" — no leading zero on the first field, so short
/// renders read as the seconds they are.
fn clock(d: std::time::Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The line under the export progress bar. A long software render used to say
/// only "42%", which tells nobody whether to wait or go and make coffee.
///
/// The estimate is linear — remaining = elapsed * (1 - f) / f — which is
/// honest enough for a render that processes frames at a steady rate, and it
/// is hedged with "about". It is withheld for the first few seconds, when the
/// rate is dominated by the pipeline starting up, and at the finish, where
/// "about 0:00 left" reads like a stall.
fn export_status_line(fraction: f32, elapsed: std::time::Duration) -> String {
    let pct = format!("{:.0}%", (fraction.clamp(0.0, 1.0)) * 100.0);
    if elapsed < std::time::Duration::from_secs(3) || fraction <= 0.0 {
        return pct;
    }
    let line = format!("{pct} \u{b7} {} elapsed", clock(elapsed));
    if fraction >= 1.0 {
        return line;
    }
    let left = elapsed.mul_f32((1.0 - fraction) / fraction);
    format!("{line} \u{b7} about {} left", clock(left))
}

/// Wire the export callbacks and the progress timer.
pub(super) fn wire(
    ui: &AppWindow,
    st: &VideoState,
    ex: &ExportState,
    timers: &mut Vec<slint::Timer>,
) {
    let ui_weak = ui.as_weak();
    let project_slot = &st.project;
    let export_active = &ex.active;
    let export_pending = &ex.pending;
    let export_path = &ex.path;
    let export_started = &ex.started;
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let export_active = export_active.clone();
        let export_pending = export_pending.clone();
        let export_path = export_path.clone();
        let export_started = export_started.clone();
        ui.on_video_export(move || {
            if export_active.get() || export_pending.get() {
                return;
            }
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            if project_slot.borrow().is_none() {
                show_error(
                    &ui,
                    "Nothing to export",
                    "Add a clip to the timeline first.",
                );
                return;
            }
            // Codec index → codec + container extension (chosen in the dialog).
            let (codec, ext, default_name) = match ui.get_export_codec() {
                1 => (kuvatin_video::VideoCodec::Vp9, "webm", "export.webm"),
                2 => (kuvatin_video::VideoCodec::Vp8, "webm", "export.webm"),
                _ => (kuvatin_video::VideoCodec::H264, "mp4", "export.mp4"),
            };
            let settings = kuvatin_video::ExportSettings {
                codec,
                width: ui.get_export_w(),
                height: ui.get_export_h(),
                fps: ui.get_export_fps().clamp(1, 240) as u32,
                bitrate_kbps: ui.get_export_bitrate().max(0) as u32,
            };
            let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    if ext == "mp4" {
                        "MP4 video"
                    } else {
                        "WebM video"
                    },
                    &[ext],
                )
                .set_file_name(default_name)
                .save_file()
            else {
                return;
            };
            // Show the modal NOW and start the render on the next tick:
            // begin_render tears the preview down and waits for NULL (up
            // to 3 s) on this thread, which used to freeze the window with
            // no feedback before anything appeared.
            *export_path.borrow_mut() = Some(path.clone());
            export_pending.set(true);
            ui.set_exporting(true);
            ui.set_export_progress(0.0);
            ui.set_export_status("Starting\u{2026}".into());
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let export_active = export_active.clone();
            let export_pending = export_pending.clone();
            let export_path = export_path.clone();
            let export_started = export_started.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(60), move || {
                if !export_pending.get() {
                    return; // cancelled before it started
                }
                export_pending.set(false);
                let result = project_slot
                    .borrow()
                    .as_ref()
                    .map(|p| p.begin_render(&path, settings));
                match result {
                    Some(Ok(())) => {
                        export_started.set(Some(std::time::Instant::now()));
                        export_active.set(true);
                    }
                    Some(Err(e)) => {
                        let _ = std::fs::remove_file(&path);
                        export_path.borrow_mut().take();
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_exporting(false);
                            show_error(&ui, "Export failed to start", e.to_string());
                        }
                    }
                    None => {
                        export_path.borrow_mut().take();
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_exporting(false);
                        }
                    }
                }
            });
        });
    }

    // Cancel a running export: tear the render down and delete the partial
    // file (no EOS wait — the file is discarded anyway).
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let export_active = export_active.clone();
        let export_pending = export_pending.clone();
        let export_path = export_path.clone();
        let export_started = export_started.clone();
        ui.on_export_cancel(move || {
            if export_pending.get() {
                // Not started yet: the deferred start sees the flag and bails.
                export_pending.set(false);
                export_path.borrow_mut().take();
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_exporting(false);
                }
                return;
            }
            if !export_active.get() {
                return;
            }
            let path = export_path.borrow_mut().take();
            export_started.take();
            if let (Some(p), Some(path)) = (project_slot.borrow().as_ref(), path) {
                let _ = p.cancel_render(&path, true);
            }
            export_active.set(false);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_exporting(false);
            }
        });
    }

    // Export progress: poll the render; finish or fail restores the preview.
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let export_active = export_active.clone();
        let export_path = export_path.clone();
        let export_started = export_started.clone();
        // Watchdog: if progress hasn't advanced for this many ticks (200ms
        // each → 20s), tell the user the render looks stuck (Cancel is right
        // there). Some pipeline stalls never post EOS/Error. The baseline
        // is the last fraction that actually ADVANCED — comparing against
        // the previous tick called a slow-but-healthy render stuck.
        let stall = Rc::new(std::cell::Cell::new((0.0f32, 0u32)));
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(200),
            move || {
                if !export_active.get() {
                    return;
                }
                let slot = project_slot.borrow();
                let Some(p) = slot.as_ref() else {
                    return;
                };
                match p.render_status() {
                    kuvatin_video::RenderStatus::Rendering(f) => {
                        let (last, ticks) = stall.get();
                        let (last, ticks) = if f > last { (f, 0) } else { (last, ticks + 1) };
                        stall.set((last, ticks));
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_export_progress(f);
                            ui.set_export_status(if ticks >= 100 {
                                "Export appears stuck — you can cancel.".into()
                            } else {
                                let elapsed = export_started
                                    .get()
                                    .map(|t| t.elapsed())
                                    .unwrap_or_default();
                                export_status_line(f, elapsed).into()
                            });
                        }
                    }
                    kuvatin_video::RenderStatus::Done => {
                        // A preview that fails to come back is otherwise a
                        // silently black viewer for the rest of the session.
                        if let Err(e) = p.end_render() {
                            if let Some(ui) = ui_weak.upgrade() {
                                show_error(&ui, "Preview could not be restored", e.to_string());
                            }
                        }
                        drop(slot);
                        export_active.set(false);
                        let done = export_path.borrow_mut().take();
                        let took = export_started.take().map(|t| t.elapsed());
                        stall.set((0.0, 0));
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_exporting(false);
                            // The dialog used to just vanish, leaving no sign
                            // that anything had been written, or where.
                            if let Some(path) = done {
                                let mut detail = path.display().to_string();
                                if let Some(t) = took {
                                    detail.push_str(&format!(
                                        "

Took {}",
                                        clock(t)
                                    ));
                                }
                                show_info_at(&ui, "Export finished", detail, &path);
                            }
                        }
                    }
                    kuvatin_video::RenderStatus::Failed(e) => {
                        if let Err(restore) = p.end_render() {
                            if let Some(ui) = ui_weak.upgrade() {
                                show_error(
                                    &ui,
                                    "Preview could not be restored",
                                    restore.to_string(),
                                );
                            }
                        }
                        drop(slot);
                        export_active.set(false);
                        export_started.take();
                        stall.set((0.0, 0));
                        // Delete the truncated output and show the real reason.
                        if let Some(path) = export_path.borrow_mut().take() {
                            let _ = std::fs::remove_file(path);
                        }
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_exporting(false);
                            show_error(&ui, "Export failed", e);
                        }
                    }
                }
            },
        );
        timers.push(timer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// An estimate from the first second of a render is noise; a software
    /// H.264 pass can spend that long on its first frame.
    #[test]
    fn no_estimate_until_there_is_something_to_estimate_from() {
        assert_eq!(export_status_line(0.0, secs(0)), "0%");
        assert_eq!(export_status_line(0.4, secs(2)), "40%");
    }

    #[test]
    fn elapsed_and_remaining_once_the_render_is_under_way() {
        // A quarter done in thirty seconds: ninety more to go.
        assert_eq!(
            export_status_line(0.25, secs(30)),
            "25% \u{b7} 0:30 elapsed \u{b7} about 1:30 left"
        );
    }

    #[test]
    fn a_long_render_is_clocked_in_hours() {
        assert_eq!(
            export_status_line(0.5, secs(3600)),
            "50% \u{b7} 1:00:00 elapsed \u{b7} about 1:00:00 left"
        );
    }

    /// At the end there is nothing left to predict, and "about 0:00 left"
    /// reads like a stall.
    #[test]
    fn the_estimate_drops_away_at_the_finish() {
        assert_eq!(
            export_status_line(1.0, secs(75)),
            "100% \u{b7} 1:15 elapsed"
        );
    }

    #[test]
    fn the_clock_rolls_over_at_a_minute_and_at_an_hour() {
        assert_eq!(clock(secs(9)), "0:09");
        assert_eq!(clock(secs(59)), "0:59");
        assert_eq!(clock(secs(60)), "1:00");
        assert_eq!(clock(secs(3599)), "59:59");
        assert_eq!(clock(secs(3661)), "1:01:01");
    }
}
