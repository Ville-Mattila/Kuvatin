use crate::pipeline::{process_file, process_file_to, Job};
use rayon::prelude::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Run one fallible file operation with panic isolation: a panicking codec or
/// dependency becomes an `Err` like any other failure instead of unwinding the
/// whole rayon batch (which discarded every result and froze GUI progress).
fn isolate<F: FnOnce() -> Result<PathBuf, String>>(f: F) -> Result<PathBuf, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(outcome) => outcome,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            Err(format!("internal error: {msg}"))
        }
    }
}

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

/// Outcome message of inputs skipped because the batch was cancelled.
pub const CANCELLED: &str = "cancelled";

/// Run `job` over every input in parallel. `on_progress` is called once per
/// finished file (from worker threads — it must be `Sync`). A single failing
/// file never aborts the batch; its error is captured in the returned results.
pub fn run_batch<F>(inputs: &[PathBuf], job: &Job, on_progress: F) -> Vec<FileResult>
where
    F: Fn(Progress) + Sync,
{
    run_batch_until(inputs, job, on_progress, || false)
}

/// Like [`run_batch`], but stops picking up inputs once `cancelled()` returns
/// true: files already in flight finish normally, the rest come back with
/// `Err(CANCELLED)` and no progress call.
pub fn run_batch_until<F, C>(
    inputs: &[PathBuf],
    job: &Job,
    on_progress: F,
    cancelled: C,
) -> Vec<FileResult>
where
    F: Fn(Progress) + Sync,
    C: Fn() -> bool + Sync,
{
    let items: Vec<(PathBuf, Job)> = inputs.iter().map(|p| (p.clone(), job.clone())).collect();
    run_jobs_until(&items, on_progress, cancelled)
}

/// Like `run_batch`, but each input carries its own `Job` (e.g. a per-image
/// crop). Runs in parallel with the same failure isolation and progress
/// semantics as `run_batch`.
pub fn run_jobs<F>(items: &[(PathBuf, Job)], on_progress: F) -> Vec<FileResult>
where
    F: Fn(Progress) + Sync,
{
    run_jobs_until(items, on_progress, || false)
}

/// [`run_jobs`] with the cancellation semantics of [`run_batch_until`].
pub fn run_jobs_until<F, C>(
    items: &[(PathBuf, Job)],
    on_progress: F,
    cancelled: C,
) -> Vec<FileResult>
where
    F: Fn(Progress) + Sync,
    C: Fn() -> bool + Sync,
{
    let total = items.len();
    let done = AtomicUsize::new(0);
    items
        .par_iter()
        .map(|(input, job)| {
            if cancelled() {
                return FileResult {
                    input: input.clone(),
                    outcome: Err(CANCELLED.into()),
                };
            }
            let outcome = isolate(|| process_file(input, job).map_err(|e| e.to_string()));
            let result = FileResult {
                input: input.clone(),
                outcome,
            };
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            on_progress(Progress {
                done: n,
                total,
                last: result.clone(),
            });
            result
        })
        .collect()
}

/// Like [`run_jobs`], but each item also carries the exact output path to write
/// to (e.g. a user-chosen save location or a per-file path inside a chosen
/// output folder). Same parallelism and failure-isolation semantics.
pub fn run_jobs_to<F>(items: &[(PathBuf, Job, PathBuf)], on_progress: F) -> Vec<FileResult>
where
    F: Fn(Progress) + Sync,
{
    run_jobs_to_until(items, on_progress, || false)
}

