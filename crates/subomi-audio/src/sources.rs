//! Pronunciation audio download timeout policy and bounded configuration
//! (Yomitan `#1876`) so stalled online audio URLs never block Anki card
//! creation indefinitely.

use std::time::Duration;

/// Minimum allowed pronunciation audio download timeout (`1_000` ms).
pub const MIN_AUDIO_DOWNLOAD_TIMEOUT_MS: u64 = 1_000;

/// Maximum allowed pronunciation audio download timeout (`30_000` ms).
pub const MAX_AUDIO_DOWNLOAD_TIMEOUT_MS: u64 = 30_000;

/// Default pronunciation audio download timeout (`8_000` ms, lowered from 30 s).
pub const DEFAULT_AUDIO_DOWNLOAD_TIMEOUT_MS: u64 = 8_000;

/// Minimum allowed pronunciation audio download timeout as a [`Duration`].
pub const MIN_AUDIO_DOWNLOAD_TIMEOUT: Duration =
    Duration::from_millis(MIN_AUDIO_DOWNLOAD_TIMEOUT_MS);

/// Maximum allowed pronunciation audio download timeout as a [`Duration`].
pub const MAX_AUDIO_DOWNLOAD_TIMEOUT: Duration =
    Duration::from_millis(MAX_AUDIO_DOWNLOAD_TIMEOUT_MS);

/// Default pronunciation audio download timeout (`8 s`).
pub const DEFAULT_AUDIO_DOWNLOAD_TIMEOUT: Duration =
    Duration::from_millis(DEFAULT_AUDIO_DOWNLOAD_TIMEOUT_MS);

/// Default TCP/TLS connect timeout cap (`5 s`), further bounded by the active
/// download timeout.
pub const DEFAULT_AUDIO_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Clamps an arbitrary [`Duration`] into the bounded audio download timeout
/// range `1_000..=30_000` ms.
pub const fn clamp_audio_download_timeout(timeout: Duration) -> Duration {
    let min_secs = MIN_AUDIO_DOWNLOAD_TIMEOUT.as_secs();
    let max_secs = MAX_AUDIO_DOWNLOAD_TIMEOUT.as_secs();
    let secs = timeout.as_secs();
    let nanos = timeout.subsec_nanos();
    if secs < min_secs {
        MIN_AUDIO_DOWNLOAD_TIMEOUT
    } else if secs > max_secs || (secs == max_secs && nanos > 0) {
        MAX_AUDIO_DOWNLOAD_TIMEOUT
    } else {
        timeout
    }
}

/// Converts a millisecond value into a bounded audio download [`Duration`]
/// within `1_000..=30_000` ms.
pub const fn audio_download_timeout_from_ms(timeout_ms: u64) -> Duration {
    let clamped = if timeout_ms < MIN_AUDIO_DOWNLOAD_TIMEOUT_MS {
        MIN_AUDIO_DOWNLOAD_TIMEOUT_MS
    } else if timeout_ms > MAX_AUDIO_DOWNLOAD_TIMEOUT_MS {
        MAX_AUDIO_DOWNLOAD_TIMEOUT_MS
    } else {
        timeout_ms
    };
    Duration::from_millis(clamped)
}

/// Bounded timeout settings for pronunciation audio HTTP requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioDownloadConfig {
    timeout: Duration,
}

impl Default for AudioDownloadConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDownloadConfig {
    /// Creates a new configuration using [`DEFAULT_AUDIO_DOWNLOAD_TIMEOUT`] (8 s).
    pub const fn new() -> Self {
        Self {
            timeout: DEFAULT_AUDIO_DOWNLOAD_TIMEOUT,
        }
    }

