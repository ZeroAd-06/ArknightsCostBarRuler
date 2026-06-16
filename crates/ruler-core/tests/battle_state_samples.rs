use std::{
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    time::Instant,
};

use ruler_core::{analysis::scanner::detect_battle_state, BattleState, PixelFormat};

struct Sample {
    path: PathBuf,
    width: u32,
    height: u32,
    data: Vec<u8>,
    expected: BattleState,
}

#[test]
fn battle_button_detector_classifies_all_capture_samples() {
    let samples = load_capture_samples();
    assert!(
        !samples.is_empty(),
        "expected PNG samples under recordings/captures"
    );

    for sample in samples {
        let actual =
            detect_battle_state(&sample.data, sample.width, sample.height, PixelFormat::Rgba);
        assert_eq!(
            actual,
            sample.expected,
            "wrong battle state for {}",
            sample.path.display()
        );
    }
}

#[test]
fn battle_button_detector_stays_under_100us_in_release() {
    if cfg!(debug_assertions) {
        return;
    }

    let samples = load_capture_samples();
    assert!(
        !samples.is_empty(),
        "expected PNG samples under recordings/captures"
    );

    let iterations = 1_000u128;
    let mut worst_avg_ns = 0u128;
    let mut worst_path = PathBuf::new();

    for sample in &samples {
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(detect_battle_state(
                black_box(&sample.data),
                sample.width,
                sample.height,
                PixelFormat::Rgba,
            ));
        }
        let avg_ns = start.elapsed().as_nanos() / iterations;
        if avg_ns > worst_avg_ns {
            worst_avg_ns = avg_ns;
            worst_path = sample.path.clone();
        }
    }

    eprintln!(
        "battle button detector worst sample average: {:.2}us ({})",
        worst_avg_ns as f64 / 1000.0,
        worst_path.display()
    );
    assert!(
        worst_avg_ns < 100_000,
        "battle button detector exceeded 100us: {:.2}us for {}",
        worst_avg_ns as f64 / 1000.0,
        worst_path.display()
    );
}

fn load_capture_samples() -> Vec<Sample> {
    let root = workspace_root().join("recordings").join("captures");
    let expected_dirs = [
        ("0.2x", BattleState::PointTwoXRunning),
        ("0.2xpause", BattleState::PointTwoXPaused),
        ("1x", BattleState::OneXRunning),
        ("2x", BattleState::TwoXRunning),
        ("1xpause", BattleState::OneXPaused),
        ("2xpause", BattleState::TwoXPaused),
        ("init", BattleState::BeforeOrAfterBattle),
        ("garbage", BattleState::NotInBattle),
    ];

    let mut samples = Vec::new();
    for (dir_name, expected) in expected_dirs {
        let dir = root.join(dir_name);
        assert!(dir.is_dir(), "missing sample directory: {}", dir.display());
        for entry in fs::read_dir(&dir).expect("failed to list capture samples") {
            let entry = entry.expect("failed to read capture sample entry");
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("png") {
                continue;
            }
            samples.push(load_sample(&path, expected));
        }
    }

    samples
}

fn load_sample(path: &Path, expected: BattleState) -> Sample {
    let image = image::ImageReader::open(path)
        .unwrap_or_else(|e| panic!("failed to open {}: {e}", path.display()))
        .decode()
        .unwrap_or_else(|e| panic!("failed to decode {}: {e}", path.display()))
        .to_rgba8();
    let (width, height) = image.dimensions();
    let mut data = image.into_raw();
    flip_rows(&mut data, width, height, 4);

    Sample {
        path: path.to_path_buf(),
        width,
        height,
        data,
        expected,
    }
}

fn flip_rows(buf: &mut [u8], width: u32, height: u32, bpp: u32) {
    let row_bytes = (width * bpp) as usize;
    for row in 0..(height as usize / 2) {
        let top = row * row_bytes;
        let bottom = (height as usize - 1 - row) * row_bytes;
        let (left, right) = buf.split_at_mut(bottom);
        left[top..top + row_bytes].swap_with_slice(&mut right[..row_bytes]);
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}
