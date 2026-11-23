//! Compiled Slint UI components for Subomi.
//!
//! Isolated in its own workspace crate so edits to application Rust code (`src/`)
//! do not re-parse, type-check, or codegen the generated Slint UI tree.

slint::include_modules!();
