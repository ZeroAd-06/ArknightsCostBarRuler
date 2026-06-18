use std::{
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    time::Instant,
};

use ruler_core::{analysis::scanner::detect_battle_state, BattleState, PixelFormat};

struct Fixture {
    path: PathBuf,
    expected: BattleState,
    /// Blurry settings-transition frames that may legitimately read as either
    /// `OneXRunning` or `NotInBattle` (marked with a `1xorGarbage` name prefix).
    one_x_or_garbage: bool,
}

struct Sample {
    path: PathBuf,
    width: u32,
    height: u32,
    data: Vec<u8>,
    expected: BattleState,
}

#[test]
fn battle_button_detector_classifies_all_capture_samples() {
    let fixtures = load_fixture_paths();
    assert!(
        !fixtures.is_empty(),
        "expected PNG samples under tests/fixtures/battle_buttons"
    );

    let mut mismatches = Vec::new();
    for fixture in fixtures {
        let sample = load_sample(&fixture.path, fixture.expected);
        let actual =
            detect_battle_state(&sample.data, sample.width, sample.height, PixelFormat::Rgba);
        let accepted = actual == sample.expected
            || (fixture.one_x_or_garbage
                && matches!(actual, BattleState::OneXRunning | BattleState::NotInBattle));
        if !accepted {
            mismatches.push(format!(
                "{}: expected {:?}, got {:?}",
                sample.path.display(),
                sample.expected,
                actual
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "wrong battle state for:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn battle_button_detector_stays_under_30us_in_release() {
    if cfg!(debug_assertions) {
        return;
    }

    let fixtures = load_fixture_paths();
    assert!(
        !fixtures.is_empty(),
        "expected PNG samples under tests/fixtures/battle_buttons"
    );

    let iterations = 1_000u128;
    let mut worst_avg_ns = 0u128;
    let mut worst_path = PathBuf::new();

    for fixture in fixtures {
        let sample = load_sample(&fixture.path, fixture.expected);
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
        worst_avg_ns < 30_000,
        "battle button detector exceeded 30us: {:.2}us for {}",
        worst_avg_ns as f64 / 1000.0,
        worst_path.display()
    );
}

fn load_fixture_paths() -> Vec<Fixture> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("battle_buttons");
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

    let mut fixtures = Vec::new();
    for (dir_name, expected) in expected_dirs {
        let dir = root.join(dir_name);
        assert!(dir.is_dir(), "missing sample directory: {}", dir.display());
        let mut paths = fs::read_dir(&dir)
            .expect("failed to list battle button fixtures")
            .map(|entry| entry.expect("failed to read fixture entry").path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("png"))
            .collect::<Vec<_>>();
        paths.sort();

        for path in paths {
            let one_x_or_garbage = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("1xorGarbage"));
            fixtures.push(Fixture {
                path,
                expected,
                one_x_or_garbage,
            });
        }
    }

    fixtures
}

fn load_sample(path: &Path, expected: BattleState) -> Sample {
    let (frame_width, frame_height) = parse_frame_size(path);
    let image = image::ImageReader::open(path)
        .unwrap_or_else(|e| panic!("failed to open {}: {e}", path.display()))
        .decode()
        .unwrap_or_else(|e| panic!("failed to decode {}: {e}", path.display()))
        .to_rgba8();
    let (crop_width, crop_height) = image.dimensions();
    assert!(
        crop_width <= frame_width && crop_height <= frame_height,
        "fixture crop exceeds declared frame size: {}",
        path.display()
    );

    let crop = image.into_raw();
    let mut data = vec![0; frame_width as usize * frame_height as usize * 4];
    let dst_x = frame_width - crop_width;
    let dst_row_bytes = frame_width as usize * 4;
    let src_row_bytes = crop_width as usize * 4;
    for y in 0..crop_height as usize {
        let src = y * src_row_bytes;
        let dst = y * dst_row_bytes + dst_x as usize * 4;
        data[dst..dst + src_row_bytes].copy_from_slice(&crop[src..src + src_row_bytes]);
    }

    flip_rows(&mut data, frame_width, frame_height, 4);

    Sample {
        path: path.to_path_buf(),
        width: frame_width,
        height: frame_height,
        data,
        expected,
    }
}

fn parse_frame_size(path: &Path) -> (u32, u32) {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_else(|| panic!("invalid fixture file name: {}", path.display()));
    // The size is the trailing `<W>x<H>` of the segment before `__`, tolerating
    // an arbitrary text prefix such as `1xorGarbage`.
    let (prefix, _) = stem
        .split_once("__")
        .unwrap_or_else(|| panic!("fixture name must contain WIDTHxHEIGHT__: {stem}"));
    let chars: Vec<char> = prefix.chars().collect();
    let mut i = chars.len();
    while i > 0 && chars[i - 1].is_ascii_digit() {
        i -= 1;
    }
    assert!(
        i > 0 && i < chars.len() && chars[i - 1] == 'x',
        "fixture size must end with WIDTHxHEIGHT: {stem}"
    );
    let height: u32 = chars[i..]
        .iter()
        .collect::<String>()
        .parse()
        .unwrap_or_else(|e| panic!("invalid fixture height in {stem}: {e}"));
    let x = i - 1;
    let mut j = x;
    while j > 0 && chars[j - 1].is_ascii_digit() {
        j -= 1;
    }
    let width: u32 = chars[j..x]
        .iter()
        .collect::<String>()
        .parse()
        .unwrap_or_else(|e| panic!("invalid fixture width in {stem}: {e}"));
    (width, height)
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
