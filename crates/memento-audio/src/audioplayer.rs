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

/// 0: unavailable or cached hash matches; 1: play cached; 2: download.
#[no_mangle]
pub extern "C" fn memento_audio_cache_action(
    mpv_ready: bool,
    cached: bool,
    hashes_equal: bool,
) -> i32 {
    if !mpv_ready {
        0
    } else if !cached {
        2
    } else if hashes_equal {
        0
    } else {
        1
    }
}

#[no_mangle]
pub extern "C" fn memento_audio_cache_evict(cached_count: usize) -> bool {
    cached_count >= 10
}

#[cfg(test)]
mod tests {
    use super::{memento_audio_cache_action, memento_audio_cache_evict};

    #[test]
    fn cache_action_preserves_availability_and_hash_decisions() {
        assert_eq!(memento_audio_cache_action(false, false, false), 0);
        assert_eq!(memento_audio_cache_action(false, true, false), 0);
        assert_eq!(memento_audio_cache_action(false, true, true), 0);
        assert_eq!(memento_audio_cache_action(true, false, false), 2);
        assert_eq!(memento_audio_cache_action(true, false, true), 2);
        assert_eq!(memento_audio_cache_action(true, true, false), 1);
        assert_eq!(memento_audio_cache_action(true, true, true), 0);
    }

    #[test]
    fn cache_evict_at_capacity() {
        assert!(!memento_audio_cache_evict(0));
        assert!(!memento_audio_cache_evict(9));
        assert!(memento_audio_cache_evict(10));
        assert!(memento_audio_cache_evict(11));
    }
}
