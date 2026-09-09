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
    preset_name: &str,
    paths: &[PathBuf],
    progress: &(dyn Fn(f32, &str) + Sync),
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<QuickRunReport> {
    let store_path = PresetStore::default_path()
        .ok_or_else(|| anyhow!("could not determine config directory"))?;
    let store = PresetStore::load_or_init(&store_path)?;
    let preset = store
        .find(preset_name)
        .ok_or_else(|| anyhow!("unknown preset: {preset_name}"))?;

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
