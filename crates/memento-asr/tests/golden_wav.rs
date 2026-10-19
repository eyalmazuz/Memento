////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////
//
// Golden-vector cross-verification of `memento_asr::audio::wavreader`
// against the C++ original (`src/asr/audio/wavreader.cpp`, where Qt does the
// IO only — the parse/decode kernel is plain C++).
//
// The fixture `tests/golden/golden.wav` was written by
// `tests/golden/golden_generator.cpp` with a malicious-but-valid RIFF layout:
//   * an unknown "JUNK" chunk (odd 5-byte payload + pad byte) BEFORE "data",
//   * a fully validated "fmt " chunk (PCM / mono / 16000 Hz / 2 B / 16 bit),
//   * an ODD "data" chunk (4021 bytes = 2010 samples + 1 ignored trailing
//     half-sample byte) followed by the mandatory pad byte.
//
// `tests/golden/golden_wav.json` carries the raw i16 payload and the expected
// decoded f32 samples, replicated from the C++ decode kernel:
//   little-endian u16 -> int16 (two's complement) -> f32 / 32768.0f
//
// All samples are compared BIT-EXACTLY via `f32::to_bits`.
//
////////////////////////////////////////////////////////////////////////////////

use memento_asr::audio::wavreader::{read_pcm16_wav, read_pcm16_wav_bytes};
use serde::Deserialize;

const GOLDEN_JSON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/golden_wav.json");
const GOLDEN_WAV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/golden.wav");
const EXPECTED_SAMPLE_RATE: u32 = 16000;

#[derive(Deserialize)]
struct WavDoc {
    version: u32,
    fixture_file: String,
    structure: Structure,
    raw_pcm_i16: Vec<i16>,
    expected_samples: Vec<f32>,
}

#[derive(Deserialize)]
struct Structure {
    expected_file_bytes: u64,
    riff_size_field: u32,
    fmt: FmtFields,
    chunks: Vec<Chunk>,
    data_chunk_size: u32,
    junk_before_data: bool,
    expected_num_samples: u64,
}

#[derive(Deserialize)]
struct FmtFields {
    format: u16,
    channels: u16,
    sample_rate: u32,
    byte_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
}

#[derive(Deserialize)]
struct Chunk {
    id: String,
    offset: u64,
    size: u32,
    odd_size: bool,
    pad_byte_after: bool,
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

#[track_caller]
fn assert_f32_bits_equal(label: &str, expected: &[f32], got: &[f32]) {
    assert_eq!(
        got.len(),
        expected.len(),
        "[{label}] length mismatch: got {} expected {}",
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
            "GOLDEN MISMATCH [{label}]: {diff_count}/{total} f32 values differ (bits not identical); \
             max ULP deviation {max_ulp} at index {max_ulp_index}; first differing element at index \
             {first}: expected bits {e_bits:08X} ({e:?}), got bits {g_bits:08X} ({g:?})",
            total = expected.len(),
            e_bits = e.to_bits(),
            g_bits = g.to_bits(),
        );
    }
}

