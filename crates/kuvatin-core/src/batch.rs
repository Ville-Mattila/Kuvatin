use crate::crop::compute_crop_rect;
use crate::format::OutputFormat;
use crate::pipeline::{process_file, process_file_to, Job, PngOptimize, WebpMode};
use crate::resize::compute_target_dimensions;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

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

/// Floor of the batch memory budget: what a failed memory query or a small VM
/// still gets, so a batch slows to one file at a time rather than stopping.
pub const MIN_MEMORY_BUDGET: u64 = 1 << 30;

/// The budget until the app sets one from the machine's RAM.
pub const DEFAULT_MEMORY_BUDGET: u64 = 4 << 30;

static MEMORY_BUDGET: AtomicU64 = AtomicU64::new(DEFAULT_MEMORY_BUDGET);

/// The budget for a machine with `total_physical` bytes of RAM: half of it, so
/// the rest of the desktop keeps working, and never under [`MIN_MEMORY_BUDGET`].
pub fn budget_for_ram(total_physical: u64) -> u64 {
    (total_physical / 2).max(MIN_MEMORY_BUDGET)
}

/// Set the memory budget every later batch runs under. The app calls this once
/// at startup with [`budget_for_ram`] of the machine.
pub fn set_memory_budget(bytes: u64) {
    MEMORY_BUDGET.store(bytes.max(MIN_MEMORY_BUDGET), Ordering::Relaxed);
}

fn memory_budget() -> u64 {
    MEMORY_BUDGET.load(Ordering::Relaxed)
}

/// Peak memory of converting one file, as a multiple of its RGBA8 working
/// buffer (width x height x 4).
///
/// Measured, not guessed: the peak private commit of one release-build process
/// per case, converting a 4000x3000 photo (48 MB as RGBA8), 2026-09-13. JPEG
/// 38 MB, lossy WebP 131, lossless WebP 362, plain PNG 113, lossless PNG 559,
/// lossy PNG 483, BMP 137, TIFF 133, GIF 117 — each rounded up to a whole
/// multiple. The PNG figures include oxipng's parallel trials. Unthrottled,
/// sixteen of those photos as lossless PNG peaked at 6.8 GB on 24 threads.
fn peak_factor(job: &Job) -> u64 {
    match job.format {
        OutputFormat::Jpeg => 1,
        OutputFormat::Webp => match job.webp {
            WebpMode::Lossy => 3,
            WebpMode::Lossless => 8,
        },
        OutputFormat::Png => match job.png {
            PngOptimize::None => 3,
            PngOptimize::Lossless => 12,
            PngOptimize::Lossy => 11,
        },
        OutputFormat::Bmp | OutputFormat::Tiff | OutputFormat::Gif => 3,
    }
}

/// What converting `input` costs against the memory budget, in bytes: its
/// largest pixel buffer (the source, or an upscale's output) times
/// [`peak_factor`]. Reads the header only — 0.22 ms for a 4000x3000 JPEG,
/// against 29 ms to decode it. A file whose header can't be read costs 0: its
/// decode fails straight away and allocates nothing.
pub fn estimate_cost(input: &Path, job: &Job) -> u64 {
    use image::ImageDecoder;
    let Some(decoder) = image::ImageReader::open(input)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.into_decoder().ok())
    else {
        return 0;
    };
    let (w, h) = decoder.dimensions();
    // A 16-bit source keeps 8 bytes a pixel through plain PNG and TIFF.
    let bytes_per_pixel = u64::from(decoder.color_type().bytes_per_pixel()).max(4);
    let (_, _, cw, ch) = compute_crop_rect(job.crop, w, h);
    let (tw, th) = compute_target_dimensions(job.resize, cw, ch);
    let pixels = (u64::from(w) * u64::from(h)).max(u64::from(tw) * u64::from(th));
    pixels * bytes_per_pixel * peak_factor(job)
}

/// Bytes of budget in use, and a signal for when some come back.
struct Budget {
    total: u64,
    used: Mutex<u64>,
    freed: Condvar,
}

