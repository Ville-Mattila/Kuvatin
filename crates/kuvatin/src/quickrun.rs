use crate::collect::collect_images;
use anyhow::{anyhow, Result};
use kuvatin_core::batch::run_batch;
use kuvatin_core::preset::PresetStore;
use std::path::PathBuf;

/// Resolve a preset by name and run it over the given paths. Returns the number
/// of failures.
pub fn run(preset_name: &str, paths: &[PathBuf]) -> Result<usize> {
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

    let results = run_batch(&images, &preset.job, &preset.name, |p| {
        println!("[{}/{}] {}", p.done, p.total, p.input_display());
    });

    let failures = results.iter().filter(|r| r.outcome.is_err()).count();
    for r in results.iter().filter(|r| r.outcome.is_err()) {
        eprintln!(
            "FAILED {}: {}",
            r.input.display(),
            r.outcome.as_ref().err().unwrap()
        );
    }
    Ok(failures)
}