/// [`run_jobs_to`] with the cancellation semantics of [`run_batch_until`]: an
/// item started after the flag is set is reported as [`CANCELLED`] without
/// being converted, and without a progress call.
pub fn run_jobs_to_until<F, C>(
    items: &[(PathBuf, Job, PathBuf)],
    on_progress: F,
    cancelled: C,
) -> Vec<FileResult>
where
    F: Fn(Progress) + Sync,
    C: Fn() -> bool + Sync,
{
    let total = items.len();
    let done = AtomicUsize::new(0);
    items
        .par_iter()
        .map(|(input, job, output)| {
            if cancelled() {
                return FileResult {
                    input: input.clone(),
                    outcome: Err(CANCELLED.into()),
                };
            }
            let outcome =
                isolate(|| process_file_to(input, job, output).map_err(|e| e.to_string()));
            let result = FileResult {
                input: input.clone(),
                outcome,
            };
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            on_progress(Progress {
                done: n,
                total,
                last: result.clone(),
            });
            result
        })
        .collect()
}

/// What a finished batch amounts to: how many files succeeded, and the bytes
/// in versus out for those (so the GUI can say "18.4 MB to 7.0 MB, 62%
/// smaller"). Failed and cancelled files count in `failed` / `cancelled` and
/// contribute no bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchSummary {
    pub ok: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

impl BatchSummary {
    pub fn total(&self) -> usize {
        self.ok + self.failed + self.cancelled
    }

    /// Size change of the successful files as a signed percentage (-62 = 62%
    /// smaller), or `None` with nothing to compare.
    pub fn percent_change(&self) -> Option<i64> {
        if self.bytes_in == 0 {
            return None;
        }
        let delta = self.bytes_out as i128 - self.bytes_in as i128;
        Some((delta * 100 / self.bytes_in as i128) as i64)
    }

    /// One line for a dialog: "18.4 MB to 7.0 MB, 62% smaller".
    pub fn size_line(&self) -> String {
        match self.percent_change() {
            None => String::new(),
            Some(p) if p < 0 => format!(
                "{} to {}, {}% smaller",
                human_bytes(self.bytes_in),
                human_bytes(self.bytes_out),
                -p
            ),
            Some(0) => format!(
                "{} to {}, same size",
                human_bytes(self.bytes_in),
                human_bytes(self.bytes_out)
            ),
            Some(p) => format!(
                "{} to {}, {}% larger",
                human_bytes(self.bytes_in),
                human_bytes(self.bytes_out),
                p
            ),
        }
    }
}

/// Tally a batch's results, reading the input and output sizes from disk.
pub fn summarize(results: &[FileResult]) -> BatchSummary {
    let mut s = BatchSummary::default();
    for r in results {
        match &r.outcome {
            Ok(out) => {
                s.ok += 1;
                s.bytes_in += std::fs::metadata(&r.input).map(|m| m.len()).unwrap_or(0);
                s.bytes_out += std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
            }
            Err(e) if e == CANCELLED => s.cancelled += 1,
            Err(_) => s.failed += 1,
        }
    }
    s
}

/// "410 KB", "7.0 MB", "1.2 GB" (decimal units, one decimal from MB up).
pub fn human_bytes(n: u64) -> String {
    const KB: f64 = 1000.0;
    let f = n as f64;
    if f < KB {
        format!("{n} B")
    } else if f < KB * KB {
        format!("{:.0} KB", f / KB)
    } else if f < KB * KB * KB {
        format!("{:.1} MB", f / (KB * KB))
    } else {
        format!("{:.2} GB", f / (KB * KB * KB))
    }
}

