//! Memento pure-Rust desktop application.

pub mod anki_help;
pub mod app;
pub mod controller;
pub mod graphics_device;
pub mod platform_renderer;
pub mod platform_window;
pub mod settings_ui;
pub mod ui;
pub mod video_gl;
pub mod video_metal;

pub use ui::*;

// Crate-internal re-exports expected by `crate::settings_ui`.
pub(crate) use app::paths::{memento_xdg_dir, open_directory_command, spawn_and_reap};
pub(crate) use app::preferences::{
    apply_dictionary_order, refresh_player_preferences, setting_enabled,
};
