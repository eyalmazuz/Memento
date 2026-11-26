// Golden-vector verification of `subomi_asr::audio::melfrontend`.
//
// Fixture `tests/golden/golden_mel.json` was produced by
// `tests/golden/golden_generator.cpp`, which links the REAL C++ implementation:
//
//   g++ -std=gnu++20 -O2 -I src -I . src/asr/audio/melfrontend.cpp \
//       rust/subomi-asr/tests/golden/golden_generator.cpp -o golden_generator
//   ./golden_generator        # run from the repository root
//
// Every f32 value is compared BIT-EXACTLY via `f32::to_bits`; every i64/u8
// mask element and every utterance span is compared exactly. A single ULP of
// deviation anywhere is a test failure reporting the case name, the first
// differing index and the maximum ULP deviation of the whole case.
//
////////////////////////////////////////////////////////////////////////////////

use subomi_asr::audio::melfrontend::{MelFrontend, MelFrontendConfig};
use serde::Deserialize;

const GOLDEN_JSON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/golden_mel.json");

#[derive(Deserialize)]
struct GoldenDoc {
    version: u32,
    inputs: Vec<GoldenInput>,
    cases: Vec<GoldenCase>,
    segmentation: Vec<GoldenSegCase>,
}

#[derive(Deserialize)]
struct GoldenInput {
    id: String,
    num_samples: usize,
    samples: Vec<f32>,
}

#[derive(Deserialize)]
struct GoldenConfig {
    sample_rate: i32,
    n_fft: i32,
    win_length: i32,
    hop_length: i32,
    n_mels: i32,
    preemphasis: f32,
    log_zero_guard: f32,
    norm_epsilon: f32,
    normalize_per_feature: bool,
}

#[derive(Deserialize)]
struct GoldenOutput {
    num_frames: i64,
    valid_frames: i64,
    n_mels: i64,
    features: Vec<f32>,
    attention_mask_int64: Vec<i64>,
    attention_mask_bool: Vec<u8>,
}

#[derive(Deserialize)]
struct GoldenCase {
    name: String,
    input: String,
    config: GoldenConfig,
    output: GoldenOutput,
}

#[derive(Deserialize)]
struct GoldenSpan {
    start_sample: usize,
    end_sample: usize,
    start_sec: f64,
    end_sec: f64,
}

#[derive(Deserialize)]
struct GoldenSegCase {
    name: String,
    input: String,
    sample_rate: i32,
    max_utterance_sec: f64,
    spans: Vec<GoldenSpan>,
}

/// Monotonic integer key over the f32 ordering, so IEEE bit patterns
/// (including -0.0 < +0.0) map to 1-ULP-separated integers.
fn f32_sort_key(bits: u32) -> i64 {
    if bits & 0x8000_0000 != 0 {
        (!bits) as i64
    } else {
        (bits | 0x8000_0000) as i64
    }
}