impl Budget {
    /// Wait until `cost` fits beside what is running — or nothing is running,
    /// so an item bigger than the whole budget still gets its turn, alone —
    /// then take it. `false` if cancelled first.
    fn take<C: Fn() -> bool>(&self, cost: u64, cancelled: &C) -> bool {
        loop {
            if cancelled() {
                return false;
            }
            {
                let mut used = self.used.lock().unwrap();
                if *used == 0 || *used + cost <= self.total {
                    *used += cost;
                    return true;
                }
            }
            // On a pool thread, run a queued job instead of sleeping: with one
            // thread, the job being waited for is queued right here.
            if let Some(rayon::Yield::Executed) = rayon::yield_now() {
                continue;
            }
            let used = self.used.lock().unwrap();
            if *used != 0 && *used + cost > self.total {
                // Bounded, so a cancel is noticed within this long.
                let _ = self
                    .freed
                    .wait_timeout(used, Duration::from_millis(20))
                    .unwrap();
            }
        }
    }

    fn give_back(&self, cost: u64) {
        *self.used.lock().unwrap() -= cost;
        self.freed.notify_all();
    }
}

/// Gives an admitted item's budget back when its task ends, however it ends.
struct Admitted<'a> {
    budget: &'a Budget,
    cost: u64,
}

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        self.budget.give_back(self.cost);
    }
}

