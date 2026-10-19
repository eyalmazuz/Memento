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

//! Port of `src/asr/audio/melfrontend.{h,cpp}` — C++ `AsrAudio::MelFrontend`.
//!
//! Numerical contract: every floating-point operation is performed with the
//! same precision (f32 vs f64) and in the same order as the C++ original on a
//! baseline x86-64 build, so outputs are bit-identical. This is verified by
//! the golden-vector test suite (`tests/golden_mel.rs`), which cross-checks
//! against the compiled C++ implementation.
//!
//! Conversion notes (C++ → Rust):
//! - `std::complex<float>` on finite inputs multiplies as `(ac - bd, ad + bc)`
//!   with each operation rounded to f32; `Complex32` below reproduces that.
//! - `std::log`, `std::exp`, `std::cos`, `std::sin`, `std::sqrt` map to the
//!   Rust `f64`/`f32` methods, which lower to the same libm calls.
//! - `std::sort` of non-NaN floats maps to Rust `sort_unstable_by` with
//!   `total_cmp`.
//! - The C++ functions assume `nFft > 0`, `nMels > 0`, and non-negative
//!   `winLength`; those are build-time invariants of every shipped
//!   configuration. `extract()` re-checks the same guard the C++ version has
//!   (`hopLength > 0 && nFft > 0`); the one Rust-specific change is that
//!   constructor allocations clamp negative sizes instead of overflowing.

use std::f64::consts::PI;

/// C++: `AsrAudio::MelFrontendConfig`
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MelFrontendConfig {
    pub sample_rate: i32,
    pub n_fft: i32,
    pub win_length: i32,
    pub hop_length: i32,
    pub n_mels: i32,
    pub preemphasis: f32,
    pub log_zero_guard: f32,
    pub norm_epsilon: f32,
    pub normalize_per_feature: bool,
}

impl Default for MelFrontendConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            n_fft: 512,
            win_length: 400,
            hop_length: 160,
            n_mels: 128,
            preemphasis: 0.97,
            log_zero_guard: 5.9604644775390625e-8, // 2^-24
            norm_epsilon: 1e-5,
            normalize_per_feature: true,
        }
    }
}

/// C++: `AsrAudio::MelFeatureOutput`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MelFeatureOutput {
    /// Row-major [numFrames, nMels] float32 features
    pub features: Vec<f32>,
    /// [numFrames] attention mask (1 for valid frames, 0 for padded frames)
    pub attention_mask_int64: Vec<i64>,
    /// [numFrames] attention mask (1 for valid frames, 0 for padded frames)
    pub attention_mask_bool: Vec<u8>,
    pub num_frames: i64,
    pub valid_frames: i64,
    pub n_mels: i64,
}

/// C++: `AsrAudio::AudioUtteranceSpan`
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AudioUtteranceSpan {
    pub start_sample: usize,
    pub end_sample: usize,
    pub start_sec: f64,
    pub end_sec: f64,
}

/// Minimal f32 complex number matching `std::complex<float>` semantics for
/// finite values. Addition/subtraction are component-wise; multiplication is
/// the naive `(ac - bd, ad + bc)` with f32 rounding per operation.
#[derive(Debug, Clone, Copy)]
struct Complex32 {
    re: f32,
    im: f32,
}