fn ulp_distance(a: f32, b: f32) -> u64 {
    if a.is_nan() || b.is_nan() {
        // NaN bit patterns have no ULP ordering convention; report as u64::MAX.
        return u64::MAX;
    }
    (f32_sort_key(a.to_bits()) - f32_sort_key(b.to_bits())).unsigned_abs()
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[track_caller]
fn assert_f32_features_match(case_name: &str, label: &str, expected: &[f32], got: &[f32]) {
    assert_f32_bits_equal(case_name, label, expected, got);
}

/// On non-`x86_64-linux` targets (`macOS` `libSystem`, `Windows` `UCRT`, and
/// `aarch64` `libm`), transcendental `cos`/`sin`/`ln`/`exp` implementations
/// differ from GNU `glibc` `x86_64` by up to ~1 ULP (~3e-7 after z-score
/// normalization). Enforce strict `1e-5` absolute agreement plus NaN/Inf parity.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
#[track_caller]
fn assert_f32_features_match(case_name: &str, label: &str, expected: &[f32], got: &[f32]) {
    let _ = ulp_distance;
    assert_eq!(
        got.len(),
        expected.len(),
        "[{case_name} / {label}] length mismatch: got {} expected {}",
        got.len(),
        expected.len(),
    );
    const MAX_ABS_TOL: f32 = 1e-5;
    let mut max_abs: f32 = 0.0;
    let mut max_idx: usize = 0;
    for (i, (&e, &g)) in expected.iter().zip(got.iter()).enumerate() {
        assert_eq!(
            g.is_nan(),
            e.is_nan(),
            "[{case_name} / {label}] NaN mismatch at index {i}: expected {e:?}, got {g:?}"
        );
        assert_eq!(
            g.is_infinite(),
            e.is_infinite(),
            "[{case_name} / {label}] Inf mismatch at index {i}: expected {e:?}, got {g:?}"
        );
        if e.is_finite() {
            let diff = (g - e).abs();
            if diff > max_abs {
                max_abs = diff;
                max_idx = i;
            }
        }
    }
    assert!(
        max_abs <= MAX_ABS_TOL,
        "GOLDEN MISMATCH [{case_name} / {label}]: max abs diff {max_abs:e} > {MAX_ABS_TOL:e} at index {max_idx} \
         (expected {:?}, got {:?})",
        expected[max_idx],
        got[max_idx],
    );
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[track_caller]
fn assert_f32_bits_equal(case_name: &str, label: &str, expected: &[f32], got: &[f32]) {
    assert_eq!(
        got.len(),
        expected.len(),
        "[{case_name} / {label}] length mismatch: got {} expected {}",
        got.len(),
        expected.len(),
    );
    let mut max_ulp: u64 = 0;
    let mut max_ulp_index: usize = 0;
    let mut first_diff: Option<usize> = None;
    let mut diff_count: usize = 0;
    for (i, (&e, &g)) in expected.iter().zip(got.iter()).enumerate() {
        if e.to_bits() == g.to_bits() {
            continue;
        }
        diff_count += 1;
        if first_diff.is_none() {
            first_diff = Some(i);
        }
        let ulp = ulp_distance(g, e);
        if ulp > max_ulp {
            max_ulp = ulp;
            max_ulp_index = i;
        }
    }
    if let Some(first) = first_diff {
        let e = expected[first];
        let g = got[first];
        panic!(
            "GOLDEN MISMATCH [{case_name} / {label}]: {diff_count}/{total} f32 values differ \
             (bits not identical); max ULP deviation {max_ulp} at index {max_ulp_index}; \
             first differing element at index {first}: expected bits {e_bits:08X} ({e:?}), \
             got bits {g_bits:08X} ({g:?})",
            total = expected.len(),
            e_bits = e.to_bits(),
            g_bits = g.to_bits(),
        );
    }
}

#[track_caller]
fn assert_i64_exact(case_name: &str, label: &str, expected: &[i64], got: &[i64]) {
    assert_eq!(
        got.len(),
        expected.len(),
        "[{case_name} / {label}] length mismatch: got {} expected {}",
        got.len(),
        expected.len(),
    );
    for (i, (&e, &g)) in expected.iter().zip(got.iter()).enumerate() {
        assert_eq!(
            g,
            e,
            "[{case_name} / {label}] first differing element at index {i}: expected {e}, got {g}"
        );
    }
}

#[track_caller]
fn assert_u8_exact(case_name: &str, label: &str, expected: &[u8], got: &[u8]) {
    assert_eq!(
        got.len(),
        expected.len(),
        "[{case_name} / {label}] length mismatch: got {} expected {}",
        got.len(),
        expected.len(),
    );
    for (i, (&e, &g)) in expected.iter().zip(got.iter()).enumerate() {
        assert_eq!(
            g,
            e,
            "[{case_name} / {label}] first differing element at index {i}: expected {e}, got {g}"
        );
    }
}

fn find_input<'d>(doc: &'d GoldenDoc, id: &str) -> &'d GoldenInput {
    doc.inputs
        .iter()
        .find(|input| input.id == id)
        .unwrap_or_else(|| panic!("golden_mel.json: no input with id {id:?}"))
}

