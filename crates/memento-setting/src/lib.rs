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

pub mod capi;
pub mod enums;
pub mod keys;
pub mod migration;
pub mod models;

