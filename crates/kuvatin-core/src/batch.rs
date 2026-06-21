use crate::pipeline::{process_file, Job};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone)]
pub struct FileResult {
    pub input: PathBuf,
    pub outcome: Result<PathBuf, String>,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub last: FileResult,
}

impl Progress {
    pub fn input_display(&self) -> String {
        self.last.input.display().to_string()
    }
}

/// Run `job` over every input in parallel. `on_progress` is called once per
/// finished file (from worker threads — it must be `Sync`). A single failing
/// file never aborts the batch; its error is captured in the returned results.
pub fn run_batch<F>(inputs: &[PathBuf], job: &Job, preset_name: &str, on_progress: F) -> Vec<FileResult>
where
    F: Fn(Progress) + Sync,
{
    let total = inputs.len();
    let done = AtomicUsize::new(0);
    inputs
        .par_iter()
        .map(|input| {
            let outcome = process_file(input, job, preset_name).map_err(|e| e.to_string());
            let result = FileResult { input: input.clone(), outcome };
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            on_progress(Progress { done: n, total, last: result.clone() });
            result
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::OutputFormat;
    use image::{Rgba, RgbaImage};
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn batch_processes_all_and_isolates_failures() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.png");
        RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255])).save(&good).unwrap();
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"not an image").unwrap();

        let job = Job { format: OutputFormat::Jpeg, ..Job::default() };
        let calls = AtomicUsize::new(0);
        let results = run_batch(&[good.clone(), bad.clone()], &job, "t", |_p| {
            calls.fetch_add(1, Ordering::SeqCst);
        });

        assert_eq!(results.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let good_res = results.iter().find(|r| r.input == good).unwrap();
        let bad_res = results.iter().find(|r| r.input == bad).unwrap();
        assert!(good_res.outcome.is_ok());
        assert!(bad_res.outcome.is_err());
    }
}
