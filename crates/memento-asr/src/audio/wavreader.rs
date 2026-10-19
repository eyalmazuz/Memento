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

//! Port of `src/asr/audio/wavreader.{h,cpp}` — C++ `AsrAudio::readPcm16Wav`.
//!
//! Error messages are kept byte-identical to the C++ strings so callers
//! surface identical diagnostics. The `qWarning` side effect of the C++
//! implementation is intentionally not reproduced (the error text is
//! returned instead).

use std::fs;
use std::path::Path;

/// C++: `inline constexpr int AsrAudio::SAMPLE_RATE_16K`
pub const SAMPLE_RATE_16K: i32 = 16000;

const WAV_HEADER_SIZE: usize = 12;
const WAV_CHUNK_HEADER_SIZE: usize = 8;
const WAV_FORMAT_PCM: u16 = 1;
const WAV_BITS_PER_SAMPLE: u16 = 16;
const WAV_CHANNELS: u16 = 1;

const BYTES_PER_SAMPLE: usize = WAV_BITS_PER_SAMPLE as usize / 8;

const ERR_OPEN_FAILED: &str = "Could not open the extracted audio file.";
const ERR_TRUNCATED_HEADER: &str = "The extracted WAV is truncated";
const ERR_NOT_RIFF: &str = "The extracted audio is not RIFF/WAVE";
const ERR_TRUNCATED: &str = "The extracted WAV file is truncated.";
const ERR_NOT_PCM16: &str = "The extracted WAV is not 16 kHz mono PCM16.";
const ERR_NO_SAMPLES: &str = "The extracted WAV contains no audio samples.";

fn read_u16(data: &[u8]) -> u16 {
    u16::from_le_bytes([data[0], data[1]])
}

fn read_u32(data: &[u8]) -> u32 {
    u32::from_le_bytes([data[0], data[1], data[2], data[3]])
}

/// C++: `readPcm16Wav(path, error, expectedSampleRate)` (default rate overload)
pub fn read_pcm16_wav<P: AsRef<Path>>(path: P) -> Result<Vec<f32>, String> {
    read_pcm16_wav_path(path, SAMPLE_RATE_16K as u32)
}

/// C++: `readPcm16Wav(path, error, expectedSampleRate)`
pub fn read_pcm16_wav_path<P: AsRef<Path>>(
    path: P,
    expected_sample_rate: u32,
) -> Result<Vec<f32>, String> {
    match fs::read(path.as_ref()) {
        Ok(bytes) => read_pcm16_wav_bytes(&bytes, expected_sample_rate),
        Err(_) => Err(ERR_OPEN_FAILED.to_string()),
    }
}

/// In-memory kernel of `readPcm16Wav`, operating on the raw file contents.
///
/// Chunk semantics mirrored from the C++ loop:
/// - scans chunks sequentially from offset 12,
/// - rejects when a chunk data range would run past EOF,
/// - accepts `fmt`/`data` in any order (last `data` wins, mirroring
///   `pcmData = bytes.mid(...)` reassignment),
/// - honors odd-size chunk padding (`offset += size + (size % 2)`),
/// - validates format/alignment AFTER the whole chunk walk.
pub fn read_pcm16_wav_bytes(bytes: &[u8], expected_sample_rate: u32) -> Result<Vec<f32>, String> {
    if bytes.len() < WAV_HEADER_SIZE {
        return Err(format!("{ERR_TRUNCATED_HEADER} ({} bytes).", bytes.len()));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        // Qt: bytes.first(12).toHex(' ') — lowercase hex, space-separated.
        let header_hex: Vec<String> =
            bytes[..12].iter().map(|b| format!("{b:02x}")).collect();
        return Err(format!("{ERR_NOT_RIFF} (header {}).", header_hex.join(" ")));
    }

    let mut format: u16 = 0;
    let mut channels: u16 = 0;
    let mut sample_rate: u32 = 0;
    let mut block_align: u16 = 0;
    let mut bits_per_sample: u16 = 0;
    let mut pcm_data: Option<&[u8]> = None;

    let mut offset = WAV_HEADER_SIZE;
    while offset + WAV_CHUNK_HEADER_SIZE <= bytes.len() {
        let chunk = &bytes[offset..];
        let id = &chunk[0..4];
        let size = read_u32(&chunk[4..8]) as usize;
        offset += WAV_CHUNK_HEADER_SIZE;

        if offset + size > bytes.len() {
            return Err(ERR_TRUNCATED.to_string());
        }

        if id == b"fmt " && size >= 16 {
            let fmt = &bytes[offset..];
            format = read_u16(&fmt[0..2]);
            channels = read_u16(&fmt[2..4]);
            sample_rate = read_u32(&fmt[4..8]);
            block_align = read_u16(&fmt[12..14]);
            bits_per_sample = read_u16(&fmt[14..16]);
        } else if id == b"data" {
            pcm_data = Some(&bytes[offset..offset + size]);
        }

        offset += size + (size % 2);
    }

    if format != WAV_FORMAT_PCM
        || channels != WAV_CHANNELS
        || sample_rate != expected_sample_rate
        || block_align != WAV_CHANNELS * (BYTES_PER_SAMPLE as u16)
        || bits_per_sample != WAV_BITS_PER_SAMPLE
        || pcm_data.map_or(true, |data| data.is_empty())
    {
        return Err(ERR_NOT_PCM16.to_string());
    }
    let pcm_data = pcm_data.unwrap();

    // C++ has a second check for `pcmData.size() < BYTES_PER_SAMPLE`; this is
    // unreachable after the empty-data check above (mirrored), but kept for
    // exact parity in case a non-empty chunk with a single leading byte root
    // (odd size) survives the floor division below.
    if pcm_data.len() < BYTES_PER_SAMPLE {
        return Err(ERR_NO_SAMPLES.to_string());
    }

    let samples = pcm_data.len() / BYTES_PER_SAMPLE;
    let mut output = Vec::with_capacity(samples);
    for i in 0..samples {
        let value = read_u16(&pcm_data[i * BYTES_PER_SAMPLE..]) as i16;
        output.push(value as f32 / 32768.0f32);
    }
    Ok(output)
}
