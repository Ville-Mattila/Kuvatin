//! Opt-in update check wiring: the Settings toggle, "Check now", the status
//! line under them, and the badge that links to the releases page. The check
//! itself (one HTTPS HEAD, see `crate::update`) runs on a worker thread; the
//! result comes back through the event loop.

use super::AppWindow;
use crate::settings::Settings;
use crate::update;
use slint::ComponentHandle;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

type Shared = Arc<Mutex<Settings>>;

pub(super) fn wire(ui: &AppWindow) {
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
