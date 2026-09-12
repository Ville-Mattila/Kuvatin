use image::{Rgba, RgbaImage};
use kuvatin_core::batch::run_batch;
use kuvatin_core::format::OutputFormat;
use kuvatin_core::pipeline::{Job, WebpMode};
use kuvatin_core::resize::ResizeMode;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn full_batch_resizes_converts_and_reports_failures() {
    let dir = tempfile::tempdir().unwrap();

    // three good inputs of differing sizes
    for (i, (w, h)) in [(800u32, 600u32), (1024, 768), (400, 400)]
        .iter()
        .enumerate()
    {
        let p = dir.path().join(format!("img{i}.png"));
        RgbaImage::from_pixel(*w, *h, Rgba([i as u8, 100, 200, 255]))
            .save(&p)
            .unwrap();
    }
    // one corrupt input
    let bad = dir.path().join("bad.png");
    std::fs::write(&bad, b"not a png").unwrap();

    let inputs: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();

    let job = Job {
        resize: ResizeMode::FitBox {
            width: 256,
            height: 256,
        },
        format: OutputFormat::Jpeg,
        quality: 85,
        ..Job::default()
    };

    let progress_calls = AtomicUsize::new(0);
    let results = run_batch(&inputs, &job, |_p| {
        progress_calls.fetch_add(1, Ordering::SeqCst);
    });

    assert_eq!(results.len(), inputs.len());
    assert_eq!(progress_calls.load(Ordering::SeqCst), inputs.len());

    let oks: Vec<_> = results.iter().filter(|r| r.outcome.is_ok()).collect();
    let errs: Vec<_> = results.iter().filter(|r| r.outcome.is_err()).collect();
    assert_eq!(oks.len(), 3);
    assert_eq!(errs.len(), 1);

    // every successful output exists, is a jpg, and fits within 256x256
    for r in oks {
        let out = r.outcome.as_ref().unwrap();
        assert!(out.exists());
        assert_eq!(out.extension().unwrap(), "jpg");
        let img = image::open(out).unwrap();
        assert!(img.width() <= 256 && img.height() <= 256);
    }
}

/// The whole way through: a screenshot-like PNG with transparency, run as a
/// real batch job, comes back out of the .webp byte-for-byte. This is the
/// claim "Lossless" makes in the Settings panel, and it is worth owning at the
/// level the app actually calls — the unit test encodes in memory, this one
/// goes through the file on disk.
#[test]
fn a_lossless_webp_job_writes_a_file_that_decodes_to_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("shot.png");

    // Flat blocks and a hard edge: what lossy WebP smears and lossless keeps.
    // The transparent band has a colour under it, which libwebp is free to
    // rewrite unless we ask it not to.
    let mut img = RgbaImage::new(64, 48);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = match (x / 16, y / 16) {
            (_, 0) => Rgba([255, 0, 0, 255]),
            (0, _) => Rgba([0, 128, 255, 255]),
            (_, 2) => Rgba([9, 200, 60, 0]), // invisible, but still a colour
            _ => Rgba([250, 250, 250, 255]),
        };
    }
    img.save(&src).unwrap();

    let job = Job {
        format: OutputFormat::Webp,
        webp: WebpMode::Lossless,
        quality: 5, // ignored: proves the slider does not reach this path
        ..Job::default()
    };
    let results = run_batch(std::slice::from_ref(&src), &job, |_p| {});
    let out = results[0].outcome.as_ref().expect("converted");
    assert_eq!(out.extension().unwrap(), "webp");

    let back = image::open(out).unwrap().to_rgba8();
    assert_eq!(back, img, "every pixel, alpha and the colour beneath it");
}
