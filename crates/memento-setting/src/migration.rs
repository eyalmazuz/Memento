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

//! Pure migration logic ported from `src/setting/migration.cpp`.

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MementoMigrationPlanC {
    pub already_current: bool,
    pub newer_version_warning: bool,
    pub run_1_to_2: bool,
    pub run_2_to_3: bool,
    pub run_3_to_4: bool,
    pub run_4_to_5: bool,
    pub run_5_to_6: bool,
    pub target_version: u32,
}

/// Compute which migration steps to execute for `version`.
pub fn migration_plan(version: u32) -> MementoMigrationPlanC {
    let current = crate::keys::version::CURRENT;
    MementoMigrationPlanC {
        already_current: version == current,
        newer_version_warning: version > current,
        run_1_to_2: version == 1,
        run_2_to_3: version >= 1 && version <= 2,
        run_3_to_4: version >= 1 && version <= 3,
        run_4_to_5: version >= 1 && version <= 4,
        run_5_to_6: version >= 1 && version <= 5,
        target_version: current,
    }
}

/// Maps boolean list setting from version 2 to GlossaryStyle enum discriminant.
pub fn migrate_2_to_3_list_result(list: bool) -> i32 {
    if list {
        crate::enums::GlossaryStyle::GlossaryStyleBullet as i32
    } else {
        crate::enums::GlossaryStyle::GlossaryStyleLineBreak as i32
    }
}

const NAME: &[u16] = &[
    b'm' as u16, b'e' as u16, b'm' as u16, b'e' as u16,
    b'n' as u16, b't' as u16, b'o' as u16,
];
const DUPLICATE: &[u16] = &[
    b'm' as u16, b'e' as u16, b'm' as u16, b'e' as u16,
    b'n' as u16, b't' as u16, b'o' as u16, b'/' as u16,
    b'm' as u16, b'e' as u16, b'm' as u16, b'e' as u16,
    b'n' as u16, b't' as u16, b'o' as u16,
];
const NAME_SLASH: &[u16] = &[
    b'm' as u16, b'e' as u16, b'm' as u16, b'e' as u16,
    b'n' as u16, b't' as u16, b'o' as u16, b'/' as u16,
];
const SLASH: u16 = b'/' as u16;
const DOT: u16 = b'.' as u16;

/// Replicates `writeablePath` in `migrate5to6()` (lines 237–259 of `src/setting/migration.cpp`):
/// - If `path` ends with `"memento/memento"`, chops `7` code units (`sizeof("memento") - 1`).
/// - If `path` now ends with `"memento"`, appends `'/'`.
/// - Else if `path` does not end with `"memento/"`, appends `"/memento/"` and normalizes path segments.
pub fn normalize_writable_path(path: &[u16]) -> Vec<u16> {
    let capacity = path
        .len()
        .checked_add(NAME_SLASH.len() + 1)
        .expect("directory path exceeds addressable memory");
    let mut result = vec![0; capacity];
    let len = normalize_writable_path_into(path, &mut result)
        .expect("buffer holds the original path and its suffix");
    result.truncate(len);
    result
}

pub(crate) fn normalize_writable_path_into(
    path: &[u16],
    output: &mut [u16],
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
    // virtual input `shortened + "/memento/"`.
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
        if last_was_slash
            && unit(input) == DOT
            && (input + 1 == len
                || unit(input + 1) == SLASH
                || (unit(input + 1) == DOT && (input + 2 == len || unit(input + 2) == SLASH)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn to_u16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn from_u16(u: &[u16]) -> String {
        String::from_utf16_lossy(u)
    }

    #[test]
    fn test_migration_plan() {
        let p0 = migration_plan(0);
        assert!(!p0.already_current);
        assert!(!p0.newer_version_warning);
        assert!(!p0.run_1_to_2);
        assert!(!p0.run_2_to_3);
        assert!(!p0.run_3_to_4);
        assert!(!p0.run_4_to_5);
        assert!(!p0.run_5_to_6);
        assert_eq!(p0.target_version, 6);

        let p1 = migration_plan(1);
        assert!(!p1.already_current);
        assert!(!p1.newer_version_warning);
        assert!(p1.run_1_to_2);
        assert!(p1.run_2_to_3);
        assert!(p1.run_3_to_4);
        assert!(p1.run_4_to_5);
        assert!(p1.run_5_to_6);
        assert_eq!(p1.target_version, 6);

        let p2 = migration_plan(2);
        assert!(!p2.run_1_to_2);
        assert!(p2.run_2_to_3);
        assert!(p2.run_3_to_4);
        assert!(p2.run_4_to_5);
        assert!(p2.run_5_to_6);

        let p3 = migration_plan(3);
        assert!(!p3.run_1_to_2);
        assert!(!p3.run_2_to_3);
        assert!(p3.run_3_to_4);
        assert!(p3.run_4_to_5);
        assert!(p3.run_5_to_6);

        let p4 = migration_plan(4);
        assert!(!p4.run_1_to_2);
        assert!(!p4.run_2_to_3);
        assert!(!p4.run_3_to_4);
        assert!(p4.run_4_to_5);
        assert!(p4.run_5_to_6);

        let p5 = migration_plan(5);
        assert!(!p5.run_1_to_2);
        assert!(!p5.run_2_to_3);
        assert!(!p5.run_3_to_4);
        assert!(!p5.run_4_to_5);
        assert!(p5.run_5_to_6);

        let p6 = migration_plan(6);
        assert!(p6.already_current);
        assert!(!p6.newer_version_warning);
        assert!(!p6.run_1_to_2);
        assert!(!p6.run_5_to_6);

        let p7 = migration_plan(7);
        assert!(!p7.already_current);
        assert!(p7.newer_version_warning);
        assert!(!p7.run_1_to_2);
        assert!(!p7.run_5_to_6);
    }

    #[test]
    fn test_migrate_2_to_3_list_result() {
        assert_eq!(
            migrate_2_to_3_list_result(true),
            crate::enums::GlossaryStyle::GlossaryStyleBullet as i32
        );
        assert_eq!(
            migrate_2_to_3_list_result(false),
            crate::enums::GlossaryStyle::GlossaryStyleLineBreak as i32
        );
    }

    #[test]
    fn test_normalize_writable_path() {
        // Ends with memento/memento -> chopped 7 -> ends with memento/
        let r1 = normalize_writable_path(&to_u16("/home/user/.config/memento/memento"));
        assert_eq!(from_u16(&r1), "/home/user/.config/memento/");

        // Ends with memento -> appends /
        let r2 = normalize_writable_path(&to_u16("/home/user/.config/memento"));
        assert_eq!(from_u16(&r2), "/home/user/.config/memento/");

        // Already ends with memento/
        let r3 = normalize_writable_path(&to_u16("/home/user/.config/memento/"));
        assert_eq!(from_u16(&r3), "/home/user/.config/memento/");

        // Needs /memento/ and cleaning
        let r4 = normalize_writable_path(&to_u16("/home/user/.config"));
        assert_eq!(from_u16(&r4), "/home/user/.config/memento/");

        let r5 = normalize_writable_path(&to_u16("/home/user/../app"));
        assert_eq!(from_u16(&r5), "/home/app/memento/");
    }
}
