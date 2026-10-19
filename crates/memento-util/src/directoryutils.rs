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

//! The UTF-16 path suffix and `QDir::cleanPath` operation in
//! `DirectoryUtils::normalizedWritableLocation`. The writable location itself
//! is still collected from Qt in C++. Operate on code units, not Unicode
//! scalar values: QString also admits isolated surrogate code units.

const NAME: &[u16] = &[b'm' as u16, b'e' as u16, b'm' as u16, b'e' as u16,
                      b'n' as u16, b't' as u16, b'o' as u16];
const DUPLICATE: &[u16] = &[b'm' as u16, b'e' as u16, b'm' as u16, b'e' as u16,
                           b'n' as u16, b't' as u16, b'o' as u16, b'/' as u16,
                           b'm' as u16, b'e' as u16, b'm' as u16, b'e' as u16,
                           b'n' as u16, b't' as u16, b'o' as u16];
const NAME_SLASH: &[u16] = &[b'm' as u16, b'e' as u16, b'm' as u16, b'e' as u16,
                            b'n' as u16, b't' as u16, b'o' as u16, b'/' as u16];
const SLASH: u16 = b'/' as u16;
const DOT: u16 = b'.' as u16;

/// The original C++ suffix tests are intentionally ordered and applied only
/// once. In particular, the first two branches do *not* clean the input.
pub fn normalized_writable_location(path: &[u16]) -> Vec<u16> {
    let capacity = path.len().checked_add(NAME_SLASH.len() + 1)
        .expect("directory path exceeds addressable memory");
    let mut result = vec![0; capacity];
    let len = normalized_writable_location_into(path, &mut result)
        .expect("buffer holds the original path and its suffix");
    result.truncate(len);
    result
}

/// Write directly into caller-owned storage: the C++ bridge supplies at most
/// the source length plus "/memento/". No second allocation or size query is
/// needed on the production path.
pub(crate) fn normalized_writable_location_into(
    path: &[u16], output: &mut [u16],
) -> Option<usize> {
    if output.len() < path.len().checked_add(NAME_SLASH.len() + 1)? {
        return None;
    }
    let shortened = if path.ends_with(DUPLICATE) {
        &path[..path.len() - NAME.len()]
    } else {
        path
    };
    if shortened.ends_with(NAME) {
        output[..shortened.len()].copy_from_slice(shortened);
        output[shortened.len()] = SLASH;
        return Some(shortened.len() + 1);
    }
    if shortened.ends_with(NAME_SLASH) {
        output[..shortened.len()].copy_from_slice(shortened);
        return Some(shortened.len());
    }

    // Port Qt's qt_normalizePathSegments(DefaultNormalization) over the
    // virtual input `shortened + "/memento/"`. Using input indices instead
    // of materializing that string keeps the hot bridge allocation-free.
    // In particular, Qt's "re-root" branch preserves unresolved `..` and
    // some doubled separators; a conventional path-component stack does not.
    let len = shortened.len() + NAME_SLASH.len() + 1;
    let unit = |index: usize| -> u16 {
        if index < shortened.len() {
            shortened[index]
        } else if index == shortened.len() {
            SLASH
        } else if index < len {
            NAME_SLASH[index - shortened.len() - 1]
        } else {
            0
        }
    };
    let root = usize::from(unit(0) == SLASH);
    let mut input = root;
    let mut last_was_slash = true;
    while input < len {
        if last_was_slash && unit(input) == DOT
            && (input + 1 == len || unit(input + 1) == SLASH
                || (unit(input + 1) == DOT
                    && (input + 2 == len || unit(input + 2) == SLASH)))
        {
            break;
        }
        if last_was_slash && unit(input) == SLASH && input > 0 {
            input -= 1;
            break;
        }
        last_was_slash = unit(input) == SLASH;
        input += 1;
    }
    for (index, slot) in output[..input].iter_mut().enumerate() {
        *slot = unit(index);
    }
    if input == len {
        // QDir::cleanPath removes the trailing separator; the utility
        // immediately restores it, so the original normalized input wins.
        return Some(len);
    }

    let mut at = input;
    let mut start = root;
    loop {
        if unit(input) == SLASH {
            output[at] = SLASH;
            at += 1;
            input += 1;
            while input < len && unit(input) == SLASH {
                input += 1;
            }
        }
        let kind = if unit(input) == DOT {
            if input + 1 == len || unit(input + 1) == SLASH {
                1
            } else if unit(input + 1) == DOT
                && (input + 2 == len || unit(input + 2) == SLASH)
            {
                2
            } else {
                0
            }
        } else {
            0
        };
        if kind == 0 {
            while input < len && unit(input) != SLASH {
                output[at] = unit(input);
                at += 1;
                input += 1;
            }
        } else {
            if kind == 2 {
                if at == start {
                    // Cannot resolve a parent above the active root. Keep
                    // this segment and start a new resolution boundary.
                    output[at] = DOT;
                    output[at + 1] = DOT;
                    at += 2;
                    if input + 2 != len {
                        output[at] = SLASH;
                        at += 1;
                        input += 1;
                    }
                    start = at;
                    input += 2;
                    if input >= len {
                        break;
                    }
                    continue;
                }
                if at > start {
                    at -= 1;
                }
                while at > start && output[at - 1] != SLASH {
                    at -= 1;
                }
                input += 2;
            } else {
                input += 1;
            }
            if input != len && at > start && output[at - 1] == SLASH {
                at -= 1;
            }
            if at == start {
                if input != len && unit(input) == SLASH {
                    input += 1;
                }
                while root == 0 && input != len && unit(input) == SLASH {
                    input += 1;
                }
            }
        }
        if input >= len {
            break;
        }
    }
    if at == 0 {
        output[0] = DOT;
        at = 1;
    }
    if at > 1 && output[at - 1] == SLASH {
        at -= 1;
    }
    output[at] = SLASH;
    Some(at + 1)
}
