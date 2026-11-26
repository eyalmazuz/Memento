//! Main binary entrypoint for pure-Rust Subomi.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    subomi_app::app::run()
}
