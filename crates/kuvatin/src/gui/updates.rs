//! Opt-in update check wiring: the Settings toggle, "Check now", the status
//! line under them, and the badge that opens the update dialog. The check
//! itself (one HTTPS HEAD, see `crate::update`) and the download behind the
//! dialog both run on worker threads; results come back through the event
//! loop.

use super::{video, AppWindow};
use crate::settings::Settings;
use crate::update;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

type Shared = Arc<Mutex<Settings>>;

/// The flag the download in flight watches. Every run gets a fresh one, so a
/// worker that is still unwinding from a cancel keeps its own flag set for
/// good and can never be mistaken for the current run.
type CancelSlot = Rc<RefCell<Arc<AtomicBool>>>;

pub(super) fn wire(ui: &AppWindow, project: video::ProjectSlot) {
    // Whatever a previous update left staged, including the copy of the
    // executable that did the installing and so could not delete itself.
    update::apply::sweep_stage();
    ui.set_app_version(update::CURRENT.into());

    let settings: Shared = Arc::new(Mutex::new(Settings::load()));
    apply_status(ui, &settings.lock().unwrap());

    {
        let settings = settings.clone();
        let ui_weak = ui.as_weak();
        ui.on_update_check_toggled(move |on| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            {
                let mut s = settings.lock().unwrap();
                s.check_updates = on;
                save(&s);
                apply_status(&ui, &s);
            }
            if on {
                start_check(&ui, &settings, false);
            }
        });
    }
    {
        let settings = settings.clone();
        let ui_weak = ui.as_weak();
        ui.on_check_updates_now(move || {
            if let Some(ui) = ui_weak.upgrade() {
                start_check(&ui, &settings, true);
            }
        });
    }
    ui.on_open_releases(update::open_releases_page);

    {
        let ui_weak = ui.as_weak();
        ui.on_update_open(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_update_unsaved(video::has_unsaved_changes(&project));
                ui.set_update_error("".into());
                ui.set_update_phase(1);
            }
        });
    }

    let cancel: CancelSlot = Rc::new(RefCell::new(Arc::new(AtomicBool::new(false))));
    {
        let settings = settings.clone();
        let cancel = cancel.clone();
        let ui_weak = ui.as_weak();
        ui.on_update_start(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let version = settings.lock().unwrap().latest_seen.clone();
            if version.is_empty() {
                fail(&ui, "There is no version to install.".into());
                return;
            }
            // Take over as the run in flight, and stop whatever held the slot
            // before: two downloads writing the same staging folder would
            // sweep each other's files away.
            let stop = Arc::new(AtomicBool::new(false));
            {
                let mut in_flight = cancel.borrow_mut();
                in_flight.store(true, Ordering::Relaxed);
                *in_flight = stop.clone();
            }
            ui.set_update_step("Starting the download".into());
            ui.set_update_progress(-1.0);

            let ui_weak = ui.as_weak();
            std::thread::spawn(move || {
                let progress_ui = ui_weak.clone();
                let mut on_progress = move |p: update::fetch::Progress| {
                    let step = match p.total {
                        Some(total) => format!(
                            "Downloading {:.1} of {:.1} MB",
                            p.done as f64 / 1_048_576.0,
                            total as f64 / 1_048_576.0
                        ),
                        None => format!("Downloading {:.1} MB", p.done as f64 / 1_048_576.0),
                    };
                    let fraction = p.fraction().unwrap_or(-1.0);
                    let _ = progress_ui.upgrade_in_event_loop(move |ui| {
                        ui.set_update_step(step.into());
                        ui.set_update_progress(fraction);
                    });
                };
                let staged = update::apply::stage(&version, &stop, &mut on_progress);
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    // Cancel is the working dialog's only way out, so it wins
                    // even when it lands after the download finished: the user
                    // asked for the update to stop, and closing the app on them
                    // instead is the one thing they did not ask for. The
                    // download is thrown away rather than installed.
                    if stop.load(Ordering::Relaxed) {
                        if staged.is_ok() {
                            crate::applog::log(
                                "update: cancelled once the download was in; discarding it",
                            );
                            update::apply::sweep_stage();
                        }
                        // Only close the dialog this run put up. By now the
                        // user may have opened the confirmation again.
                        if ui.get_update_phase() == 2 {
                            ui.set_update_phase(0);
                        }
                        return;
                    }
                    match staged {
                        Ok(staged) => {
                            ui.set_update_step("Closing to install".into());
                            let exe = std::env::current_exe().unwrap_or_default();
                            match update::apply::hand_off(&staged, &exe) {
                                Ok(()) => {
                                    crate::applog::log("update: handed off to the staged copy");
                                    let _ = slint::quit_event_loop();
                                }
                                Err(e) => {
                                    let said = format!("{e:#}");
                                    crate::applog::log(&format!("update failed: {said}"));
                                    fail(&ui, readable(&said, &version));
                                }
                            }
                        }
                        Err(e) => {
                            let said = format!("{e:#}");
                            crate::applog::log(&format!("update failed: {said}"));
                            fail(&ui, readable(&said, &version));
                        }
                    }
                });
            });
        });
    }
    {
        let cancel = cancel.clone();
        let ui_weak = ui.as_weak();
        ui.on_update_cancel(move || {
            cancel.borrow().store(true, Ordering::Relaxed);
            crate::applog::log("update: cancelled");
            // Nothing here waits on the worker. The click closes the dialog
            // and the download unwinds into a discard, so the one button the
            // working dialog has always works.
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_update_phase(0);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_update_save_first(move || {
            // The dialog has already closed itself. Saving may ask where to
            // put the file and may be called off, so nothing is chained to it:
            // the badge is still there to click again afterwards.
            if let Some(ui) = ui_weak.upgrade() {
                ui.invoke_video_save_project(true);
            }
        });
    }

    // Opted in: check at startup when the last one is a day old.
    if settings.lock().unwrap().check_updates {
        start_check(ui, &settings, false);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn save(s: &Settings) {
    if let Err(e) = s.save() {
        crate::applog::log(&format!("settings save failed: {e:#}"));
    }
}

/// Put the dialog into its failed state with a sentence the user can act on.
fn fail(ui: &AppWindow, message: String) {
    ui.set_update_error(message.into());
    ui.set_update_phase(3);
}

/// The Win32 error inside an `0x8007XXXX` HRESULT, when the text carries one.
/// WinHTTP failures arrive as nothing but that code.
fn win32_code(said: &str) -> Option<u32> {
    said.match_indices("0x").find_map(|(at, _)| {
        let digits = said.get(at + 2..at + 10)?;
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let hresult = u32::from_str_radix(digits, 16).ok()?;
        // Failure, facility Win32: the low half is the Win32 error itself.
        (hresult >> 16 == 0x8007).then_some(hresult & 0xffff)
    })
}

/// Plain words for the WinHTTP failures a user actually meets. Windows keeps
/// these strings in `winhttp.dll` rather than the system message table, so
/// `FormatMessage` finds nothing and the code is all that is left.
fn network_reason(code: u32) -> Option<&'static str> {
    Some(match code {
        12002 => "the connection timed out",
        12007 => "the address could not be looked up",
        12029 => "no connection could be made",
        12030 | 12031 => "the connection was lost",
        12045 | 12175 => "the secure connection could not be set up",
        12152 => "the server's answer made no sense",
        _ => return None,
    })
}

/// The WinHTTP error numbers, which all sit above 12000.
const WINHTTP_ERRORS: std::ops::Range<u32> = 12001..13000;

/// Turn what a download failed with into a sentence for `update-error`. The
/// module below writes its failures as fragments, so they can be joined into a
/// context chain; here they become something to read.
fn readable(said: &str, version: &str) -> String {
    if said.contains("HTTP 404") {
        return format!("Version {version} has no installer published.");
    }
    match win32_code(said).filter(|c| WINHTTP_ERRORS.contains(c)) {
        Some(code) => {
            let reason = match network_reason(code) {
                Some(words) => words.to_string(),
                // Not one we have words for, but the code still has to show:
                // it is the only thing to search for or report.
                None => format!("Windows reported {}", said.trim_end_matches('.')),
            };
            format!("Could not reach the download: {reason}.")
        }
        None => sentence(said),
    }
}

/// A capital and a full stop.
fn sentence(said: &str) -> String {
    let said = said.trim();
    if said.is_empty() {
        return "The update did not go through.".to_string();
    }
    let mut out = String::with_capacity(said.len() + 1);
    let mut chars = said.chars();
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// Mirror the settings into the toggle, the status line and the badge.
fn apply_status(ui: &AppWindow, s: &Settings) {
    ui.set_update_check_enabled(s.check_updates);
    let newer = !s.latest_seen.is_empty() && update::is_newer(&s.latest_seen, update::CURRENT);
    ui.set_update_available(if newer {
        s.latest_seen.clone().into()
    } else {
        "".into()
    });
    let text = if !s.check_updates {
        "Off. Kuvatin makes no network requests unless you turn this on.".to_string()
    } else if s.latest_seen.is_empty() {
        "Not checked yet.".to_string()
    } else if newer {
        format!("Version {} is available.", s.latest_seen)
    } else {
        format!("Up to date ({}).", update::CURRENT)
    };
    ui.set_update_status(text.into());
}

/// Run a check on a worker thread unless one ran within the interval (a
/// forced check ignores the interval). The result is stored, then shown.
fn start_check(ui: &AppWindow, settings: &Shared, force: bool) {
    let last = settings.lock().unwrap().last_update_check;
    if !force && now().saturating_sub(last) < update::CHECK_INTERVAL_SECS {
        return;
    }
    ui.set_update_status("Checking…".into());
    let ui_weak = ui.as_weak();
    let settings = settings.clone();
    std::thread::spawn(move || {
        let result = update::latest_version();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            let mut s = settings.lock().unwrap();
            s.last_update_check = now();
            match result {
                Ok(v) => {
                    crate::applog::log(&format!(
                        "update check: latest {v}, running {}",
                        update::CURRENT
                    ));
                    s.latest_seen = v;
                    save(&s);
                    apply_status(&ui, &s);
                }
                Err(e) => {
                    crate::applog::log(&format!("update check failed: {e:#}"));
                    save(&s);
                    apply_status(&ui, &s);
                    ui.set_update_status(format!("Could not check: {e}").into());
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_hresult_gives_up_the_win32_error_inside_it() {
        assert_eq!(win32_code("0x80072EE7"), Some(12007));
        assert_eq!(win32_code("the download stopped (0x80072ee2)"), Some(12002));
        // A different facility is not a Win32 error, and a bare number is not
        // an HRESULT at all.
        assert_eq!(win32_code("0x80004005"), None);
        assert_eq!(win32_code("the server answered HTTP 404"), None);
        assert_eq!(win32_code("0x8007"), None);
    }

    #[test]
    fn a_transport_failure_reads_as_a_sentence() {
        assert_eq!(
            readable("0x80072EE7", "2.13.0"),
            "Could not reach the download: the address could not be looked up."
        );
        assert_eq!(
            readable("0x80072EFD", "2.13.0"),
            "Could not reach the download: no connection could be made."
        );
        assert_eq!(
            readable("0x80072EE2", "2.13.0"),
            "Could not reach the download: the connection timed out."
        );
        assert_eq!(
            readable("0x80072EFE", "2.13.0"),
            "Could not reach the download: the connection was lost."
        );
    }

    #[test]
    fn a_code_with_no_words_for_it_still_shows_behind_a_lead_in() {
        let said = readable("0x80072F05", "2.13.0");
        assert!(
            said.starts_with("Could not reach the download: "),
            "no lead-in: {said}"
        );
        assert!(said.contains("0x80072F05"), "code went missing: {said}");
    }

    #[test]
    fn the_other_failures_keep_their_own_words() {
        assert_eq!(
            readable("the download did not arrive intact", "2.13.0"),
            "The download did not arrive intact."
        );
        assert_eq!(
            readable("the server answered HTTP 404", "2.13.0"),
            "Version 2.13.0 has no installer published."
        );
        assert_eq!(
            readable(
                r"could not write to C:\Temp\kuvatin\update: access denied",
                "2.13.0"
            ),
            r"Could not write to C:\Temp\kuvatin\update: access denied."
        );
    }
}