    /// Creates a new configuration with `timeout` clamped to `1_000..=30_000` ms.
    pub const fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout: clamp_audio_download_timeout(timeout),
        }
    }

    /// Creates a new configuration from milliseconds clamped to `1_000..=30_000` ms.
    pub const fn with_timeout_ms(timeout_ms: u64) -> Self {
        Self {
            timeout: audio_download_timeout_from_ms(timeout_ms),
        }
    }

    /// Updates the download timeout, clamping to `1_000..=30_000` ms.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = clamp_audio_download_timeout(timeout);
    }

    /// Updates the download timeout from milliseconds, clamping to `1_000..=30_000` ms.
    pub fn set_timeout_ms(&mut self, timeout_ms: u64) {
        self.timeout = audio_download_timeout_from_ms(timeout_ms);
    }

    /// Returns the bounded transfer timeout.
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Returns the effective connect timeout, never exceeding [`Self::timeout`].
    pub const fn connect_timeout(&self) -> Duration {
        let cap_secs = DEFAULT_AUDIO_CONNECT_TIMEOUT.as_secs();
        if self.timeout.as_secs() < cap_secs {
            self.timeout
        } else {
            DEFAULT_AUDIO_CONNECT_TIMEOUT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_audio_timeout_is_eight_seconds_and_bounded() {
        assert_eq!(DEFAULT_AUDIO_DOWNLOAD_TIMEOUT, Duration::from_secs(8));
        assert_eq!(MIN_AUDIO_DOWNLOAD_TIMEOUT, Duration::from_millis(1_000));
        assert_eq!(MAX_AUDIO_DOWNLOAD_TIMEOUT, Duration::from_millis(30_000));

        let cfg = AudioDownloadConfig::default();
        assert_eq!(cfg.timeout(), Duration::from_secs(8));
        assert_eq!(cfg.connect_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn timeout_clamping_enforces_lower_and_upper_bounds() {
        assert_eq!(
            clamp_audio_download_timeout(Duration::ZERO),
            Duration::from_millis(1_000)
        );
        assert_eq!(
            clamp_audio_download_timeout(Duration::from_millis(250)),
            Duration::from_millis(1_000)
        );
        assert_eq!(
            clamp_audio_download_timeout(Duration::from_millis(1_000)),
            Duration::from_millis(1_000)
        );
        assert_eq!(
            clamp_audio_download_timeout(Duration::from_millis(4_500)),
            Duration::from_millis(4_500)
        );
        assert_eq!(
            clamp_audio_download_timeout(Duration::from_millis(30_000)),
            Duration::from_millis(30_000)
        );
        assert_eq!(
            clamp_audio_download_timeout(Duration::from_millis(30_001)),
            Duration::from_millis(30_000)
        );
        assert_eq!(
            clamp_audio_download_timeout(Duration::from_secs(120)),
            Duration::from_millis(30_000)
        );
        assert_eq!(
            audio_download_timeout_from_ms(0),
            Duration::from_millis(1_000)
        );
        assert_eq!(
            audio_download_timeout_from_ms(12_345),
            Duration::from_millis(12_345)
        );
        assert_eq!(
            audio_download_timeout_from_ms(u64::MAX),
            Duration::from_millis(30_000)
        );
    }

    #[test]
    fn audio_download_config_setter_and_connect_cap() {
        let mut cfg = AudioDownloadConfig::with_timeout(Duration::from_millis(2_500));
        assert_eq!(cfg.timeout(), Duration::from_millis(2_500));
        assert_eq!(cfg.connect_timeout(), Duration::from_millis(2_500));

        cfg.set_timeout(Duration::from_secs(60));
        assert_eq!(cfg.timeout(), Duration::from_secs(30));
        assert_eq!(cfg.connect_timeout(), Duration::from_secs(5));

        cfg.set_timeout_ms(500);
        assert_eq!(cfg.timeout(), Duration::from_secs(1));
        assert_eq!(cfg.connect_timeout(), Duration::from_secs(1));

        let ms_cfg = AudioDownloadConfig::with_timeout_ms(15_000);
        assert_eq!(ms_cfg.timeout(), Duration::from_secs(15));
        assert_eq!(ms_cfg.connect_timeout(), Duration::from_secs(5));
    }
}
