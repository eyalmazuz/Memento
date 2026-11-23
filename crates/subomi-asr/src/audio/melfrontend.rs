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

/// Selects which attention mask buffer(s) [`MelFrontend::extract_with_mask`]
/// allocates and populates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionMaskKind {
    /// Populate only `attention_mask_int64`.
    Int64,
    /// Populate only `attention_mask_bool`.
    Bool,
    /// Populate both `attention_mask_int64` and `attention_mask_bool`.
    Both,
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

/// Precomputed radix-2 FFT bit-reversal swaps and twiddle tables for power-of-two
/// `n_fft`, plus a precomputed real-DFT cosine/sine matrix fallback for
/// non-power-of-two `n_fft` (e.g., `n_fft = 400` in Whisper ONNX configs).
/// Twiddle factors for radix-2 are accumulated by the same repeated `f32` complex
/// multiplication as the C++ reference so STFT outputs remain bit-identical.
#[derive(Debug, Clone)]
struct FftPlan {
    n: usize,
    bit_reversal_swaps: Vec<(usize, usize)>,
    twiddles: Vec<Complex32>,
    dft_cos: Vec<f32>,
    dft_sin: Vec<f32>,
}

const MAX_PRECOMPUTED_DFT_N: usize = 4096;

impl FftPlan {
    fn new(n_fft: i32) -> Self {
        let n = n_fft.max(0) as usize;
        let mut bit_reversal_swaps = Vec::new();
        let mut twiddles = Vec::new();
        let mut dft_cos = Vec::new();
        let mut dft_sin = Vec::new();

        if n > 1 && n.is_power_of_two() {
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
                    bit_reversal_swaps.push((i, j));
                }
                i += 1;
            }

            twiddles.reserve(n);
            let mut len = 2usize;
            while len <= n {
                let ang = -2.0 * PI / (len as f64);
                let wlen = Complex32::new(ang.cos() as f32, ang.sin() as f32);
                let half = len >> 1;
                let mut w = Complex32::new(1.0, 0.0);
                for _ in 0..half {
                    twiddles.push(w);
                    w = w.mul(wlen);
                }
                len <<= 1;
            }
        } else if n > 1 && n <= MAX_PRECOMPUTED_DFT_N {
            let num_bins = n / 2 + 1;
            let total = num_bins * n;
            dft_cos.reserve_exact(total);
            dft_sin.reserve_exact(total);
            let inv_n = -2.0 * PI / (n as f64);
            for k in 0..num_bins {
                let k_factor = inv_n * (k as f64);
                for j in 0..n {
                    let ang = k_factor * (j as f64);
                    dft_cos.push(ang.cos() as f32);
                    dft_sin.push(ang.sin() as f32);
                }
            }
        }

        Self {
            n,
            bit_reversal_swaps,
            twiddles,
            dft_cos,
            dft_sin,
        }
    }

    #[inline]
    fn fft_in_place(&self, a: &mut [Complex32]) {
        let n = self.n;
        if n <= 1 || a.len() != n {
            return;
        }
        if !n.is_power_of_two() {
            let input = a.to_vec();
            let inv_n = -2.0 * PI / (n as f64);
            for (k, out) in a.iter_mut().enumerate() {
                let step = inv_n * (k as f64);
                let mut re = 0.0f64;
                let mut im = 0.0f64;
                for (j, c) in input.iter().enumerate() {
                    let ang = step * (j as f64);
                    let (sin_a, cos_a) = ang.sin_cos();
                    let xr = c.re as f64;
                    let xi = c.im as f64;
                    re += xr * cos_a - xi * sin_a;
                    im += xr * sin_a + xi * cos_a;
                }
                *out = Complex32::new(re as f32, im as f32);
            }
            return;
        }

        for &(i, j) in &self.bit_reversal_swaps {
            a.swap(i, j);
        }

        let mut twiddle_offset = 0usize;
        let mut len = 2usize;
        while len <= n {
            let half = len >> 1;
            let stage_twiddles = &self.twiddles[twiddle_offset..twiddle_offset + half];
            twiddle_offset += half;
            let mut i = 0usize;
            while i < n {
                for (j, &w) in stage_twiddles.iter().enumerate() {
                    let u = a[i + j];
                    let v = a[i + j + half].mul(w);
                    a[i + j] = u.add(v);
                    a[i + j + half] = u.sub(v);
                }
                i += len;
            }
            len <<= 1;
        }
    }

    #[inline]
    fn compute_power_spectrum(
        &self,
        frame: &[f32],
        window: &[f32],
        fft_buf: &mut [Complex32],
        power_spec: &mut [f32],
    ) {
        let n = self.n;
        if n == 0 || frame.len() < n || window.len() < n {
            power_spec.fill(0.0);
            return;
        }
        if n.is_power_of_two() {
            for i in 0..n {
                fft_buf[i] = Complex32::new(frame[i] * window[i], 0.0);
            }
            self.fft_in_place(fft_buf);
            for (k, out_bin) in power_spec.iter_mut().enumerate() {
                let re = fft_buf[k].re;
                let im = fft_buf[k].im;
                *out_bin = re * re + im * im;
            }
        } else if !self.dft_cos.is_empty() {
            // Pre-window into fft_buf[..].re so each bin's dot product reads contiguous f32s.
            for i in 0..n {
                fft_buf[i].re = frame[i] * window[i];
            }
            for (k, out_bin) in power_spec.iter_mut().enumerate() {
                let row = k * n;
                let cos_row = &self.dft_cos[row..row + n];
                let sin_row = &self.dft_sin[row..row + n];
                let mut re = 0.0f32;
                let mut im = 0.0f32;
                for j in 0..n {
                    let x = fft_buf[j].re;
                    re += x * cos_row[j];
                    im += x * sin_row[j];
                }
                *out_bin = re * re + im * im;
            }
        } else {
            for i in 0..n {
                fft_buf[i].re = frame[i] * window[i];
            }
            let inv_n = -2.0 * PI / (n as f64);
            for (k, out_bin) in power_spec.iter_mut().enumerate() {
                let step = inv_n * (k as f64);
                let (step_sin, step_cos) = step.sin_cos();
                let mut cos_j = 1.0f64;
                let mut sin_j = 0.0f64;
                let mut re = 0.0f64;
                let mut im = 0.0f64;
                for item in fft_buf.iter().take(n) {
                    let x = item.re as f64;
                    re += x * cos_j;
                    im += x * sin_j;
                    let next_cos = cos_j * step_cos - sin_j * step_sin;
                    let next_sin = cos_j * step_sin + sin_j * step_cos;
                    cos_j = next_cos;
                    sin_j = next_sin;
                }
                let re_f = re as f32;
                let im_f = im as f32;
                *out_bin = re_f * re_f + im_f * im_f;
            }
        }
    }
}

