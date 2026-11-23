//! Main binary entrypoint for pure-Rust Subomi.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    subomi_app::app::run()
}
