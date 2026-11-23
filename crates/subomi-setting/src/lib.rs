//! The pure constant/data layer of `src/setting/keys.h` (544 lines,
//! `Q_NAMESPACE`) ported for the shell side.
//!
//! Only what is representable without Qt is ported here:
//!
//! * the `Setting` namespace enums (`Q_ENUM_NS`) — `src/enums.rs`;
//! * the `constexpr` settings key names, group names, and the
//!   string/int/bool/double/enum-typed defaults — `src/keys.rs`.
//!
//! Everything requiring Qt types (QFont typography defaults,
//! QStandardPaths derived paths, conditionals on CMake feature defines)
//! stays in C++ and is documented where the corresponding Rust key lives.

pub mod enums;
pub mod keys;
pub mod migration;
pub mod models;