impl Complex32 {
    #[inline]
    fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }

    #[inline]
    fn add(self, o: Self) -> Self {
        Self {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }

    #[inline]
    fn sub(self, o: Self) -> Self {
        Self {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }

    /// Naive f32 complex multiplication: `(ac - bd, ad + bc)`.
    #[inline]
    fn mul(self, o: Self) -> Self {
        Self {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
}

/// C++: `hzToMelSlaney`
fn hz_to_mel_slaney(freq_hz: f64) -> f64 {
    const F_MIN: f64 = 0.0;
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = (MIN_LOG_HZ - F_MIN) / F_SP;
    let log_step = f64::ln(6.4) / 27.0;

    if freq_hz >= MIN_LOG_HZ {
        return MIN_LOG_MEL + f64::ln(freq_hz / MIN_LOG_HZ) / log_step;
    }
    (freq_hz - F_MIN) / F_SP
}

/// C++: `melToHzSlaney`
fn mel_to_hz_slaney(mel: f64) -> f64 {
    const F_MIN: f64 = 0.0;
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = (MIN_LOG_HZ - F_MIN) / F_SP;
    let log_step = f64::ln(6.4) / 27.0;

    if mel >= MIN_LOG_MEL {
        return MIN_LOG_HZ * f64::exp(log_step * (mel - MIN_LOG_MEL));
    }
    F_MIN + F_SP * mel
}

/// C++: `radix2FftInPlace`
fn radix2_fft_in_place(a: &mut [Complex32]) {
    let n = a.len();

    // Bit-reversal permutation
    let mut i = 1usize;
    let mut j = 0usize;
    while i < n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            a.swap(i, j);
        }
        i += 1;
    }

    // Iterative radix-2 butterfly passes; twiddle factors are accumulated by
    // repeated f32 complex multiplication exactly like the C++ original.
    let mut len = 2usize;
    while len <= n {
        let ang = -2.0 * PI / (len as f64);
        let wlen = Complex32::new(ang.cos() as f32, ang.sin() as f32);
        let mut i = 0usize;
        while i < n {
            let mut w = Complex32::new(1.0, 0.0);
            let half = len >> 1;
            for j in 0..half {
                let u = a[i + j];
                let v = a[i + j + half].mul(w);
                a[i + j] = u.add(v);
                a[i + j + half] = u.sub(v);
                w = w.mul(wlen);
            }
            i += len;
        }
        len <<= 1;
    }
}

/// C++: `AsrAudio::MelFrontend`
#[derive(Debug, Clone)]
pub struct MelFrontend {
    config: MelFrontendConfig,
    padded_window: Vec<f32>,
    /// Row-major [nMels, numFreqBins] where numFreqBins = nFft / 2 + 1
    mel_filters: Vec<f32>,
}

impl MelFrontend {
    /// C++: `MelFrontend::MelFrontend(MelFrontendConfig config)`
    pub fn new(config: MelFrontendConfig) -> Self {
        let mut this = Self {
            config,
            padded_window: Vec::new(),
            mel_filters: Vec::new(),
        };
        this.init_window();
        this.init_mel_filterbank();
        this
    }

    /// C++: `config() const noexcept`
    pub fn config(&self) -> &MelFrontendConfig {
        &self.config
    }

    /// C++: `initWindow()`
    fn init_window(&mut self) {
        let n_fft = self.config.n_fft.max(0);
        self.padded_window = vec![0.0f32; n_fft as usize];
        let win_len = self.config.win_length.min(self.config.n_fft).max(0);
        let left_pad = (self.config.n_fft - win_len) / 2;
        for i in 0..win_len {
            // Symmetric Hann window (periodic=False),
            // matching torch.hann_window(win_length, periodic=False)
            let denom = if win_len > 1 {
                (win_len - 1) as f64
            } else {
                1.0
            };
            let w = 0.5 * (1.0 - (2.0 * PI * (i as f64) / denom).cos());
            self.padded_window[(left_pad + i) as usize] = w as f32;
        }
    }

    /// C++: `initMelFilterbank()`
    fn init_mel_filterbank(&mut self) {
        let num_bins = self.config.n_fft / 2 + 1;
        let n_mels = self.config.n_mels;
        let count = if n_mels >= 0 && num_bins >= 0 {
            (n_mels as usize) * (num_bins as usize)
        } else {
            0
        };
        self.mel_filters = vec![0.0f32; count];

        let f_min = 0.0;
        let f_max = (self.config.sample_rate as f64) / 2.0;
        let mel_min = hz_to_mel_slaney(f_min);
        let mel_max = hz_to_mel_slaney(f_max);

        if n_mels < 0 || num_bins < 0 {
            return;
        }
        let mut mel_pts = vec![0.0f64; (n_mels + 2) as usize];
        for i in 0..(n_mels + 2) as usize {
            let m = mel_min + (mel_max - mel_min) * (i as f64) / ((n_mels + 1) as f64);
            mel_pts[i] = mel_to_hz_slaney(m);
        }

        let mut fft_freqs = vec![0.0f64; num_bins as usize];
        for k in 0..num_bins as usize {
            fft_freqs[k] =
                (k as f64) * (self.config.sample_rate as f64) / (self.config.n_fft as f64);
        }

        for m in 0..n_mels as usize {
            let f_left = mel_pts[m];
            let f_center = mel_pts[m + 1];
            let f_right = mel_pts[m + 2];
            let diff_lower = (f_center - f_left).max(1e-12);
            let diff_upper = (f_right - f_center).max(1e-12);
            let enorm = 2.0 / (f_right - f_left).max(1e-12);

            let row = m * (num_bins as usize);
            for k in 0..num_bins as usize {
                let freq = fft_freqs[k];
                let lower = (freq - f_left) / diff_lower;
                let upper = (f_right - freq) / diff_upper;
                let weight = lower.min(upper).max(0.0) * enorm;
                self.mel_filters[row + k] = weight as f32;
            }
        }
    }

    /// C++: `MelFrontend::extract(const float *samples, size_t numSamples) const`
    pub fn extract(&self, samples: &[f32]) -> MelFeatureOutput {
        let num_samples = samples.len();
        let mut out = MelFeatureOutput {
            n_mels: self.config.n_mels as i64,
            ..Default::default()
        };
        if num_samples == 0 || self.config.hop_length <= 0 || self.config.n_fft <= 0 {
            return out;
        }

        // 1. Pre-emphasis filter: y[0] = x[0], y[t] = x[t] - preemphasis * x[t-1]
        let mut preemph = vec![0.0f32; num_samples];
        if self.config.preemphasis > 0.0 {
            preemph[0] = samples[0];
            for i in 1..num_samples {
                preemph[i] = samples[i] - self.config.preemphasis * samples[i - 1];
            }
        } else {
            preemph.copy_from_slice(samples);
        }

        // 2. Center constant zero-padding of n_fft / 2 on both sides
        //    (matching torch.stft(..., pad_mode="constant"))
        let pad = self.config.n_fft / 2;
        let padded_len = num_samples + (2 * pad) as usize;
        let mut padded = vec![0.0f32; padded_len];
        padded[(pad as usize)..(pad as usize + num_samples)].copy_from_slice(&preemph);

        let n_fft = self.config.n_fft;
        let hop = self.config.hop_length;

        // C++: totalFrames = 1 + (paddedLen - nFft) / nFft-independent hop
        // (size_t division; paddedLen >= nFft holds because numSamples >= 1)
        let total_frames = 1 + ((padded_len - n_fft as usize) / hop as usize) as i64;
        let valid_frames = total_frames.min((num_samples as i64) / (hop as i64));
        if total_frames <= 0 || valid_frames <= 0 {
            return out;
        }

        out.num_frames = total_frames;
        out.valid_frames = valid_frames;
        out.features = vec![0.0f32; total_frames as usize * self.config.n_mels as usize];
        out.attention_mask_int64 = vec![0i64; total_frames as usize];
        out.attention_mask_bool = vec![0u8; total_frames as usize];

        for t in 0..valid_frames as usize {
            out.attention_mask_int64[t] = 1;
            out.attention_mask_bool[t] = 1;
        }

        // 3. STFT power spectrum + Slaney Mel filterbank + log compression
        let num_bins = n_fft / 2 + 1;
        let n_mels = self.config.n_mels;
        let mut fft_buf = vec![Complex32::new(0.0, 0.0); n_fft as usize];
        let mut power_spec = vec![0.0f32; num_bins as usize];

        for t in 0..total_frames as usize {
            let frame_start = t * hop as usize;
            for i in 0..n_fft as usize {
                fft_buf[i] =
                    Complex32::new(padded[frame_start + i] * self.padded_window[i], 0.0);
            }

            radix2_fft_in_place(&mut fft_buf);

            for k in 0..num_bins as usize {
                let re = fft_buf[k].re;
                let im = fft_buf[k].im;
                power_spec[k] = re * re + im * im;
            }

            let mel_frame = t as usize * n_mels as usize;
            for m in 0..n_mels as usize {
                let filter_row = m as usize * num_bins as usize;
                let mut sum = 0.0f32;
                for k in 0..num_bins as usize {
                    sum += self.mel_filters[filter_row + k] * power_spec[k];
                }
                out.features[mel_frame + m] = (sum.max(0.0) + self.config.log_zero_guard).ln();
            }
        }

        // 4. Per-feature normalization over validFrames
        //    (with Bessel's correction: divisor validFrames - 1)
        if self.config.normalize_per_feature && valid_frames > 1 {
            for m in 0..n_mels as usize {
                let mut sum = 0.0f64;
                for t in 0..valid_frames as usize {
                    sum += out.features[t * (n_mels as usize) + m] as f64;
                }
                let mean = sum / (valid_frames as f64);

                let mut sq_diff_sum = 0.0f64;
                for t in 0..valid_frames as usize {
                    let diff = out.features[t * (n_mels as usize) + m] as f64 - mean;
                    sq_diff_sum += diff * diff;
                }
                let variance = sq_diff_sum / ((valid_frames - 1) as f64);
                let inv_std =
                    (1.0 / (f64::sqrt(variance) + self.config.norm_epsilon as f64)) as f32;
                let mean_f = mean as f32;

                for t in 0..valid_frames as usize {
                    let val = out.features[t * (n_mels as usize) + m];
                    out.features[t * (n_mels as usize) + m] = (val - mean_f) * inv_std;
                }
                for t in valid_frames as usize..total_frames as usize {
                    out.features[t * (n_mels as usize) + m] = 0.0;
                }
            }
        } else {
            for t in valid_frames as usize..total_frames as usize {
                for m in 0..n_mels as usize {
                    out.features[t * (n_mels as usize) + m] = 0.0;
                }
            }
        }

        out
    }

    /// C++: `MelFrontend::segmentSpeechUtterances(...)`
    pub fn segment_speech_utterances(
        samples: &[f32],
        sample_rate: i32,
        max_utterance_sec: f64,
    ) -> Vec<AudioUtteranceSpan> {
        let num_samples = samples.len();
        let mut result: Vec<AudioUtteranceSpan> = Vec::new();
        if num_samples == 0 || sample_rate <= 0 {
            return result;
        }

        // 20 ms analysis frames
        let frame_len = 1.max(sample_rate / 50) as usize;
        let num_frames = (num_samples + frame_len - 1) / frame_len;
        let mut rms = vec![0.0f32; num_frames];

        for f in 0..num_frames {
            let start = f * frame_len;
            let end = num_samples.min(start + frame_len);
            let mut sum_sq = 0.0f64;
            for v in &samples[start..end] {
                sum_sq += (*v as f64) * (*v as f64);
            }
            rms[f] = f64::sqrt(sum_sq / ((end - start) as f64)) as f32;
        }

        // Compute adaptive RMS threshold from percentiles
        let mut sorted_rms = rms.clone();
        sorted_rms.sort_unstable_by(f32::total_cmp);
        let p20 = sorted_rms[sorted_rms.len() * 20 / 100];
        let p85 = sorted_rms[sorted_rms.len() * 85 / 100];
        if p85 < 0.0015f32 {
            // Entire clip is near-silent
            return result;
        }

        let threshold = (p20 + 0.12f32 * (p85 - p20)).clamp(0.0025f32, 0.025f32);
        // Minimum silence gap to split utterances: 14 frames = 280 ms
        const MIN_SILENCE_FRAMES: usize = 14;
        // Context padding around speech utterances: 6 frames = 120 ms
        const PAD_FRAMES: usize = 6;
        // Minimum speech frames to keep an utterance: 8 frames = 160 ms
        const MIN_SPEECH_FRAMES: usize = 8;

        #[derive(Clone, Copy)]
        struct FrameSpan {
            start_frame: usize,
            end_frame: usize,
        }
        let mut raw_spans: Vec<FrameSpan> = Vec::new();

        // Mirrors the C++ scanning loop exactly: `i` is never incremented
        // after the inner scan; iteration resumes at the same frame (which is
        // silent when the inner scan broke, so the next outer pass skips it).
        let mut i = 0usize;
        while i < num_frames {
            if rms[i] < threshold {
                i += 1;
                continue;
            }
            let speech_start = i;
            let mut last_active = i;
            i += 1;
            while i < num_frames {
                if rms[i] >= threshold {
                    last_active = i;
                } else if i - last_active >= MIN_SILENCE_FRAMES {
                    break;
                }
                i += 1;
            }
            let speech_end = last_active + 1;
            if speech_end - speech_start >= MIN_SPEECH_FRAMES {
                // C++: paddedStart = speechStart > padFrames ? speechStart - padFrames : 0
                let padded_start = if speech_start > PAD_FRAMES {
                    speech_start - PAD_FRAMES
                } else {
                    0
                };
                let padded_end = num_frames.min(speech_end + PAD_FRAMES);
                let merged = matches!(
                    raw_spans.last(),
                    Some(last) if padded_start <= last.end_frame
                );
                if merged {
                    let last = raw_spans.len() - 1;
                    raw_spans[last].end_frame = raw_spans[last].end_frame.max(padded_end);
                } else {
                    raw_spans.push(FrameSpan {
                        start_frame: padded_start,
                        end_frame: padded_end,
                    });
                }
            }
        }

        // Split any span exceeding maxUtteranceSec at lowest energy valleys.
        // Precondition: `curStart` only ever increases (bestFrame >=
        // curStart + minSplitFrames), so `endFrame - curStart` cannot wrap.
        let max_frames = (max_utterance_sec.max(2.0) * 50.0) as usize;
        let min_split_frames = (2.0 * 50.0) as usize;

        for span in &raw_spans {
            let mut cur_start = span.start_frame;
            while span.end_frame - cur_start > max_frames {
                let search_start = cur_start + min_split_frames;
                let search_end = (span.end_frame - min_split_frames).min(cur_start + max_frames);
                let mut best_frame = search_end;
                let mut min_window_energy = 1e9f32;

                for f in search_start..=search_end {
                    // 5-frame (100 ms) local energy average
                    let lo = f.saturating_sub(2);
                    let hi = (num_frames - 1).min(f + 2);
                    let mut local_sum = 0.0f32;
                    for k in lo..=hi {
                        local_sum += rms[k];
                    }
                    if local_sum < min_window_energy {
                        min_window_energy = local_sum;
                        best_frame = f;
                    }
                }

                let start_sample = num_samples.min(cur_start * frame_len);
                let end_sample = num_samples.min(best_frame * frame_len);
                if end_sample > start_sample {
                    result.push(AudioUtteranceSpan {
                        start_sample,
                        end_sample,
                        start_sec: (start_sample as f64) / (sample_rate as f64),
                        end_sec: (end_sample as f64) / (sample_rate as f64),
                    });
                }
                cur_start = best_frame;
            }

            let start_sample = num_samples.min(cur_start * frame_len);
            let end_sample = num_samples.min(span.end_frame * frame_len);
            if end_sample > start_sample {
                result.push(AudioUtteranceSpan {
                    start_sample,
                    end_sample,
                    start_sec: (start_sample as f64) / (sample_rate as f64),
                    end_sec: (end_sample as f64) / (sample_rate as f64),
                });
            }
        }

        result
    }
}
