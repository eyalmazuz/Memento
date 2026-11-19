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

//! Pure model calculation logic and model registries shared across
//! `memento-config`, `memento-net`, and `src/`.

/// Matches Qt 6's `qFuzzyCompare(double p1, double p2)`:
/// `qAbs(p1 - p2) * 1000000000000. <= qMin(qAbs(p1), qAbs(p2))`
#[inline]
pub fn q_fuzzy_compare_f64(p1: f64, p2: f64) -> bool {
    (p1 - p2).abs() * 1_000_000_000_000.0 <= p1.abs().min(p2.abs())
}

/// Computes download speed in bytes/second from bytes received and elapsed milliseconds.
/// Matches `qMax<qint64>(1, timer.elapsed())` and `(received * 1000) / elapsed`.
#[inline]
pub fn download_speed(received: i64, elapsed_ms: i64) -> i64 {
    let elapsed = elapsed_ms.max(1);
    (received.saturating_mul(1000)) / elapsed
}

/// Evaluates if download progress state has changed and computes the new progress fraction.
/// Matches `DownloadManager::setDownloadProgress`.
pub fn download_progress_update(
    prev_progress: f64,
    prev_received: i64,
    prev_total: i64,
    prev_speed: i64,
    received: i64,
    total: i64,
    speed: i64,
) -> (f64, bool) {
    let progress = if total > 0 {
        (received as f64) / (total as f64)
    } else {
        0.0
    };
    let changed = !q_fuzzy_compare_f64(prev_progress, progress)
        || prev_received != received
        || prev_total != total
        || prev_speed != speed;
    (progress, changed)
}

/// Valid Whisper models accepted by `Settings::setWhisperModel`.
pub const WHISPER_MODELS: &[&str] = &[
    "tiny",
    "base",
    "small",
    "medium",
    "large-v3",
    "large-v3-turbo",
    "custom",
];

/// Valid ONNX models accepted by `Settings::setOnnxModel`.
pub const ONNX_MODELS: &[&str] = &[
    "kitsune-transcribe-t0.6b-ja",
    "kitsune-transcribe-p0.3b-ja",
    "custom",
];

/// Maximum capacity for the recent files list.
/// Matches `MAX_SIZE` in `Settings::recentFilesAdd`.
pub const RECENT_FILES_MAX_SIZE: usize = 10;

/// Plans updating a recent files list when prepending `new_val`.
///
/// 1. Filters out any existing items that equal `new_val`.
/// 2. Truncates the surviving items so the resulting list (`1 + surviving.len()`)
///    never exceeds `max_size`.
///
/// Returns the surviving original indices from `items` that should follow `new_val`.
pub fn plan_recent_files_add_with_limit<T: AsRef<[u16]>>(
    items: &[T],
    new_val: &[u16],
    max_size: usize,
) -> Vec<usize> {
    let mut surviving: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.as_ref() != new_val)
        .map(|(idx, _)| idx)
        .collect();

    surviving.truncate(max_size.saturating_sub(1));

    surviving
}

/// Plans updating a recent files list when prepending `new_val` with default max size of 10.
pub fn plan_recent_files_add<T: AsRef<[u16]>>(
    items: &[T],
    new_val: &[u16],
) -> Vec<usize> {
    plan_recent_files_add_with_limit(items, new_val, RECENT_FILES_MAX_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_vec(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn test_q_fuzzy_compare_f64() {
        assert!(q_fuzzy_compare_f64(0.0, 0.0));
        assert!(q_fuzzy_compare_f64(1.0, 1.0));
        assert!(q_fuzzy_compare_f64(1.0, 1.0 + 1e-13));
        assert!(!q_fuzzy_compare_f64(1.0, 1.01));
        assert!(!q_fuzzy_compare_f64(0.0, 0.0001));
    }

    #[test]
    fn test_download_speed() {
        assert_eq!(download_speed(1000, 1000), 1000);
        assert_eq!(download_speed(1000, 500), 2000);
        assert_eq!(download_speed(1000, 0), 1_000_000);
        assert_eq!(download_speed(1000, -10), 1_000_000);
    }

    #[test]
    fn test_download_progress_update() {
        let (p, changed) = download_progress_update(0.0, 0, 0, 0, 0, 0, 0);
        assert_eq!(p, 0.0);
        assert!(!changed);

        let (p, changed) = download_progress_update(0.0, 0, 0, 0, 50, 100, 1000);
        assert_eq!(p, 0.5);
        assert!(changed);

        let (p2, changed2) = download_progress_update(0.5, 50, 100, 1000, 50, 100, 1000);
        assert_eq!(p2, 0.5);
        assert!(!changed2);

        let (p3, changed3) = download_progress_update(0.5, 50, 100, 1000, 50, 100, 1200);
        assert_eq!(p3, 0.5);
        assert!(changed3);
    }

    #[test]
    fn test_plan_recent_files_add() {
        let empty: Vec<Vec<u16>> = vec![];
        let val1 = u16_vec("file1");
        assert_eq!(plan_recent_files_add(&empty, &val1), Vec::<usize>::new());

        let items = vec![u16_vec("file1"), u16_vec("file2")];
        let val0 = u16_vec("file0");
        assert_eq!(plan_recent_files_add(&items, &val0), vec![0, 1]);
        assert_eq!(plan_recent_files_add(&items, &val1), vec![1]);

        let items3 = vec![u16_vec("a"), u16_vec("b"), u16_vec("c")];
        assert_eq!(plan_recent_files_add(&items3, &u16_vec("b")), vec![0, 2]);
        assert_eq!(plan_recent_files_add(&items3, &u16_vec("c")), vec![0, 1]);

        let items_dups = vec![u16_vec("a"), u16_vec("b"), u16_vec("a"), u16_vec("c")];
        assert_eq!(plan_recent_files_add(&items_dups, &u16_vec("a")), vec![1, 3]);

        let ten_items: Vec<Vec<u16>> = (1..=10).map(|i| u16_vec(&format!("file{i}"))).collect();
        let new_item = u16_vec("file0");
        assert_eq!(plan_recent_files_add(&ten_items, &new_item), vec![0, 1, 2, 3, 4, 5, 6, 7, 8]);

        let item5 = u16_vec("file5");
        assert_eq!(
            plan_recent_files_add(&ten_items, &item5),
            vec![0, 1, 2, 3, 5, 6, 7, 8, 9]
        );

        // Oversized list (> max_size) is truncated down to max_size - 1 surviving entries.
        let twelve_items: Vec<Vec<u16>> = (1..=12).map(|i| u16_vec(&format!("file{i}"))).collect();
        let plan12 = plan_recent_files_add(&twelve_items, &new_item);
        assert_eq!(plan12, (0..9).collect::<Vec<usize>>());

        // Limit of 0 or 1 leaves no surviving tail.
        assert!(plan_recent_files_add_with_limit(&items, &val0, 1).is_empty());
        assert!(plan_recent_files_add_with_limit(&items, &val0, 0).is_empty());
    }
}
