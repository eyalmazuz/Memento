//! Main binary entrypoint for pure-Rust Memento.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    memento_app::app::run()
}
