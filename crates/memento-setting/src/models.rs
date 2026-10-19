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

//! Pure model calculation logic for `AudioSourceModel`, `KeybindProfileModel`,
//! and `DownloadManager`.

/// Plans an item move within a list of `count` items.
///
/// Matches `AudioSourceModel::move(int from, int to)`:
/// - If `from == to || from < 0 || from >= count || to < 0 || to >= count`: returns `None`.
/// - Destination row before the move: `if to > from { to + 1 } else { to }`.
pub fn audio_source_plan_move(from: i32, to: i32, count: i32) -> Option<i32> {
    if from == to || from < 0 || from >= count || to < 0 || to >= count {
        None
    } else {
        Some(if to > from { to + 1 } else { to })
    }
}

/// Performs a binary search lower bound over an array of sorted UTF-16 names.
///
/// Matches `std::lower_bound` over `profile->name() < name`.
/// Returns `(index, exact_match)`.
pub fn profile_lower_bound<S: AsRef<[u16]>>(names: &[S], target: &[u16]) -> (usize, bool) {
    let idx = names.partition_point(|name| name.as_ref() < target);
    let exact_match = idx < names.len() && names[idx].as_ref() == target;
    (idx, exact_match)
}

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

/// Result of planning a keybind sequence update.
///
/// Matches the decision points of `KeybindProfile::setSequence`:
/// 1. If `seq == newSeq`, returns `Unchanged` (returns `false`, no signal emitted).
/// 2. Else if `m_existingBinds.contains(newSeq.toString())`, returns `ConflictKeepOld`
///    (returns `true`, signal emitted with old sequence, profile untouched).
/// 3. Else returns `Updated` (returns `true`, profile updated, existing binds updated, signal emitted).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceUpdate {
    /// New sequence is identical to current sequence (`seq == newSeq`).
    Unchanged = 0,
    /// New sequence conflicts with another existing bind in the profile.
    ConflictKeepOld = 1,
    /// Sequence successfully updated.
    Updated = 2,
}

impl SequenceUpdate {
    /// Whether the profile should emit a change signal.
    #[inline]
    pub fn should_emit_signal(self) -> bool {
        matches!(self, Self::ConflictKeepOld | Self::Updated)
    }

    /// Whether the profile's sequence should be updated.
    #[inline]
    pub fn should_update_profile(self) -> bool {
        matches!(self, Self::Updated)
    }
}

/// Evaluates the keybind sequence update state transition.
#[inline]
pub fn plan_keybind_sequence(seq_equal: bool, is_conflict: bool) -> SequenceUpdate {
    if seq_equal {
        SequenceUpdate::Unchanged
    } else if is_conflict {
        SequenceUpdate::ConflictKeepOld
    } else {
        SequenceUpdate::Updated
    }
}

/// Evaluates sequence update by checking whether `new_seq` conflicts with any item in `existing_binds`.
pub fn plan_keybind_sequence_lookup<S: AsRef<[u16]>>(
    seq_equal: bool,
    new_seq: &[u16],
    existing_binds: impl IntoIterator<Item = S>,
) -> SequenceUpdate {
    if seq_equal {
        SequenceUpdate::Unchanged
    } else if existing_binds.into_iter().any(|b| b.as_ref() == new_seq) {
        SequenceUpdate::ConflictKeepOld
    } else {
        SequenceUpdate::Updated
    }
}

/// Returns true if UTF-16 slice `requested` exactly matches the ASCII string `ascii`.
#[inline]
pub fn requested_matches_ascii(requested: &[u16], ascii: &str) -> bool {
    requested.len() == ascii.len()
        && requested
            .iter()
            .zip(ascii.bytes())
            .all(|(&r, b)| r == b as u16)
}

