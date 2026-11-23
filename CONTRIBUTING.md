# Contributing to Subomi

Contributions, bug reports, and pull requests are welcome.

## Development Setup

See [README.md](README.md#quick-start) for required system libraries (`libmpv`, `libass`, `sqlite3`) and [docs/development.md](docs/development.md) for custom native build overrides and playback timing tests.

## Workspace Layout

* `src/` — Desktop application binary (`subomi`), Slint UI bridge, controllers, and platform renderers.
* `ui/` — Slint UI components (`app.slint`, `palette.slint`, `popup/`, `player/`, `settings/`).
* `crates/subomi-*` — Core Rust libraries (player, subtitles, ASS, dictionaries, structured-content CSS, Anki, networking, config, OS integration, ASR).
* `tests/` — Integration and UI layout tests.

## Code Style & Quality

* **Rust:** Follow standard `rustfmt` formatting (`cargo fmt --check`) and keep `cargo clippy --workspace` clean.
* **Slint:** Reuse design tokens from `ui/palette.slint` (`SubomiPalette`) so both Light and Dark themes stay consistent.
* **Tests:** Run the full workspace test suite before opening a PR:

```bash
cargo test --workspace
```

For dictionary popup geometry checks:

```bash
SLINT_EMIT_DEBUG_INFO=1 cargo test --test dictionary_popup_layout_test
```

## Pull Requests

* Keep PRs focused on a single bug fix or feature where practical.
* Include regression tests for parser, dictionary, config, Anki, or UI state fixes.
* Preserve existing upstream copyright notices on files derived from upstream projects.