/// C++: `AsrAudio::MelFrontend`
#[derive(Debug, Clone)]
pub struct MelFrontend {
    config: MelFrontendConfig,
    padded_window: Vec<f32>,
    /// Row-major [nMels, numFreqBins] where numFreqBins = nFft / 2 + 1
    mel_filters: Vec<f32>,
    /// Half-open non-zero frequency bin range `(k_start, k_end)` per mel row.
    mel_filter_ranges: Vec<(usize, usize)>,
    fft_plan: FftPlan,
}

impl MelFrontend {
    /// C++: `MelFrontend::MelFrontend(MelFrontendConfig config)`
    pub fn new(config: MelFrontendConfig) -> Self {
        let mut this = Self {
            config,
            padded_window: Vec::new(),
            mel_filters: Vec::new(),
            mel_filter_ranges: Vec::new(),
            fft_plan: FftPlan::new(config.n_fft),
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
        self.mel_filter_ranges.clear();

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

        self.mel_filter_ranges.reserve(n_mels as usize);
        let num_bins_usize = num_bins as usize;
        for m in 0..n_mels as usize {
            let f_left = mel_pts[m];
            let f_center = mel_pts[m + 1];
            let f_right = mel_pts[m + 2];
            let diff_lower = (f_center - f_left).max(1e-12);
            let diff_upper = (f_right - f_center).max(1e-12);
            let enorm = 2.0 / (f_right - f_left).max(1e-12);

            let row = m * num_bins_usize;
            let mut k_start = num_bins_usize;
            let mut k_end = 0usize;
            for k in 0..num_bins_usize {
                let freq = fft_freqs[k];
                let lower = (freq - f_left) / diff_lower;
                let upper = (f_right - freq) / diff_upper;
                let weight = (lower.min(upper).max(0.0) * enorm) as f32;
                self.mel_filters[row + k] = weight;
                if weight != 0.0 {
                    if k_start == num_bins_usize {
                        k_start = k;
                    }
                    k_end = k + 1;
                }
            }
            if k_start > k_end {
                k_start = 0;
            }
            self.mel_filter_ranges.push((k_start, k_end));
        }
    }

    /// C++: `MelFrontend::extract(const float *samples, size_t numSamples) const`
    pub fn extract(&self, samples: &[f32]) -> MelFeatureOutput {
        self.extract_with_mask(samples, AttentionMaskKind::Both)
    }

    /// Like [`Self::extract`], but only allocates and populates the requested
    /// attention mask buffer(s) to avoid allocating an unused mask format.
    pub fn extract_with_mask(
        &self,
        samples: &[f32],
        mask_kind: AttentionMaskKind,
    ) -> MelFeatureOutput {
        let num_samples = samples.len();
        let mut out = MelFeatureOutput {
            n_mels: self.config.n_mels.max(0) as i64,
            ..Default::default()
        };
        if num_samples == 0
            || self.config.hop_length <= 0
            || self.config.n_fft <= 0
            || self.config.n_mels <= 0
        {
            return out;
        }

        // 1 & 2. Pre-emphasis filter applied directly into the center-padded
        //        buffer (matching torch.stft(..., pad_mode="constant")).
        let pad = (self.config.n_fft / 2) as usize;
        let padded_len = num_samples + 2 * pad;
        let mut padded = vec![0.0f32; padded_len];
        let dst = &mut padded[pad..pad + num_samples];
        if self.config.preemphasis > 0.0 {
            let coeff = self.config.preemphasis;
            dst[0] = samples[0];
            for i in 1..num_samples {
                dst[i] = samples[i] - coeff * samples[i - 1];
            }
        } else {
            dst.copy_from_slice(samples);
        }

        let n_fft = self.config.n_fft;
        let hop = self.config.hop_length;

        // C++: totalFrames = 1 + (paddedLen - nFft) / hop
        // (size_t division; paddedLen >= nFft holds because numSamples >= 1)
        let total_frames = 1 + ((padded_len - n_fft as usize) / hop as usize) as i64;
        let valid_frames = total_frames.min((num_samples as i64) / (hop as i64));
        if total_frames <= 0 || valid_frames <= 0 {
            return out;
        }

        let total_usize = total_frames as usize;
        let valid_usize = valid_frames as usize;
        let n_mels_usize = self.config.n_mels as usize;

        out.num_frames = total_frames;
        out.valid_frames = valid_frames;
        out.features = vec![0.0f32; total_usize * n_mels_usize];

        match mask_kind {
            AttentionMaskKind::Int64 => {
                out.attention_mask_int64 = vec![0i64; total_usize];
                out.attention_mask_int64[..valid_usize].fill(1);
            }
            AttentionMaskKind::Bool => {
                out.attention_mask_bool = vec![0u8; total_usize];
                out.attention_mask_bool[..valid_usize].fill(1);
            }
            AttentionMaskKind::Both => {
                out.attention_mask_int64 = vec![0i64; total_usize];
                out.attention_mask_bool = vec![0u8; total_usize];
                out.attention_mask_int64[..valid_usize].fill(1);
                out.attention_mask_bool[..valid_usize].fill(1);
            }
        }

        // 3. STFT power spectrum + sparse Slaney Mel filterbank + log compression
        let num_bins = (n_fft / 2 + 1) as usize;
        let mut fft_buf = vec![Complex32::new(0.0, 0.0); n_fft as usize];
        let mut power_spec = vec![0.0f32; num_bins];

        for t in 0..total_usize {
            let frame_start = t * hop as usize;
            self.fft_plan.compute_power_spectrum(
                &padded[frame_start..frame_start + n_fft as usize],
                &self.padded_window,
                &mut fft_buf,
                &mut power_spec,
            );

            let mel_frame = t * n_mels_usize;
            for m in 0..n_mels_usize {
                let (k_start, k_end) = self.mel_filter_ranges[m];
                let filter_row = m * num_bins;
                let filter_slice = &self.mel_filters[filter_row + k_start..filter_row + k_end];
                let power_slice = &power_spec[k_start..k_end];
                let mut sum = 0.0f32;
                for (&w, &p) in filter_slice.iter().zip(power_slice.iter()) {
                    sum += w * p;
                }
                out.features[mel_frame + m] = (sum.max(0.0) + self.config.log_zero_guard).ln();
            }
        }

        // 4. Per-feature normalization over validFrames (t-outer, m-inner for
        //    sequential row-major cache locality, preserving exact per-m f64
        //    summation order and Bessel's correction: divisor validFrames - 1).
        if self.config.normalize_per_feature && valid_frames > 1 {
            let mut means = vec![0.0f64; n_mels_usize];
            for t in 0..valid_usize {
                let row = &out.features[t * n_mels_usize..(t + 1) * n_mels_usize];
                for m in 0..n_mels_usize {
                    means[m] += row[m] as f64;
                }
            }
            let valid_f64 = valid_frames as f64;
            for mean in &mut means {
                *mean /= valid_f64;
            }

            let mut sq_diff_sums = vec![0.0f64; n_mels_usize];
            for t in 0..valid_usize {
                let row = &out.features[t * n_mels_usize..(t + 1) * n_mels_usize];
                for m in 0..n_mels_usize {
                    let diff = row[m] as f64 - means[m];
                    sq_diff_sums[m] += diff * diff;
                }
            }

            let denom = (valid_frames - 1) as f64;
            let eps = self.config.norm_epsilon as f64;
            let mut mean_f = vec![0.0f32; n_mels_usize];
            let mut inv_std = vec![0.0f32; n_mels_usize];
            for m in 0..n_mels_usize {
                let variance = sq_diff_sums[m] / denom;
                inv_std[m] = (1.0 / (f64::sqrt(variance) + eps)) as f32;
                mean_f[m] = means[m] as f32;
            }

            for t in 0..valid_usize {
                let row = &mut out.features[t * n_mels_usize..(t + 1) * n_mels_usize];
                for m in 0..n_mels_usize {
                    row[m] = (row[m] - mean_f[m]) * inv_std[m];
                }
            }
        }
        out.features[valid_usize * n_mels_usize..total_usize * n_mels_usize].fill(0.0);

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
        // Capping `min_split_frames` at `max_frames / 2` guarantees
        // `search_start <= search_end` even for short `max_utterance_sec` in
        // `[2.0, 4.0)` while preserving exact `100`-frame behavior for `>= 4.0`.
        let max_frames = (max_utterance_sec.max(2.0) * 50.0) as usize;
        let min_split_frames = ((2.0 * 50.0) as usize).min(max_frames / 2);

        for span in &raw_spans {
            let mut cur_start = span.start_frame;
            while span.end_frame - cur_start > max_frames {
                let search_start = cur_start + min_split_frames;
                let search_end =
                    (span.end_frame - min_split_frames).min(cur_start.saturating_add(max_frames));
                let mut best_frame = search_end;
                let mut min_window_energy = f32::INFINITY;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_plan_precomputes_twiddles_and_bit_reversal_for_power_of_two() {
        let frontend = MelFrontend::new(MelFrontendConfig::default());
        assert_eq!(frontend.fft_plan.n, 512);
        // Sum of 1 + 2 + 4 + ... + 256 = 511 twiddles across all stages.
        assert_eq!(frontend.fft_plan.twiddles.len(), 511);
        assert!(!frontend.fft_plan.bit_reversal_swaps.is_empty());

        let mut buf: Vec<Complex32> = (0..512)
            .map(|i| Complex32::new((i as f32 * 0.05).sin(), 0.0))
            .collect();
        let original = buf.clone();
        frontend.fft_plan.fft_in_place(&mut buf);
        // DC bin equals sum of real inputs.
        let dc_expected: f32 = original.iter().map(|c| c.re).sum();
        assert!((buf[0].re - dc_expected).abs() < 1e-4);
        assert!(buf[0].im.abs() < 1e-4);
    }

    #[test]
    fn non_power_of_two_n_fft_400_computes_accurate_dft_without_panicking() {
        let cfg = MelFrontendConfig {
            sample_rate: 16000,
            n_fft: 400,
            win_length: 400,
            hop_length: 160,
            n_mels: 80,
            preemphasis: 0.0,
            normalize_per_feature: false,
            ..MelFrontendConfig::default()
        };
        let frontend = MelFrontend::new(cfg);
        assert_eq!(frontend.fft_plan.n, 400);
        assert!(frontend.fft_plan.twiddles.is_empty());
        assert_eq!(frontend.fft_plan.dft_cos.len(), 201 * 400);
        assert_eq!(frontend.fft_plan.dft_sin.len(), 201 * 400);

        // Verify exact 400-point DFT power spectrum against f64 reference on a 400 Hz tone
        // (exact bin k = 400 * 400 / 16000 = 10).
        let frame: Vec<f32> = (0..400)
            .map(|n| (2.0 * PI * 400.0 * (n as f64) / 16000.0).sin() as f32)
            .collect();
        let ones = vec![1.0f32; 400];
        let mut fft_buf = vec![Complex32::new(0.0, 0.0); 400];
        let mut power = vec![0.0f32; 201];
        frontend
            .fft_plan
            .compute_power_spectrum(&frame, &ones, &mut fft_buf, &mut power);
        // Bin 10 has amplitude 200 -> power = 40000; adjacent bins are ~0.
        assert!((power[10] - 40_000.0).abs() < 1.0, "power[10]={}", power[10]);
        assert!(power[0] < 1e-3);
        assert!(power[9] < 1e-2);
        assert!(power[11] < 1e-2);

        // Full MelFrontend::extract on 1 second of 16 kHz audio with n_fft = 400.
        let samples: Vec<f32> = (0..16000)
            .map(|i| (2.0 * PI * 440.0 * (i as f64) / 16000.0).sin() as f32 * 0.5)
            .collect();
        let mel = frontend.extract(&samples);
        assert!(mel.num_frames > 0);
        assert!(mel.valid_frames > 0);
        assert_eq!(mel.n_mels, 80);
        assert_eq!(mel.features.len(), mel.num_frames as usize * 80);
        assert!(mel.features.iter().all(|v| v.is_finite()));

        // Malformed zero / negative configs return empty output cleanly without panicking.
        for bad_fft in [0, -1, -400] {
            let bad = MelFrontend::new(MelFrontendConfig {
                n_fft: bad_fft,
                ..MelFrontendConfig::default()
            });
            let empty = bad.extract(&samples);
            assert_eq!(empty.num_frames, 0);
            assert!(empty.features.is_empty());
        }
    }

    #[test]
    fn sparse_mel_ranges_and_selective_attention_mask_allocation() {
        let frontend = MelFrontend::new(MelFrontendConfig::default());
        let num_bins = (frontend.config.n_fft / 2 + 1) as usize;
        assert_eq!(frontend.mel_filter_ranges.len(), frontend.config.n_mels as usize);
        for (m, &(k_start, k_end)) in frontend.mel_filter_ranges.iter().enumerate() {
            assert!(k_start <= k_end && k_end <= num_bins);
            let row = &frontend.mel_filters[m * num_bins..(m + 1) * num_bins];
            for (k, &w) in row.iter().enumerate() {
                if k < k_start || k >= k_end {
                    assert_eq!(w, 0.0, "non-zero weight outside sparse range at m={m}, k={k}");
                }
            }
        }

        let samples: Vec<f32> = (0..8000)
            .map(|i| (2.0 * PI * 440.0 * (i as f64) / 16000.0).sin() as f32 * 0.4)
            .collect();
        let both = frontend.extract(&samples);
        let only_i64 = frontend.extract_with_mask(&samples, AttentionMaskKind::Int64);
        let only_bool = frontend.extract_with_mask(&samples, AttentionMaskKind::Bool);

        assert_eq!(only_i64.features, both.features);
        assert_eq!(only_i64.attention_mask_int64, both.attention_mask_int64);
        assert!(only_i64.attention_mask_bool.is_empty());

        assert_eq!(only_bool.features, both.features);
        assert!(only_bool.attention_mask_int64.is_empty());
        assert_eq!(only_bool.attention_mask_bool, both.attention_mask_bool);
    }

    #[test]
    fn segment_speech_utterances_handles_short_max_utterance_split() {
        // 3.0 s of continuous speech with an energy dip near 1.5 s.
        let sample_rate = 16000;
        let total_samples = sample_rate * 3;
        let samples: Vec<f32> = (0..total_samples)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                let amp = if (1.4..=1.6).contains(&t) { 0.05 } else { 0.3 };
                ((2.0 * PI * 300.0 * t).sin() * amp) as f32
            })
            .collect();

        for max_sec in [2.0, 2.5, 3.0] {
            let spans = MelFrontend::segment_speech_utterances(&samples, sample_rate as i32, max_sec);
            assert!(!spans.is_empty(), "expected speech spans for max_sec={max_sec}");
            for span in &spans {
                let dur = span.end_sec - span.start_sec;
                assert!(
                    dur <= max_sec + 1e-6,
                    "span duration {dur} exceeded max_sec={max_sec}"
                );
                assert!(
                    dur >= 0.99,
                    "short-split produced a tiny sliver ({dur}s) for max_sec={max_sec}"
                );
            }
        }

        // Regression test: 101 frames (2.02 s) split at max_utterance_sec = 2.0
        // previously inverted `search_start > search_end` and emitted a 0.02 s sliver.
        let sliver_samples = &samples[..sample_rate * 101 / 50];
        let sliver_spans =
            MelFrontend::segment_speech_utterances(sliver_samples, sample_rate as i32, 2.0);
        assert_eq!(sliver_spans.len(), 2);
        for span in &sliver_spans {
            let dur = span.end_sec - span.start_sec;
            assert!((0.99..=2.0 + 1e-6).contains(&dur), "unexpected span duration {dur}");
        }
    }
}