/// Resolves the ASR backend given compile flags and requested backend string.
///
/// Matches `Settings::setAsrBackend`:
/// Available backends are determined by `whisper_compiled` and `onnx_compiled`.
/// If both are disabled, falls back to `Keys::Asr::BACKEND_DEFAULT`.
/// If `requested` matches one of the available backends, it is used;
/// otherwise the first available backend (the default) is returned.
pub fn resolve_asr_backend(
    whisper_compiled: bool,
    onnx_compiled: bool,
    requested: &[u16],
) -> &'static str {
    let mut backends: [&'static str; 2] = ["", ""];
    let mut count = 0;
    if whisper_compiled {
        backends[count] = crate::keys::asr::BACKEND_WHISPER;
        count += 1;
    }
    if onnx_compiled {
        backends[count] = crate::keys::asr::BACKEND_ONNX;
        count += 1;
    }
    let available = if count == 0 {
        &[crate::keys::asr::backend_default(whisper_compiled, onnx_compiled)][..]
    } else {
        &backends[..count]
    };
    let default_backend = available[0];

    available
        .iter()
        .copied()
        .find(|&backend| requested_matches_ascii(requested, backend))
        .unwrap_or(default_backend)
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

/// Checks if `requested` is a valid Whisper model.
#[inline]
pub fn is_valid_whisper_model(requested: &[u16]) -> bool {
    WHISPER_MODELS
        .iter()
        .any(|&m| requested_matches_ascii(requested, m))
}

/// Resolves requested Whisper model, returning `(model, use_default)`.
pub fn resolve_whisper_model(requested: &[u16]) -> (&'static str, bool) {
    if let Some(&model) = WHISPER_MODELS
        .iter()
        .find(|&&m| requested_matches_ascii(requested, m))
    {
        (model, false)
    } else {
        (crate::keys::whisper::MODEL_DEFAULT, true)
    }
}

/// Valid ONNX models accepted by `Settings::setOnnxModel`.
pub const ONNX_MODELS: &[&str] = &[
    "kitsune-transcribe-t0.6b-ja",
    "kitsune-transcribe-p0.3b-ja",
    "custom",
];

/// Checks if `requested` is a valid ONNX model.
#[inline]
pub fn is_valid_onnx_model(requested: &[u16]) -> bool {
    ONNX_MODELS
        .iter()
        .any(|&m| requested_matches_ascii(requested, m))
}

/// Resolves requested ONNX model, returning `(model, use_default)`.
pub fn resolve_onnx_model(requested: &[u16]) -> (&'static str, bool) {
    if let Some(&model) = ONNX_MODELS
        .iter()
        .find(|&&m| requested_matches_ascii(requested, m))
    {
        (model, false)
    } else {
        (crate::keys::onnx::MODEL_DEFAULT, true)
    }
}

/// Clamps GPU device index to `[0, 15]`.
///
/// Matches `qBound(0, value, 15)` in `setWhisperGpuDevice` and `setOnnxGpuDevice`.
#[inline]
pub fn clamp_gpu_device(value: i32) -> i32 {
    value.clamp(0, 15)
}

/// Calculates max threads for Whisper/ONNX given Qt's ideal thread count.
///
/// Matches `whisperMaxThreads()`: `qMax(1, QThread::idealThreadCount())`.
#[inline]
pub fn whisper_max_threads(ideal_thread_count: i32) -> i32 {
    ideal_thread_count.max(1)
}

/// Clamps thread count given value and ideal thread count.
///
/// Matches `setWhisperThreads` / `setOnnxThreads`:
/// `value <= 0 ? qMin(4, whisperMaxThreads()) : qBound(1, value, whisperMaxThreads())`.
#[inline]
pub fn clamp_threads(value: i32, ideal_thread_count: i32) -> i32 {
    let max_threads = whisper_max_threads(ideal_thread_count);
    if value <= 0 {
        4.min(max_threads)
    } else {
        value.clamp(1, max_threads)
    }
}

/// Clamps beam size or best_of value to `[1, 10]`.
///
/// Matches `qBound(1, value, 10)` in `setWhisperBestOf` and `setWhisperBeamSize`.
#[inline]
pub fn clamp_beam_or_best_of(value: i32) -> i32 {
    value.clamp(1, 10)
}

/// Maximum capacity for the recent files list.
/// Matches `MAX_SIZE` in `Settings::recentFilesAdd`.
pub const RECENT_FILES_MAX_SIZE: usize = 10;

/// Plans updating a recent files list when prepending `new_val`.
///
/// Matches `Settings::recentFilesAdd(const QString &value)`:
/// 1. Filters out any existing items that equal `new_val`.
/// 2. If the new list length (`1 + surviving.len()`) exceeds `max_size`,
///    pops one element from the back.
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

    if 1 + surviving.len() > max_size {
        surviving.pop();
    }

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

    #[test]
    fn test_audio_source_plan_move() {
        assert_eq!(audio_source_plan_move(0, 0, 3), None);
        assert_eq!(audio_source_plan_move(-1, 1, 3), None);
        assert_eq!(audio_source_plan_move(0, 3, 3), None);
        assert_eq!(audio_source_plan_move(3, 1, 3), None);
        assert_eq!(audio_source_plan_move(0, -1, 3), None);
        assert_eq!(audio_source_plan_move(0, 1, 0), None);

        // Move down (to > from): destination = to + 1
        assert_eq!(audio_source_plan_move(0, 1, 3), Some(2));
        assert_eq!(audio_source_plan_move(0, 2, 3), Some(3));
        assert_eq!(audio_source_plan_move(1, 2, 3), Some(3));

        // Move up (to < from): destination = to
        assert_eq!(audio_source_plan_move(2, 0, 3), Some(0));
        assert_eq!(audio_source_plan_move(2, 1, 3), Some(1));
        assert_eq!(audio_source_plan_move(1, 0, 3), Some(0));
    }

    fn u16_vec(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn test_profile_lower_bound() {
        let empty: Vec<Vec<u16>> = vec![];
        let target = u16_vec("Default");
        assert_eq!(profile_lower_bound(&empty, &target), (0, false));

        let profiles = vec![
            u16_vec("Alpha"),
            u16_vec("Beta"),
            u16_vec("Delta"),
        ];

        // Before first
        assert_eq!(profile_lower_bound(&profiles, &u16_vec("A")), (0, false));
        // Exact match first
        assert_eq!(profile_lower_bound(&profiles, &u16_vec("Alpha")), (0, true));
        // Between first and second
        assert_eq!(profile_lower_bound(&profiles, &u16_vec("Apple")), (1, false));
        // Exact match second
        assert_eq!(profile_lower_bound(&profiles, &u16_vec("Beta")), (1, true));
        // Between second and third
        assert_eq!(profile_lower_bound(&profiles, &u16_vec("Charlie")), (2, false));
        // Exact match third
        assert_eq!(profile_lower_bound(&profiles, &u16_vec("Delta")), (2, true));
        // After third
        assert_eq!(profile_lower_bound(&profiles, &u16_vec("Echo")), (3, false));
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
        assert_eq!(download_speed(1000, 0), 1_000_000); // 0 elapsed clamped to 1
        assert_eq!(download_speed(1000, -10), 1_000_000); // negative clamped to 1
    }

    #[test]
    fn test_download_progress_update() {
        // total == 0 -> progress 0.0
        let (p, changed) = download_progress_update(0.0, 0, 0, 0, 0, 0, 0);
        assert_eq!(p, 0.0);
        assert!(!changed); // 0 == 0

        // Total positive, 50%
        let (p, changed) = download_progress_update(0.0, 0, 0, 0, 50, 100, 1000);
        assert_eq!(p, 0.5);
        assert!(changed);

        // Same state unchanged
        let (p2, changed2) = download_progress_update(0.5, 50, 100, 1000, 50, 100, 1000);
        assert_eq!(p2, 0.5);
        assert!(!changed2);

        // Only speed changed
        let (p3, changed3) = download_progress_update(0.5, 50, 100, 1000, 50, 100, 1200);
        assert_eq!(p3, 0.5);
        assert!(changed3);
    }

    #[test]
    fn test_plan_keybind_sequence() {
        // Equal takes precedence over conflict: Unchanged
        assert_eq!(plan_keybind_sequence(true, false), SequenceUpdate::Unchanged);
        assert_eq!(plan_keybind_sequence(true, true), SequenceUpdate::Unchanged);
        assert!(!SequenceUpdate::Unchanged.should_emit_signal());
        assert!(!SequenceUpdate::Unchanged.should_update_profile());

        // Not equal, but conflict: ConflictKeepOld
        assert_eq!(plan_keybind_sequence(false, true), SequenceUpdate::ConflictKeepOld);
        assert!(SequenceUpdate::ConflictKeepOld.should_emit_signal());
        assert!(!SequenceUpdate::ConflictKeepOld.should_update_profile());

        // Not equal and no conflict: Updated
        assert_eq!(plan_keybind_sequence(false, false), SequenceUpdate::Updated);
        assert!(SequenceUpdate::Updated.should_emit_signal());
        assert!(SequenceUpdate::Updated.should_update_profile());

        // Lookup helper test
        let binds = vec![u16_vec("Ctrl+C"), u16_vec("Ctrl+V")];
        assert_eq!(
            plan_keybind_sequence_lookup(true, &u16_vec("Ctrl+C"), &binds),
            SequenceUpdate::Unchanged
        );
        assert_eq!(
            plan_keybind_sequence_lookup(false, &u16_vec("Ctrl+C"), &binds),
            SequenceUpdate::ConflictKeepOld
        );
        assert_eq!(
            plan_keybind_sequence_lookup(false, &u16_vec("Ctrl+X"), &binds),
            SequenceUpdate::Updated
        );
    }

    #[test]
    fn test_resolve_asr_backend() {
        let whisper_u16 = u16_vec("whisper");
        let onnx_u16 = u16_vec("onnx");
        let invalid_u16 = u16_vec("other");

        // Both compiled: available = ["whisper", "onnx"], default = "whisper"
        assert_eq!(resolve_asr_backend(true, true, &whisper_u16), "whisper");
        assert_eq!(resolve_asr_backend(true, true, &onnx_u16), "onnx");
        assert_eq!(resolve_asr_backend(true, true, &invalid_u16), "whisper");

        // Whisper only: available = ["whisper"], default = "whisper"
        assert_eq!(resolve_asr_backend(true, false, &whisper_u16), "whisper");
        assert_eq!(resolve_asr_backend(true, false, &onnx_u16), "whisper");
        assert_eq!(resolve_asr_backend(true, false, &invalid_u16), "whisper");

        // Onnx only: available = ["onnx"], default = "onnx"
        assert_eq!(resolve_asr_backend(false, true, &whisper_u16), "onnx");
        assert_eq!(resolve_asr_backend(false, true, &onnx_u16), "onnx");
        assert_eq!(resolve_asr_backend(false, true, &invalid_u16), "onnx");

        // Neither: fallback to backend_default(false, false) = "whisper"
        assert_eq!(resolve_asr_backend(false, false, &whisper_u16), "whisper");
        assert_eq!(resolve_asr_backend(false, false, &onnx_u16), "whisper");
        assert_eq!(resolve_asr_backend(false, false, &invalid_u16), "whisper");
    }

    #[test]
    fn test_resolve_whisper_model() {
        for &m in WHISPER_MODELS {
            let u = u16_vec(m);
            assert!(is_valid_whisper_model(&u));
            assert_eq!(resolve_whisper_model(&u), (m, false));
        }

        let invalid = [u16_vec(""), u16_vec("base-en"), u16_vec("large"), u16_vec("unknown")];
        for inv in &invalid {
            assert!(!is_valid_whisper_model(inv));
            assert_eq!(resolve_whisper_model(inv), ("base", true));
        }
    }

    #[test]
    fn test_resolve_onnx_model() {
        for &m in ONNX_MODELS {
            let u = u16_vec(m);
            assert!(is_valid_onnx_model(&u));
            assert_eq!(resolve_onnx_model(&u), (m, false));
        }

        let invalid = [u16_vec(""), u16_vec("base"), u16_vec("kitsune"), u16_vec("whisper")];
        for inv in &invalid {
            assert!(!is_valid_onnx_model(inv));
            assert_eq!(resolve_onnx_model(inv), ("kitsune-transcribe-t0.6b-ja", true));
        }
    }

    #[test]
    fn test_clamping_functions() {
        // GPU device
        assert_eq!(clamp_gpu_device(-10), 0);
        assert_eq!(clamp_gpu_device(0), 0);
        assert_eq!(clamp_gpu_device(7), 7);
        assert_eq!(clamp_gpu_device(15), 15);
        assert_eq!(clamp_gpu_device(16), 15);
        assert_eq!(clamp_gpu_device(100), 15);

        // Whisper max threads
        assert_eq!(whisper_max_threads(-2), 1);
        assert_eq!(whisper_max_threads(0), 1);
        assert_eq!(whisper_max_threads(1), 1);
        assert_eq!(whisper_max_threads(8), 8);

        // Threads clamping
        // value <= 0 -> 4.min(max_threads)
        assert_eq!(clamp_threads(0, 8), 4);
        assert_eq!(clamp_threads(-1, 8), 4);
        assert_eq!(clamp_threads(0, 2), 2); // 4.min(2) = 2
        // value > 0 -> value.clamp(1, max_threads)
        assert_eq!(clamp_threads(1, 8), 1);
        assert_eq!(clamp_threads(5, 8), 5);
        assert_eq!(clamp_threads(16, 8), 8);
        assert_eq!(clamp_threads(5, 1), 1);

        // Beam / best of
        assert_eq!(clamp_beam_or_best_of(-5), 1);
        assert_eq!(clamp_beam_or_best_of(0), 1);
        assert_eq!(clamp_beam_or_best_of(1), 1);
        assert_eq!(clamp_beam_or_best_of(5), 5);
        assert_eq!(clamp_beam_or_best_of(10), 10);
        assert_eq!(clamp_beam_or_best_of(11), 10);
    }

    #[test]
    fn test_plan_recent_files_add() {
        // Empty list: prepending to empty list yields no surviving items
        let empty: Vec<Vec<u16>> = vec![];
        let val1 = u16_vec("file1");
        assert_eq!(plan_recent_files_add(&empty, &val1), Vec::<usize>::new());

        // Adding distinct item to non-empty list
        let items = vec![u16_vec("file1"), u16_vec("file2")];
        let val0 = u16_vec("file0");
        assert_eq!(plan_recent_files_add(&items, &val0), vec![0, 1]);

        // Re-adding existing item at front: "file1" removed from index 0
        assert_eq!(plan_recent_files_add(&items, &val1), vec![1]);

        // Re-adding existing item in middle
        let items3 = vec![u16_vec("a"), u16_vec("b"), u16_vec("c")];
        assert_eq!(plan_recent_files_add(&items3, &u16_vec("b")), vec![0, 2]);

        // Re-adding item at end
        assert_eq!(plan_recent_files_add(&items3, &u16_vec("c")), vec![0, 1]);

        // Duplicate entries of new_val in items: all should be removed
        let items_dups = vec![u16_vec("a"), u16_vec("b"), u16_vec("a"), u16_vec("c")];
        assert_eq!(plan_recent_files_add(&items_dups, &u16_vec("a")), vec![1, 3]);

        // Capacity limit: list of 10 items, add 11th distinct item
        let ten_items: Vec<Vec<u16>> = (1..=10).map(|i| u16_vec(&format!("file{}", i))).collect();
        let new_item = u16_vec("file0");
        // Surviving should have 9 items (indices 0..=8), index 9 popped
        assert_eq!(plan_recent_files_add(&ten_items, &new_item), vec![0, 1, 2, 3, 4, 5, 6, 7, 8]);

        // 10 items, re-add item 5: filtered list has 9 items, 1 + 9 <= 10, nothing popped
        let item5 = u16_vec("file5");
        assert_eq!(
            plan_recent_files_add(&ten_items, &item5),
            vec![0, 1, 2, 3, 5, 6, 7, 8, 9]
        );

        // List with >10 items (e.g. 12 items), add new item:
        // C++: if (size > MAX_SIZE) pop_back() -> only pops ONE item!
        let twelve_items: Vec<Vec<u16>> = (1..=12).map(|i| u16_vec(&format!("file{}", i))).collect();
        let plan12 = plan_recent_files_add(&twelve_items, &new_item);
        // 12 surviving items, 1 + 12 = 13 > 10 -> pops 1 item -> 11 surviving items
        assert_eq!(plan12.len(), 11);
        assert_eq!(plan12, (0..11).collect::<Vec<usize>>());
    }
}