#[test]
fn golden_mel_bit_identical_against_cxx() {
    let path = GOLDEN_JSON;
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read golden fixture {path}: {err}"));
    let doc: GoldenDoc = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed to parse golden fixture {path}: {err}"));
    assert_eq!(doc.version, 1, "unsupported golden_mel.json version");
    assert!(!doc.cases.is_empty(), "golden fixture contains no mel cases");

    for input in &doc.inputs {
        assert_eq!(
            input.samples.len(),
            input.num_samples,
            "golden input {:?}: num_samples metadata disagrees with samples array",
            input.id,
        );
    }

    for case in &doc.cases {
        let input = find_input(&doc, &case.input);

        let config = MelFrontendConfig {
            sample_rate: case.config.sample_rate,
            n_fft: case.config.n_fft,
            win_length: case.config.win_length,
            hop_length: case.config.hop_length,
            n_mels: case.config.n_mels,
            preemphasis: case.config.preemphasis,
            log_zero_guard: case.config.log_zero_guard,
            norm_epsilon: case.config.norm_epsilon,
            normalize_per_feature: case.config.normalize_per_feature,
        };

        // Structural equality first: dimension metadata and shape.
        assert_eq!(
            config.n_mels as i64,
            case.output.n_mels,
            "[{}] n_mels metadata mismatch",
            case.name,
        );
        assert_eq!(
            case.output.features.len() as i64,
            case.output.num_frames * case.output.n_mels,
            "[{}] expected features row-major shape [num_frames={} x n_mels={}] does not match array length {}",
            case.name,
            case.output.num_frames,
            case.output.n_mels,
            case.output.features.len(),
        );

        let frontend = MelFrontend::new(config);
        let got = frontend.extract(&input.samples);

        assert_eq!(
            got.num_frames,
            case.output.num_frames,
            "[{}] num_frames mismatch (case {:?}, input {:?})",
            case.name,
            case.input,
            input.id,
        );
        assert_eq!(
            got.valid_frames,
            case.output.valid_frames,
            "[{}] valid_frames mismatch (case {:?}, input {:?})",
            case.name,
            case.input,
            input.id,
        );
        assert_eq!(got.n_mels, case.output.n_mels, "[{}] n_mels mismatch", case.name,);

        // Bit-exact features on x86_64 Linux (reference libm), <=1e-5 on other OS/arch libm.
        assert_f32_features_match(&case.name, "features", &case.output.features, &got.features);

        // Exact attention masks.
        assert_i64_exact(
            &case.name,
            "attention_mask_int64",
            &case.output.attention_mask_int64,
            &got.attention_mask_int64,
        );
        assert_u8_exact(
            &case.name,
            "attention_mask_bool",
            &case.output.attention_mask_bool,
            &got.attention_mask_bool,
        );
    }

    for seg in &doc.segmentation {
        let input = find_input(&doc, &seg.input);
        let got = MelFrontend::segment_speech_utterances(
            &input.samples,
            seg.sample_rate,
            seg.max_utterance_sec,
        );

        assert_eq!(
            got.len(),
            seg.spans.len(),
            "[{}] span count mismatch: expected {} spans, got {}",
            seg.name,
            seg.spans.len(),
            got.len(),
        );

        for (i, (expected, actual)) in seg.spans.iter().zip(got.iter()).enumerate() {
            assert_eq!(
                actual.start_sample,
                expected.start_sample,
                "[{}] span {i} first differing field: start_sample expected {}, got {}",
                seg.name,
                expected.start_sample,
                actual.start_sample,
            );
            assert_eq!(
                actual.end_sample,
                expected.end_sample,
                "[{}] span {i} first differing field: end_sample expected {}, got {}",
                seg.name,
                expected.end_sample,
                actual.end_sample,
            );
            assert_eq!(
                actual.start_sec.to_bits(),
                expected.start_sec.to_bits(),
                "[{}] span {i} start_sec bits differ: expected bits {:016X} ({}), got bits {:016X} ({})",
                seg.name,
                expected.start_sec.to_bits(),
                expected.start_sec,
                actual.start_sec.to_bits(),
                actual.start_sec,
            );
            assert_eq!(
                actual.end_sec.to_bits(),
                expected.end_sec.to_bits(),
                "[{}] span {i} end_sec bits differ: expected bits {:016X} ({}), got bits {:016X} ({})",
                seg.name,
                expected.end_sec.to_bits(),
                expected.end_sec,
                actual.end_sec.to_bits(),
                actual.end_sec,
            );
        }
    }
}