#[test]
fn golden_wav_bit_identical_against_cxx() {
    let raw = std::fs::read_to_string(GOLDEN_JSON)
        .unwrap_or_else(|err| panic!("failed to read golden wav metadata {GOLDEN_JSON}: {err}"));
    let doc: WavDoc = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed to parse {GOLDEN_JSON}: {err}"));
    let bytes = std::fs::read(GOLDEN_WAV)
        .unwrap_or_else(|err| panic!("failed to read fixture {GOLDEN_WAV}: {err}"));

    assert_eq!(doc.version, 1, "unsupported golden_wav.json version");
    assert_eq!(doc.fixture_file, "golden.wav");

    // --- structural equality of the RIFF layout -------------------------
    assert_eq!(
        bytes.len() as u64,
        doc.structure.expected_file_bytes,
        "fixture file length disagrees with metadata",
    );
    assert_eq!(
        bytes.len(),
        4080,
        "fixture must be 12 header + 14 JUNK + 24 fmt + 4030 data",
    );
    let riff_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    assert_eq!(
        riff_size,
        doc.structure.riff_size_field,
        "RIFF size field disagrees",
    );
    assert_eq!(
        &bytes[0..4],
        b"RIFF",
        "fixture is not RIFF (generator defect)",
    );
    assert_eq!(&bytes[8..12], b"WAVE", "fixture is not WAVE");

    for (i, chunk) in doc.structure.chunks.iter().enumerate() {
        let offset = chunk.offset as usize;
        assert!(
            offset + 8 <= bytes.len(),
            "chunk {i} ({:?}) offset {} out of range",
            chunk.id,
            offset,
        );
        assert_eq!(
            &bytes[offset..offset + 4],
            chunk.id.as_bytes(),
            "chunk {i} magic mismatch at offset {offset}",
        );
        let chunk_size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]);
        assert_eq!(chunk_size, chunk.size, "chunk {i} ({:?}) size field mismatch", chunk.id);
        assert_eq!(
            chunk_size % 2 == 1,
            chunk.odd_size,
            "chunk {i} ({:?}) odd_size metadata mismatch",
            chunk.id,
        );
        // Odd chunks MUST carry a pad byte and consume size + size%2.
        assert_eq!(
            chunk_size % 2 == 1,
            chunk.pad_byte_after,
            "chunk {i} ({:?}) pad semantics mismatch",
            chunk.id,
        );
        let consumed = chunk_size as usize + (chunk_size % 2) as usize;
        let next_offset = match doc.structure.chunks.get(i + 1) {
            Some(next) => next.offset as usize,
            None => bytes.len(),
        };
        assert_eq!(
            offset + 8 + consumed,
            next_offset,
            "chunk {i} ({:?}) does not tile exactly up to the next chunk (pad walk)",
            chunk.id,
        );
    }
    assert_eq!(
        doc.structure.chunks[0].id,
        "JUNK",
        "first chunk must be the unknown JUNK chunk",
    );
    assert_eq!(
        doc.structure.chunks[2].id,
        "data",
        "third chunk must be 'data'",
    );
    assert!(
        doc.structure.junk_before_data
            && doc.structure.chunks[0].offset < doc.structure.chunks[2].offset,
        "JUNK chunk must precede the data chunk",
    );
    assert_eq!(
        doc.structure.data_chunk_size % 2,
        1,
        "data chunk size must be ODD to exercise the pad-byte walk",
    );
    let fmt = &doc.structure.fmt;
    assert_eq!(
        (fmt.format, fmt.channels, fmt.sample_rate, fmt.block_align, fmt.bits_per_sample),
        (1, 1, EXPECTED_SAMPLE_RATE, 2, 16),
        "fmt chunk fields must be validated PCM/mono/16 kHz/block-align 2/16 bit",
    );
    assert_eq!(
        fmt.byte_rate,
        fmt.sample_rate as u32 * fmt.block_align as u32,
        "fmt byte_rate must be consistent",
    );
    assert_eq!(
        u64::from(doc.structure.data_chunk_size / 2),
        doc.structure.expected_num_samples,
        "data chunk odd size must floor-divide to the expected sample count",
    );
    assert_eq!(
        doc.raw_pcm_i16.len() as u64,
        doc.structure.expected_num_samples,
        "raw_pcm_i16 metadata mismatch",
    );

    // --- JSON-internal consistency: replicate the decode formula --------
    for (i, (&raw16, &expected)) in doc.raw_pcm_i16.iter().zip(doc.expected_samples.iter()).enumerate() {
        let direct = raw16 as f32 / 32768.0f32;
        assert_eq!(
            direct.to_bits(),
            expected.to_bits(),
            "[formula check] sample {i}: expected bits {:08X} for raw {raw16}, metadata holds bits {:08X}",
            direct.to_bits(),
            expected.to_bits(),
        );
    }

    // --- golden output: decode via the file-path API ---------------------
    let decoded = read_pcm16_wav(GOLDEN_WAV)
        .unwrap_or_else(|err| panic!("readPcm16Wav failed on {GOLDEN_WAV}: {err}"));
    assert_f32_bits_equal(
        "golden.wav / read_pcm16_wav(decoded f32 samples)",
        &doc.expected_samples,
        &decoded,
    );

    // --- in-memory kernel produces the same bit patterns -----------------
    let decoded_in_memory = read_pcm16_wav_bytes(&bytes, EXPECTED_SAMPLE_RATE)
        .unwrap_or_else(|err| panic!("readPcm16WavBytes failed on {GOLDEN_WAV} bytes: {err}"));
    assert_f32_bits_equal(
        "golden.wav / read_pcm16_wav_bytes(decoded f32 samples)",
        &doc.expected_samples,
        &decoded_in_memory,
    );
    assert_eq!(
        decoded_in_memory.len(),
        decoded.len(),
        "path API and in-memory kernel disagree in output length",
    );
}
