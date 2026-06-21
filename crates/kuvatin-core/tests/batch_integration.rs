use kuvatin_core::batch::run_batch;
use kuvatin_core::format::OutputFormat;
use kuvatin_core::pipeline::Job;
use kuvatin_core::resize::ResizeMode;
use image::{Rgba, RgbaImage};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn full_batch_resizes_converts_and_reports_failures() {
    let dir = tempfile::tempdir().unwrap();

    // three good inputs of differing sizes
    for (i, (w, h)) in [(800u32, 600u32), (1024, 768), (400, 400)].iter().enumerate() {
        let p = dir.path().join(format!("img{i}.png"));
        RgbaImage::from_pixel(*w, *h, Rgba([i as u8, 100, 200, 255])).save(&p).unwrap();
    }
    // one corrupt input
    let bad = dir.path().join("bad.png");
    std::fs::write(&bad, b"not a png").unwrap();

    let inputs: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();

    let job = Job {
        resize: ResizeMode::FitBox { width: 256, height: 256 },
        format: OutputFormat::Jpeg,
        quality: 85,
        ..Job::default()
    };

    let progress_calls = AtomicUsize::new(0);
    let results = run_batch(&inputs, &job, "itest", |_p| {
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