/// "410 KB (-66%)" for one finished file, or "" when the sizes can't be read.
pub fn file_result_line(input: &std::path::Path, output: &std::path::Path) -> String {
    let (Ok(i), Ok(o)) = (std::fs::metadata(input), std::fs::metadata(output)) else {
        return String::new();
    };
    let (i, o) = (i.len(), o.len());
    if i == 0 {
        return human_bytes(o);
    }
    let pct = (o as i128 - i as i128) * 100 / i as i128;
    format!(
        "{} ({}{}%)",
        human_bytes(o),
        if pct > 0 { "+" } else { "" },
        pct
    )
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    #[test]
    fn human_bytes_picks_a_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(410_300), "410 KB");
        assert_eq!(human_bytes(7_040_000), "7.0 MB");
        assert_eq!(human_bytes(1_234_000_000), "1.23 GB");
    }

    #[test]
    fn summary_counts_and_compares_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let a_in = dir.path().join("a.png");
        let a_out = dir.path().join("a.webp");
        let b_in = dir.path().join("b.png");
        std::fs::write(&a_in, vec![0u8; 1000]).unwrap();
        std::fs::write(&a_out, vec![0u8; 380]).unwrap();
        std::fs::write(&b_in, vec![0u8; 500]).unwrap();
        let results = vec![
            FileResult {
                input: a_in.clone(),
                outcome: Ok(a_out.clone()),
            },
            FileResult {
                input: b_in.clone(),
                outcome: Err("boom".into()),
            },
            FileResult {
                input: b_in,
                outcome: Err(CANCELLED.into()),
            },
        ];
        let s = summarize(&results);
        assert_eq!((s.ok, s.failed, s.cancelled, s.total()), (1, 1, 1, 3));
        assert_eq!((s.bytes_in, s.bytes_out), (1000, 380));
        assert_eq!(s.percent_change(), Some(-62));
        assert_eq!(s.size_line(), "1 KB to 380 B, 62% smaller");
        assert_eq!(file_result_line(&a_in, &a_out), "380 B (-62%)");
        // Nothing succeeded: no size line, no percentage.
        let none = summarize(&results[1..]);
        assert_eq!(none.percent_change(), None);
        assert_eq!(none.size_line(), "");
    }
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
        RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255]))
            .save(&good)
            .unwrap();
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"not an image").unwrap();

        let job = Job {
            format: OutputFormat::Jpeg,
            ..Job::default()
        };
        let calls = AtomicUsize::new(0);
        let results = run_batch(&[good.clone(), bad.clone()], &job, |_p| {
            calls.fetch_add(1, Ordering::SeqCst);
        });

        assert_eq!(results.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let good_res = results.iter().find(|r| r.input == good).unwrap();
        let bad_res = results.iter().find(|r| r.input == bad).unwrap();
        assert!(good_res.outcome.is_ok());
        assert!(bad_res.outcome.is_err());
    }

    /// Once cancelled, no further input is started: the rest come back as
    /// `CANCELLED` with no progress call. A 2-thread pool bounds how many are
    /// already in flight when the flag flips.
    #[test]
    fn cancellation_skips_the_remaining_inputs() {
        use std::sync::atomic::AtomicBool;
        let dir = tempfile::tempdir().unwrap();
        let inputs: Vec<PathBuf> = (0..8)
            .map(|i| {
                let p = dir.path().join(format!("{i}.png"));
                RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255]))
                    .save(&p)
                    .unwrap();
                p
            })
            .collect();
        let job = Job {
            format: OutputFormat::Jpeg,
            ..Job::default()
        };
        let stop = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let results = pool.install(|| {
            run_batch_until(
                &inputs,
                &job,
                |_p| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    stop.store(true, Ordering::SeqCst); // cancel after the first finish
                },
                || stop.load(Ordering::SeqCst),
            )
        });
        assert_eq!(results.len(), 8);
        let skipped = results
            .iter()
            .filter(|r| r.outcome.as_ref().err().map(String::as_str) == Some(CANCELLED))
            .count();
        assert!(
            skipped >= 4,
            "most inputs skipped after cancel, got {skipped}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            8 - skipped,
            "no progress for skipped inputs"
        );
    }

    /// A panicking worker becomes an Err result — it must not unwind the batch
    /// (which used to discard every result and freeze GUI progress).
    #[test]
    fn panicking_worker_is_isolated() {
        let outcome = isolate(|| panic!("boom in codec"));
        let err = outcome.unwrap_err();
        assert!(err.contains("boom in codec"), "got: {err}");
        // And a normal closure passes through untouched.
        assert_eq!(
            isolate(|| Ok(PathBuf::from("x"))).unwrap(),
            PathBuf::from("x")
        );
    }

    /// Exporting into a chosen folder is the one batch path that could not be
    /// stopped: the other two take a cancel predicate and this did not, so a
    /// few thousand files had to run to the end.
    #[test]
    fn run_jobs_to_stops_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let job = Job {
            format: OutputFormat::Webp,
            ..Job::default()
        };
        let mut items = Vec::new();
        for n in 0..8 {
            let src = dir.path().join(format!("{n}.png"));
            RgbaImage::from_pixel(8, 8, Rgba([n as u8, 9, 9, 255]))
                .save(&src)
                .unwrap();
            items.push((src, job.clone(), dir.path().join(format!("out/{n}.webp"))));
        }

        let results = run_jobs_to_until(&items, |_| {}, || true);

        assert_eq!(results.len(), items.len(), "every item is accounted for");
        assert!(
            results
                .iter()
                .all(|r| matches!(&r.outcome, Err(e) if e == CANCELLED)),
            "a cancelled run converts nothing"
        );
        assert_eq!(
            summarize(&results).cancelled,
            items.len(),
            "and the summary says so"
        );
        assert!(
            !dir.path().join("out").exists(),
            "no output folder was created"
        );
    }

    /// Not cancelled behaves exactly as before.
    #[test]
    fn run_jobs_to_until_without_cancellation_converts_everything() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.png");
        RgbaImage::from_pixel(10, 10, Rgba([4, 5, 6, 255]))
            .save(&src)
            .unwrap();
        let out = dir.path().join("out").join("a.webp");
        let items = vec![(
            src,
            Job {
                format: OutputFormat::Webp,
                ..Job::default()
            },
            out.clone(),
        )];
        let results = run_jobs_to_until(&items, |_| {}, || false);
        assert!(results[0].outcome.is_ok(), "{:?}", results[0].outcome);
        assert!(out.exists());
    }

    /// The GUI's real path: explicit targets, parents created, progress per
    /// item, failures isolated.
    #[test]
    fn run_jobs_to_writes_each_items_target() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        RgbaImage::from_pixel(12, 12, Rgba([9, 9, 9, 255]))
            .save(&a)
            .unwrap();
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"nope").unwrap();
        let out_a = dir.path().join("out").join("nested").join("a.webp");
        let out_bad = dir.path().join("out").join("bad.webp");
        let job = Job {
            format: OutputFormat::Webp,
            ..Job::default()
        };
        let items = vec![
            (a.clone(), job.clone(), out_a.clone()),
            (bad.clone(), job, out_bad.clone()),
        ];
        let calls = AtomicUsize::new(0);
        let results = run_jobs_to(&items, |p| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(p.total, 2);
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let ra = results.iter().find(|r| r.input == a).unwrap();
        assert_eq!(ra.outcome.as_ref().unwrap(), &out_a);
        assert!(out_a.exists(), "parents created, file written");
        let rb = results.iter().find(|r| r.input == bad).unwrap();
        assert!(rb.outcome.is_err());
        assert!(!out_bad.exists());
    }

    #[test]
    fn run_jobs_uses_each_files_own_job() {
        use crate::format::OutputFormat;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        RgbaImage::from_pixel(20, 20, Rgba([5, 5, 5, 255]))
            .save(&a)
            .unwrap();
        RgbaImage::from_pixel(20, 20, Rgba([5, 5, 5, 255]))
            .save(&b)
            .unwrap();
        let items = vec![
            (
                a.clone(),
                Job {
                    format: OutputFormat::Jpeg,
                    ..Job::default()
                },
            ),
            (
                b.clone(),
                Job {
                    format: OutputFormat::Webp,
                    ..Job::default()
                },
            ),
        ];
        let results = run_jobs(&items, |_p| {});
        let ra = results
            .iter()
            .find(|r| r.input == a)
            .unwrap()
            .outcome
            .as_ref()
            .unwrap();
        let rb = results
            .iter()
            .find(|r| r.input == b)
            .unwrap()
            .outcome
            .as_ref()
            .unwrap();
        assert_eq!(ra.extension().unwrap(), "jpg");
        assert_eq!(rb.extension().unwrap(), "webp");
    }
}
