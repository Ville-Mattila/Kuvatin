//! Export: the settings dialog's "Export…" → deferred `begin_render`, the
//! progress modal (with a stall watchdog) and Cancel.

use super::VideoState;
use crate::gui::{show_error, show_info_at, AppWindow};
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

/// What the user is told once the preview is back and the modal closes.
enum Ending {
    /// The render finished: the file it wrote, how long it took, the encoder
    /// that did it, and whether it had to fall back to software on the way.
    Done(PathBuf, Option<std::time::Duration>, Option<String>, bool),
    /// It failed, with the engine's reason.
    Failed(String),
}

/// The running (or about-to-start) export.
#[derive(Default)]
pub(crate) struct ExportState {
    /// True while an export/render is running (pauses the preview timer, whose
    /// seeks/commits would corrupt the render).
    pub(super) active: Rc<Cell<bool>>,
    /// True while the pipeline is TRANSITIONING for an export: tearing the
    /// preview down before a render, or bringing it back after one. The
    /// preview timer must not touch the pipeline in either window.
    pub(super) pending: Rc<Cell<bool>>,
    /// The output path of the running export, so Cancel/failure can delete
    /// the partial file.
    pub(super) path: Rc<RefCell<Option<PathBuf>>>,
    /// When the render actually started, for the elapsed/remaining line and
    /// the "took 2:14" in the finished dialog.
    pub(super) started: Rc<Cell<Option<std::time::Instant>>>,
    /// A render waiting for the preview teardown to finish; the progress timer
    /// starts it once the pipeline has settled.
    starting: Rc<RefCell<Option<(PathBuf, kuvatin_video::ExportSettings)>>>,
    /// The preview is coming back (after a finish, a failure or a cancel).
    finishing: Rc<Cell<bool>>,
    /// The settings the running render was started with, so a failure knows
    /// what was asked for and a fallback knows what to change.
    running: Rc<Cell<Option<kuvatin_video::ExportSettings>>>,
    /// This export has already been retried in software; a second failure is
    /// the file's, not the encoder's.
    fell_back: Rc<Cell<bool>>,
    /// What to say when it is back. None after a cancel: nothing to report.
    ending: Rc<RefCell<Option<Ending>>>,
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

/// Should a failed render be tried again in software?
///
/// Only when the machine was asked to decide (`Auto`), only when the encoder
/// that failed was a hardware one, and only once. A software encoder that
/// failed will fail the same way twice, and someone who explicitly chose
/// Hardware wants to see the failure, not a quiet substitution. `None` for the
/// encoder means the render fell over before one was built, which a different
/// encoder would not fix either.
fn should_fall_back(
    choice: kuvatin_video::Encoder,
    encoder_used: Option<&str>,
    already_fell_back: bool,
) -> bool {
    choice == kuvatin_video::Encoder::Auto
        && !already_fell_back
        && encoder_used.map(kuvatin_video::is_hardware_encoder) == Some(true)
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
    let export_starting = &ex.starting;
    let export_finishing = &ex.finishing;
    let export_running = &ex.running;
    let export_fell_back = &ex.fell_back;
    let export_ending = &ex.ending;

    // Whether the "Hardware" choice is offered at all. Asking costs a registry
    // scan, so it happens on a worker rather than in front of the window.
    {
        let ui_weak = ui_weak.clone();
        let _ = std::thread::Builder::new()
            .name("kuvatin-encoder-probe".into())
            .spawn(move || {
                let available = kuvatin_video::hardware_encoding_available();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_hardware_encoder_available(available);
                    }
                });
            });
    }
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let export_active = export_active.clone();
        let export_pending = export_pending.clone();
        let export_path = export_path.clone();
        let export_starting = export_starting.clone();
        let export_fell_back = export_fell_back.clone();
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
                encoder: match ui.get_export_encoder() {
                    1 => kuvatin_video::Encoder::Hardware,
                    2 => kuvatin_video::Encoder::Software,
                    _ => kuvatin_video::Encoder::Auto,
                },
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
            // Tearing the preview down has to finish before the render can
            // take the pipeline over (the GPU context is not released
            // synchronously), but waiting for it here would freeze the window
            // for up to three seconds with the modal already on screen. Ask
            // for the teardown, show the modal, and let the progress timer
            // start the render when the pipeline has settled.
            *export_path.borrow_mut() = Some(path.clone());
            let prepared = project_slot.borrow().as_ref().map(|p| p.prepare_render());
            match prepared {
                Some(Ok(())) => {
                    export_pending.set(true);
                    export_fell_back.set(false);
                    *export_starting.borrow_mut() = Some((path, settings));
                    ui.set_exporting(true);
                    ui.set_export_progress(0.0);
                    ui.set_export_status("Starting\u{2026}".into());
                }
                Some(Err(e)) => {
                    export_path.borrow_mut().take();
                    show_error(&ui, "Export failed to start", e.to_string());
                }
                None => {
                    export_path.borrow_mut().take();
                }
            }
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
        let export_starting = export_starting.clone();
        let export_finishing = export_finishing.clone();
        let export_ending = export_ending.clone();
        ui.on_export_cancel(move || {
            // Nothing to cancel once the preview is already coming back.
            if export_finishing.get() {
                return;
            }
            let waiting_to_start = export_starting.borrow_mut().take().is_some();
            if !waiting_to_start && !export_active.get() {
                return;
            }
            let path = export_path.borrow_mut().take();
            export_started.take();
            export_active.set(false);
            // The partial file is discarded, so there is no muxer to wait for:
            // stop, delete, and bring the preview back on the timer.
            if let Some(p) = project_slot.borrow().as_ref() {
                let _ = p.begin_restore();
            }
            if let Some(path) = path {
                let _ = std::fs::remove_file(path);
            }
            export_ending.borrow_mut().take();
            export_finishing.set(true);
            export_pending.set(true);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_export_status("Cancelling\u{2026}".into());
            }
        });
    }

    // Export progress: poll the render; finish or fail restores the preview.
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let export_active = export_active.clone();
        let export_pending = export_pending.clone();
        let export_path = export_path.clone();
        let export_started = export_started.clone();
        let export_starting = export_starting.clone();
        let export_finishing = export_finishing.clone();
        let export_ending = export_ending.clone();
        let export_running = export_running.clone();
        let export_fell_back = export_fell_back.clone();
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
                let slot = project_slot.borrow();
                let Some(p) = slot.as_ref() else {
                    return;
                };

                // Phase one: the preview is being torn down. Start the render
                // the moment the pipeline has settled, and not before — this
                // is the wait that used to freeze the window.
                let waiting = export_starting.borrow().clone();
                if let Some((path, settings)) = waiting {
                    if p.render_ready() == kuvatin_video::Step::Pending {
                        return;
                    }
                    export_starting.borrow_mut().take();
                    match p.start_render(&path, settings) {
                        Ok(()) => {
                            export_started.set(Some(std::time::Instant::now()));
                            export_running.set(Some(settings));
                            export_pending.set(false);
                            export_active.set(true);
                        }
                        Err(e) => {
                            drop(slot);
                            let _ = std::fs::remove_file(&path);
                            export_path.borrow_mut().take();
                            export_pending.set(false);
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_exporting(false);
                                show_error(&ui, "Export failed to start", e.to_string());
                            }
                        }
                    }
                    return;
                }

                // Phase three: the preview is coming back. The modal stays up
                // (saying so) until it has, because the timeline is inert
                // until then — a click that did nothing would be worse.
                if export_finishing.get() {
                    if p.restore_ready() == kuvatin_video::Step::Pending {
                        return;
                    }
                    drop(slot);
                    export_finishing.set(false);
                    export_pending.set(false);
                    stall.set((0.0, 0));
                    let ending = export_ending.borrow_mut().take();
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_exporting(false);
                        match ending {
                            // The dialog used to just vanish, leaving no sign
                            // that anything had been written, or where.
                            Some(Ending::Done(path, took, used, fell_back)) => {
                                let mut detail = path.display().to_string();
                                if let Some(t) = took {
                                    detail.push_str(&format!("\n\nTook {}", clock(t)));
                                }
                                if let Some(name) = used {
                                    detail.push_str(&format!("\nEncoder: {name}"));
                                }
                                if fell_back {
                                    detail.push_str(
                                        "\n\nThe hardware encoder failed on this export, so it was \
                                         encoded in software. Choosing Software in the export \
                                         settings skips the attempt next time.",
                                    );
                                }
                                show_info_at(&ui, "Export finished", detail, &path);
                            }
                            Some(Ending::Failed(e)) => show_error(&ui, "Export failed", e),
                            None => {}
                        }
                    }
                    return;
                }

                // Phase two: rendering.
                if !export_active.get() {
                    return;
                }
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
                        let used = p.render_encoder();
                        let restore = p.begin_restore();
                        drop(slot);
                        export_running.set(None);
                        export_active.set(false);
                        export_pending.set(true);
                        export_finishing.set(true);
                        let done = export_path.borrow_mut().take();
                        let took = export_started.take().map(|t| t.elapsed());
                        *export_ending.borrow_mut() = match (restore, done) {
                            (Err(e), _) => Some(Ending::Failed(format!(
                                "The file is written, but the preview could not be restored: {e}"
                            ))),
                            (Ok(()), Some(path)) => {
                                Some(Ending::Done(path, took, used, export_fell_back.get()))
                            }
                            (Ok(()), None) => None,
                        };
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_export_progress(1.0);
                            ui.set_export_status("Finishing\u{2026}".into());
                        }
                    }
                    kuvatin_video::RenderStatus::Failed(e) => {
                        let used = p.render_encoder();
                        let settings = export_running.get();
                        // A hardware encoder that fails mid-session used to
                        // fail the whole export with an engine message about
                        // an encode session. If the machine was asked to
                        // decide, it decides again — in software.
                        let retry = should_fall_back(
                            settings.map(|s| s.encoder).unwrap_or_default(),
                            used.as_deref(),
                            export_fell_back.get(),
                        );
                        if retry {
                            if let (Some(mut settings), Some(path)) =
                                (settings, export_path.borrow().clone())
                            {
                                settings.encoder = kuvatin_video::Encoder::Software;
                                // The half-written file goes: the retry writes
                                // the same name from the beginning.
                                let _ = std::fs::remove_file(&path);
                                if p.prepare_render().is_ok() {
                                    drop(slot);
                                    export_active.set(false);
                                    export_pending.set(true);
                                    export_fell_back.set(true);
                                    export_started.take();
                                    stall.set((0.0, 0));
                                    *export_starting.borrow_mut() = Some((path, settings));
                                    if let Some(ui) = ui_weak.upgrade() {
                                        ui.set_export_progress(0.0);
                                        ui.set_export_status(
                                            "The hardware encoder failed \u{2014} encoding in software\u{2026}"
                                                .into(),
                                        );
                                    }
                                    return;
                                }
                            }
                        }
                        let _ = p.begin_restore();
                        drop(slot);
                        export_active.set(false);
                        export_pending.set(true);
                        export_finishing.set(true);
                        export_started.take();
                        export_running.set(None);
                        // Delete the truncated output; the reason waits for
                        // the preview so there is only ever one dialog.
                        if let Some(path) = export_path.borrow_mut().take() {
                            let _ = std::fs::remove_file(path);
                        }
                        *export_ending.borrow_mut() = Some(Ending::Failed(match used {
                            Some(name) => format!("{e}\n\nEncoder: {name}"),
                            None => e,
                        }));
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_export_status("Finishing\u{2026}".into());
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

    /// The case this exists for: the machine was asked to decide, the hardware
    /// encoder it picked failed, and the file is still wanted.
    #[test]
    fn a_hardware_failure_under_automatic_is_retried_in_software() {
        use kuvatin_video::Encoder;
        assert!(should_fall_back(
            Encoder::Auto,
            Some("nvautogpuh264enc"),
            false
        ));
    }

    #[test]
    fn nothing_else_is_retried() {
        use kuvatin_video::Encoder;
        // The software encoder failing is not an encoder problem.
        assert!(!should_fall_back(Encoder::Auto, Some("x264enc"), false));
        // Asked for hardware: report the failure, do not substitute.
        assert!(!should_fall_back(
            Encoder::Hardware,
            Some("nvautogpuh264enc"),
            false
        ));
        // Already software.
        assert!(!should_fall_back(Encoder::Software, Some("x264enc"), false));
        // Once is a fallback; twice is a loop.
        assert!(!should_fall_back(
            Encoder::Auto,
            Some("nvautogpuh264enc"),
            true
        ));
        // It fell over before an encoder existed: something else is wrong.
        assert!(!should_fall_back(Encoder::Auto, None, false));
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
