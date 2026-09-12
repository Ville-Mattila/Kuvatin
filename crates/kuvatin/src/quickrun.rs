use crate::collect::collect_images;
use anyhow::{anyhow, Result};
use kuvatin_core::batch::{run_batch_until, CANCELLED};
use kuvatin_core::preset::PresetStore;
use std::io::Write;
use std::path::PathBuf;

/// Outcome of a quick-run over a set of paths.
pub struct QuickRunReport {
    /// Total images the run attempted.
    pub total: usize,
    /// `(input path, error message)` for every file that failed.
    pub failures: Vec<(PathBuf, String)>,
    /// Inputs skipped because the user cancelled mid-run.
    pub cancelled: usize,
}

impl QuickRunReport {
    pub fn failure_count(&self) -> usize {
        self.failures.len()
    }
}

/// What a finished quick run means for the person who started it.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The user cancelled: no dialog at all, even if files had already failed
    /// before they stopped it. They know what they did.
    Cancelled,
    /// Some files could not be processed: name them and exit non-zero, so a
    /// script that invoked us can tell.
    Failed { failed: usize, total: usize },
    /// Everything went through; log the count and say nothing.
    Converted(usize),
}

/// Read the report the way the shell entry point does. This is the contract of
/// the entire right-click feature, and it used to live inline in a match arm.
pub fn verdict(report: &QuickRunReport) -> Verdict {
    if report.cancelled > 0 {
        return Verdict::Cancelled;
    }
    if report.failure_count() > 0 {
        return Verdict::Failed {
            failed: report.failure_count(),
            total: report.total,
        };
    }
    Verdict::Converted(report.total - report.failure_count() - report.cancelled)
}

/// Find the named preset, or explain why it isn't there.
///
/// "unknown preset" on its own is misleading when the store was unreadable
/// and quietly replaced by the built-ins: the user's preset really did exist,
/// and the note explaining what happened to it is attached here rather than
/// discarded.
pub fn resolve_preset<'a>(
    store: &'a PresetStore,
    name: &str,
) -> Result<&'a kuvatin_core::preset::Preset> {
    if let Some(preset) = store.find(name) {
        return Ok(preset);
    }
    match &store.last_load_warning {
        Some(note) => Err(anyhow!("unknown preset: {name}\n\n{note}")),
        None => Err(anyhow!("unknown preset: {name}")),
    }
}

/// The user's preset store for the headless paths, with any load warning
/// recorded — nobody is watching a console on a right-click run.
pub fn load_store() -> Result<PresetStore> {
    let store_path = PresetStore::default_path()
        .ok_or_else(|| anyhow!("could not determine config directory"))?;
    let store = PresetStore::load_or_init(&store_path)?;
    if let Some(note) = &store.last_load_warning {
        crate::applog::log(&format!("preset store: {note}"));
    }
    Ok(store)
}

/// Resolve a preset by name and run it over the given paths.
///
/// `progress(fraction, status)` is called once per finished file (from worker
/// threads); `cancelled()` is polled before each file starts. Progress and
/// per-file failures are also written to the console when one exists; in the
/// windowed release build (launched from the Explorer context menu) there is
/// no console, so those writes are best-effort — a plain `println!` there
/// would panic on the failed write. The returned report lets `main` surface
/// the outcome to the user via a message box instead.
pub fn run(
    store: &PresetStore,
    preset_name: &str,
    paths: &[PathBuf],
    progress: &(dyn Fn(f32, &str) + Sync),
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<QuickRunReport> {
    let preset = resolve_preset(store, preset_name)?;

    let images = collect_images(paths);
    if images.is_empty() {
        return Err(anyhow!("no image files in selection"));
    }

    let results = run_batch_until(
        &images,
        &preset.job,
        |p| {
            let _ = writeln!(
                std::io::stdout(),
                "[{}/{}] {}",
                p.done,
                p.total,
                p.input_display()
            );
            let name = p
                .last
                .input
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            progress(
                p.done as f32 / p.total.max(1) as f32,
                &format!("{} / {}  ·  {name}", p.done, p.total),
            );
        },
        cancelled,
    );

    let mut failures = Vec::new();
    let mut cancelled_count = 0;
    for r in results.iter() {
        match &r.outcome {
            Err(e) if e == CANCELLED => cancelled_count += 1,
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "FAILED {}: {}", r.input.display(), e);
                crate::applog::log(&format!("FAILED {}: {e}", r.input.display()));
                failures.push((r.input.clone(), e.to_string()));
            }
            Ok(_) => {}
        }
    }

    Ok(QuickRunReport {
        total: images.len(),
        failures,
        cancelled: cancelled_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(total: usize, failed: usize, cancelled: usize) -> QuickRunReport {
        QuickRunReport {
            total,
            failures: (0..failed)
                .map(|n| (PathBuf::from(format!("{n}.png")), "no".to_string()))
                .collect(),
            cancelled,
        }
    }

    /// The contract of the whole right-click feature: a cancelled run says
    /// nothing, a run with failures reports them and exits non-zero, and an
    /// ordinary run just logs. It lived in a match arm in main() with no test.
    #[test]
    fn a_cancelled_run_is_silent_even_when_files_had_already_failed() {
        assert_eq!(verdict(&report(9, 2, 4)), Verdict::Cancelled);
        assert_eq!(verdict(&report(9, 0, 9)), Verdict::Cancelled);
    }

    #[test]
    fn failures_are_reported_against_the_total() {
        assert_eq!(
            verdict(&report(5, 2, 0)),
            Verdict::Failed {
                failed: 2,
                total: 5
            }
        );
    }

    #[test]
    fn a_clean_run_counts_what_it_converted() {
        assert_eq!(verdict(&report(3, 0, 0)), Verdict::Converted(3));
    }

    /// "unknown preset" on its own sends the user hunting for a typo when the
    /// real story is that their store could not be read and was replaced.
    #[test]
    fn an_unknown_preset_carries_the_stores_warning() {
        let mut store = PresetStore::builtin();
        store.last_load_warning =
            Some("presets.toml could not be read (invalid UTF-8); using the built-ins.".into());

        let err = resolve_preset(&store, "My Preset")
            .expect_err("not a built-in")
            .to_string();
        assert!(err.contains("My Preset"), "names the preset: {err}");
        assert!(err.contains("could not be read"), "explains why: {err}");
    }

    /// With a healthy store the message stays short: it really is a typo.
    #[test]
    fn an_unknown_preset_in_a_healthy_store_says_only_that() {
        let err = resolve_preset(&PresetStore::builtin(), "Nope")
            .expect_err("not a built-in")
            .to_string();
        assert!(err.contains("Nope"), "{err}");
        assert!(!err.contains("presets.toml"), "no invented context: {err}");
    }

    /// Resolution matches the way the menu and the command line spell names.
    #[test]
    fn a_known_preset_resolves_case_insensitively() {
        let store = PresetStore::builtin();
        let preset = resolve_preset(&store, "  convert to WEBP ").expect("a built-in");
        assert_eq!(preset.name, "Convert to WebP");
    }
}
