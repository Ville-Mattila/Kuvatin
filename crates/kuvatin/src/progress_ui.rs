//! A small always-on-top progress window for the headless right-click runs
//! (preset quick-runs, sequence renders). The work runs on a worker thread
//! while the Slint event loop owns the window; the window only appears if the
//! work outlives a short grace period, so an instant one-file job doesn't
//! flash a dialog. Cancel (button or close box) raises a flag the work polls —
//! the window stays until the work actually stops, so partial output can be
//! cleaned up.

use crate::gui::ProgressWindow;
use anyhow::{anyhow, Result};
use slint::ComponentHandle;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a run may take before the window is worth showing.
const GRACE: Duration = Duration::from_millis(350);

/// The worker's side of the window: publish progress, poll for cancellation.
/// Plain shared state (no UI handles), so it is freely `Send + Sync` for
/// rayon workers; a UI timer mirrors the latest value into the window.
pub struct ProgressSink {
    latest: Mutex<(f32, String)>,
    cancel: AtomicBool,
}

impl ProgressSink {
    fn new() -> Self {
        ProgressSink {
            latest: Mutex::new((0.0, String::new())),
            cancel: AtomicBool::new(false),
        }
    }

    pub fn set(&self, fraction: f32, status: &str) {
        // `done / total` is NaN for an empty queue, and NaN passes straight
        // through `clamp` — a bar of undefined width in the window.
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        *self.latest.lock().unwrap() = (fraction, status.to_string());
    }

    /// The most recent (fraction, status) the work reported.
    pub fn latest(&self) -> (f32, String) {
        self.latest.lock().unwrap().clone()
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }
}

/// Run `work` with no window at all (`--quiet`: installers, CI smoke runs and
/// scripts have nobody to watch a progress bar, and a headless runner may not
/// even be able to create one). Progress is discarded; cancel never fires.
pub fn run_headless<T>(work: impl FnOnce(&ProgressSink) -> T) -> Result<T> {
    Ok(work(&ProgressSink::new()))
}

/// Run `work` on a worker thread under a progress window titled `heading`,
/// blocking until it finishes; returns the work's result.
pub fn run_with_progress<T: Send + 'static>(
    heading: &str,
    work: impl FnOnce(&ProgressSink) -> T + Send + 'static,
) -> Result<T> {
    let ui = ProgressWindow::new()?;
    ui.set_heading(heading.into());
    let sink = Arc::new(ProgressSink::new());

    // Cancel button and the close box both REQUEST cancellation; the window
    // stays open (showing "Cancelling…") until the worker returns.
    {
        let sink = sink.clone();
        let w = ui.as_weak();
        ui.on_cancel(move || {
            sink.cancel.store(true, Ordering::Relaxed);
            if let Some(u) = w.upgrade() {
                u.set_cancelling(true);
            }
        });
    }
    {
        let sink = sink.clone();
        let w = ui.as_weak();
        ui.window().on_close_requested(move || {
            sink.cancel.store(true, Ordering::Relaxed);
            if let Some(u) = w.upgrade() {
                u.set_cancelling(true);
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }

    let result: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
    let done = Arc::new(AtomicBool::new(false));
    let worker = {
        let sink = sink.clone();
        let result = result.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            // A panicking worker must still release the event loop.
            if let Ok(r) = catch_unwind(AssertUnwindSafe(|| work(&sink))) {
                *result.lock().unwrap() = Some(r);
            }
            done.store(true, Ordering::SeqCst);
            let _ = slint::invoke_from_event_loop(|| {
                let _ = slint::quit_event_loop();
            });
        })
    };

    // Show only if the work outlives the grace period.
    let show_timer = slint::Timer::default();
    {
        let w = ui.as_weak();
        let done = done.clone();
        show_timer.start(slint::TimerMode::SingleShot, GRACE, move || {
            if !done.load(Ordering::SeqCst) {
                if let Some(u) = w.upgrade() {
                    let _ = u.show();
                }
            }
        });
    }
    // Mirror the latest progress into the window, coalesced to one update
    // per tick (workers may report dozens of times a second).
    let poll = slint::Timer::default();
    {
        let w = ui.as_weak();
        let sink = sink.clone();
        let done = done.clone();
        poll.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(50),
            move || {
                if let Some(u) = w.upgrade() {
                    let (f, s) = sink.latest();
                    u.set_progress(f);
                    u.set_status(s.into());
                }
                // The worker posts a quit when it finishes, but a quit posted
                // before this loop was running is dropped on the floor — a
                // fast job would then hang the process on a loop whose window
                // is never even shown. This is the backstop: the loop cannot
                // outlive the work by more than one tick.
                if done.load(Ordering::SeqCst) {
                    let _ = slint::quit_event_loop();
                }
            },
        );
    }

    // ...and if the work finished before we got here, don't start at all.
    if !done.load(Ordering::SeqCst) {
        slint::run_event_loop_until_quit()?;
    }
    drop(show_timer);
    drop(poll);
    let _ = ui.hide();
    let _ = worker.join();
    let outcome = result.lock().unwrap().take();
    outcome.ok_or_else(|| anyhow!("the run stopped unexpectedly (internal error)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fraction_is_clamped_into_range() {
        let sink = ProgressSink::new();
        sink.set(-1.0, "before the start");
        assert_eq!(sink.latest().0, 0.0);
        sink.set(2.0, "past the end");
        assert_eq!(sink.latest().0, 1.0);
        sink.set(0.25, "a quarter");
        assert_eq!(sink.latest(), (0.25, "a quarter".to_string()));
    }

    /// `done / total` with an empty queue is NaN, and NaN survives `clamp` —
    /// it would reach the window as a bar of undefined width.
    #[test]
    fn a_nonsense_fraction_reads_as_no_progress() {
        let sink = ProgressSink::new();
        sink.set(f32::NAN, "empty queue");
        assert_eq!(sink.latest().0, 0.0);
        sink.set(f32::INFINITY, "still nonsense");
        assert_eq!(sink.latest().0, 0.0);
    }

    #[test]
    fn cancellation_latches_once_asked_for() {
        let sink = ProgressSink::new();
        assert!(!sink.cancelled());
        sink.cancel_flag().store(true, Ordering::Relaxed);
        assert!(sink.cancelled());
        // Later progress does not clear it: the run is over either way.
        sink.set(0.9, "nearly there");
        assert!(sink.cancelled());
    }

    #[test]
    fn a_headless_run_never_reports_cancellation() {
        let seen = run_headless(|sink| {
            sink.set(0.5, "half");
            (sink.cancelled(), sink.latest())
        })
        .unwrap();
        assert_eq!(seen, (false, (0.5, "half".to_string())));
    }
}