/// Run `work` over `items` on the rayon pool, starting each only once its
/// `cost_of` fits under `budget` beside what is already running. Results come
/// back in input order, `None` for an item that never started because
/// `cancelled()` turned true first.
///
/// Admission happens here, on the one thread handing out work, and never
/// inside a worker. A worker waiting for budget could be the very thread that
/// holds it: while a thread waits on its own nested tasks (oxipng's parallel
/// trials), rayon lets it pick up the next top-level item, which would then
/// wait forever for budget its own thread has.
fn admit_all<T, R, K, C, W>(
    items: &[T],
    cost_of: K,
    budget: u64,
    cancelled: C,
    work: W,
) -> Vec<Option<R>>
where
    T: Sync,
    R: Send,
    K: Fn(&T) -> u64 + Sync,
    C: Fn() -> bool + Sync,
    W: Fn(&T) -> R + Sync,
{
    let budget = Budget {
        total: budget.max(1),
        used: Mutex::new(0),
        freed: Condvar::new(),
    };
    let slots: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    rayon::scope(|scope| {
        for (item, slot) in items.iter().zip(&slots) {
            let cost = cost_of(item).min(budget.total);
            if !budget.take(cost, &cancelled) {
                break;
            }
            let (budget, work, cancelled) = (&budget, &work, &cancelled);
            scope.spawn(move |_| {
                let _admitted = Admitted { budget, cost };
                if !cancelled() {
                    *slot.lock().unwrap() = Some(work(item));
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| slot.into_inner().unwrap())
        .collect()
}

/// The loop every public entry point shares: admission under the memory
/// budget, panic isolation, one progress call per finished file, and
/// [`CANCELLED`] for whatever never started.
fn run_governed<T, F, C>(
    items: &[T],
    input_of: impl Fn(&T) -> &PathBuf + Sync,
    cost_of: impl Fn(&T) -> u64 + Sync,
    process: impl Fn(&T) -> Result<PathBuf, String> + Sync,
    on_progress: F,
    cancelled: C,
) -> Vec<FileResult>
where
    T: Sync,
    F: Fn(Progress) + Sync,
    C: Fn() -> bool + Sync,
{
    let total = items.len();
    let done = AtomicUsize::new(0);
    let finished = admit_all(items, cost_of, memory_budget(), cancelled, |item| {
        let result = FileResult {
            input: input_of(item).clone(),
            outcome: isolate(|| process(item)),
        };
        let n = done.fetch_add(1, Ordering::SeqCst) + 1;
        on_progress(Progress {
            done: n,
            total,
            last: result.clone(),
        });
        result
    });
    items
        .iter()
        .zip(finished)
        .map(|(item, result)| {
            result.unwrap_or_else(|| FileResult {
                input: input_of(item).clone(),
                outcome: Err(CANCELLED.into()),
            })
        })
        .collect()
}

/// Run `job` over every input in parallel, as many at once as the memory
/// budget allows (see [`set_memory_budget`]). `on_progress` is called once per
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
    run_governed(
        items,
        |(input, _)| input,
        |(input, job)| estimate_cost(input, job),
        |(input, job)| process_file(input, job).map_err(|e| e.to_string()),
        on_progress,
        cancelled,
    )
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
    run_governed(
        items,
        |(input, _, _)| input,
        |(input, job, _)| estimate_cost(input, job),
        |(input, job, output)| process_file_to(input, job, output).map_err(|e| e.to_string()),
        on_progress,
        cancelled,
    )
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
    use image::{Rgba, RgbaImage};
    use rayon::prelude::*;
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
    /// Run `f` on its own thread, and fail rather than hang the whole suite if
    /// it has not come back within ten seconds.
    fn within_ten_seconds<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("deadlocked: still running after ten seconds")
    }

    /// The point of the governor: the cost of what is running at once never
    /// passes the budget — and it still runs in parallel under it.
    #[test]
    fn the_budget_bounds_what_runs_at_once() {
        use std::sync::atomic::AtomicU64;
        let in_flight = AtomicU64::new(0);
        let most = AtomicU64::new(0);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();
        let items: Vec<u64> = vec![30; 12];
        let out = pool.install(|| {
            admit_all(
                &items,
                |c| *c,
                100,
                || false,
                |c| {
                    let now = in_flight.fetch_add(*c, Ordering::SeqCst) + *c;
                    most.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(15));
                    in_flight.fetch_sub(*c, Ordering::SeqCst);
                    *c
                },
            )
        });
        assert_eq!(out.iter().flatten().count(), 12, "everything ran");
        let most = most.load(Ordering::SeqCst);
        assert!(
            most <= 100,
            "never more than the budget in flight, saw {most}"
        );
        assert!(most >= 60, "and still in parallel under it, saw {most}");
    }

    /// A file bigger than the whole budget is not refused — a 100-megapixel
    /// scan on a small machine is still a file the user asked for. It waits
    /// for the pool to empty and then runs with nothing beside it.
    #[test]
    fn an_item_bigger_than_the_whole_budget_runs_alone() {
        let running = AtomicUsize::new(0);
        let alone = std::sync::atomic::AtomicBool::new(true);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();
        let items: Vec<u64> = vec![40, 40, 500, 40, 40];
        let out = pool.install(|| {
            admit_all(
                &items,
                |c| *c,
                100,
                || false,
                |c| {
                    let n = running.fetch_add(1, Ordering::SeqCst) + 1;
                    if *c == 500 && n != 1 {
                        alone.store(false, Ordering::SeqCst);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(15));
                    if *c == 500 && running.load(Ordering::SeqCst) != 1 {
                        alone.store(false, Ordering::SeqCst);
                    }
                    running.fetch_sub(1, Ordering::SeqCst);
                },
            )
        });
        assert_eq!(out.iter().flatten().count(), 5, "everything ran");
        assert!(
            alone.load(Ordering::SeqCst),
            "nothing else ran beside the oversized item"
        );
    }

    /// With one pool thread, the job being waited for is queued on the very
    /// thread doing the waiting. Blocking there would wait forever.
    #[test]
    fn a_single_thread_pool_does_not_deadlock_waiting_for_budget() {
        let out = within_ten_seconds(|| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap();
            let items: Vec<u64> = vec![60; 6];
            pool.install(|| admit_all(&items, |c| *c, 100, || false, |c| *c))
        });
        assert_eq!(out.into_iter().flatten().count(), 6);
    }

    /// What oxipng does: parallel work inside an item. A pool thread waiting
    /// on its own sub-tasks may pick up the next top-level item, which then
    /// waits for budget that same thread holds — the deadlock a governor inside
    /// the workers would have.
    #[test]
    fn parallel_work_inside_an_item_does_not_deadlock() {
        let out = within_ten_seconds(|| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .unwrap();
            let items: Vec<u64> = vec![100; 16];
            pool.install(|| {
                admit_all(
                    &items,
                    |c| *c,
                    100,
                    || false,
                    |_| (0..20_000u64).into_par_iter().map(|x| x % 7).sum::<u64>(),
                )
            })
        });
        assert_eq!(out.into_iter().flatten().count(), 16);
    }

    /// Cancel arrives while the rest are queued behind the budget: they must
    /// not start once it frees up, and they report as never started.
    #[test]
    fn cancelling_while_waiting_for_budget_starts_nothing_more() {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let ran = AtomicUsize::new(0);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let items: Vec<u64> = vec![100; 6];
        let out = pool.install(|| {
            admit_all(
                &items,
                |c| *c,
                100,
                || stop.load(Ordering::SeqCst),
                |_| {
                    ran.fetch_add(1, Ordering::SeqCst);
                    stop.store(true, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                },
            )
        });
        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "only the item already running"
        );
        assert_eq!(
            out.iter().filter(|o| o.is_none()).count(),
            5,
            "the rest report as never started"
        );
    }

    /// A file is priced at its largest working buffer — the source, or the
    /// output of an upscale — times what its encoder was measured to need.
    #[test]
    fn a_file_costs_its_largest_buffer_times_the_measured_factor() {
        use crate::pipeline::PngOptimize;
        use crate::resize::ResizeMode;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.png");
        RgbaImage::from_pixel(100, 50, Rgba([1, 2, 3, 255]))
            .save(&src)
            .unwrap();
        let jpeg = Job {
            format: OutputFormat::Jpeg,
            ..Job::default()
        };
        assert_eq!(
            estimate_cost(&src, &jpeg),
            100 * 50 * 4 * peak_factor(&jpeg)
        );

        let lossless_png = Job {
            format: OutputFormat::Png,
            png: PngOptimize::Lossless,
            ..Job::default()
        };
        assert!(
            estimate_cost(&src, &lossless_png) > estimate_cost(&src, &jpeg),
            "oxipng was measured at many times a JPEG encode"
        );

        let upscale = Job {
            format: OutputFormat::Jpeg,
            resize: ResizeMode::Pixels {
                width: Some(400),
                height: Some(200),
                keep_aspect: false,
            },
            ..Job::default()
        };
        assert_eq!(
            estimate_cost(&src, &upscale),
            400 * 200 * 4 * peak_factor(&upscale)
        );

        // Unreadable: costs nothing to admit, because its decode fails at once.
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"nope").unwrap();
        assert_eq!(estimate_cost(&bad, &jpeg), 0);
    }

    /// Half the machine, with a floor so a failed memory query (which reports
    /// 0) or a tiny VM still converts.
    #[test]
    fn the_budget_is_half_the_machine_but_never_under_the_floor() {
        const GIB: u64 = 1 << 30;
        assert_eq!(budget_for_ram(32 * GIB), 16 * GIB);
        assert_eq!(budget_for_ram(8 * GIB), 4 * GIB);
        assert_eq!(budget_for_ram(GIB), MIN_MEMORY_BUDGET);
        assert_eq!(budget_for_ram(0), MIN_MEMORY_BUDGET);
    }

    /// The scheduler is no longer a plain ordered `par_iter`, so pin what it
    /// replaced: results line up with the inputs.
    #[test]
    fn results_come_back_in_input_order() {
        let dir = tempfile::tempdir().unwrap();
        let inputs: Vec<PathBuf> = (0..20)
            .map(|i| {
                let p = dir.path().join(format!("{i}.png"));
                RgbaImage::from_pixel(8 + i, 8, Rgba([1, 2, 3, 255]))
                    .save(&p)
                    .unwrap();
                p
            })
            .collect();
        let job = Job {
            format: OutputFormat::Jpeg,
            ..Job::default()
        };
        let results = run_batch(&inputs, &job, |_| {});
        let order: Vec<&PathBuf> = results.iter().map(|r| &r.input).collect();
        assert_eq!(order, inputs.iter().collect::<Vec<_>>());
    }
}
