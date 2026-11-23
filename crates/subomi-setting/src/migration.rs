//! Settings schema migration planning (`v1` → current).

use crate::enums::GlossaryStyle;

/// Migration actions required to bring a stored settings schema up to date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationPlan {
    pub already_current: bool,
    pub newer_version_warning: bool,
    pub run_1_to_2: bool,
    pub run_2_to_3: bool,
    pub run_3_to_4: bool,
    pub run_4_to_5: bool,
    pub run_5_to_6: bool,
    pub target_version: u32,
}

/// Current settings schema version.
pub const CURRENT_SCHEMA_VERSION: u32 = 6;

/// Computes the sequential migration steps needed for a saved schema `version`.
pub fn migration_plan(version: u32) -> MigrationPlan {
    let current = CURRENT_SCHEMA_VERSION;
    MigrationPlan {
        already_current: version == current,
        newer_version_warning: version > current,
        run_1_to_2: version == 1,
        run_2_to_3: (1..=2).contains(&version),
        run_3_to_4: (1..=3).contains(&version),
        run_4_to_5: (1..=4).contains(&version),
        run_5_to_6: (1..=5).contains(&version),
        target_version: current,
    }
}

/// Maps the v2 boolean `list` setting to the v3 [`GlossaryStyle`] discriminant.
pub fn migrate_2_to_3_list_result(list: bool) -> i32 {
    if list {
        GlossaryStyle::GlossaryStyleBullet as i32
    } else {
        GlossaryStyle::GlossaryStyleLineBreak as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_plan_covers_all_versions() {
        let p0 = migration_plan(0);
        assert!(!p0.already_current && !p0.newer_version_warning && !p0.run_1_to_2 && !p0.run_5_to_6);
        assert_eq!(p0.target_version, 6);

        let p1 = migration_plan(1);
        assert!(p1.run_1_to_2 && p1.run_2_to_3 && p1.run_3_to_4 && p1.run_4_to_5 && p1.run_5_to_6);

        let p3 = migration_plan(3);
        assert!(!p3.run_1_to_2 && !p3.run_2_to_3 && p3.run_3_to_4 && p3.run_4_to_5 && p3.run_5_to_6);

        let p6 = migration_plan(6);
        assert!(p6.already_current && !p6.newer_version_warning && !p6.run_5_to_6);

        let p7 = migration_plan(7);
        assert!(!p7.already_current && p7.newer_version_warning && !p7.run_5_to_6);
    }

    #[test]
    fn migrate_2_to_3_maps_boolean_to_glossary_style() {
        assert_eq!(
            migrate_2_to_3_list_result(true),
            GlossaryStyle::GlossaryStyleBullet as i32
        );
        assert_eq!(
            migrate_2_to_3_list_result(false),
            GlossaryStyle::GlossaryStyleLineBreak as i32
        );
    }
}
