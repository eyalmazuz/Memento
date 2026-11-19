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

#[cfg(test)]
mod tests {
    use super::*;

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
}
