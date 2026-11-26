//! Compile-time embedded changelog parser for the in-app "What's New" modal.

use std::rc::Rc;

use slint::{ModelRc, SharedString, VecModel};

use crate::app::updater::{BuildChannel, SUBOMI_NIGHTLY_RELEASE_URL};
use crate::ui::{ChangelogSection, SubomiApp};

pub(crate) const EMBEDDED_CHANGELOG_MD: &str =
    include_str!(concat!(env!("OUT_DIR"), "/embedded_changelog.md"));

const SUBOMI_ALL_RELEASES_URL: &str = "https://github.com/eyalmazuz/Subomi/releases";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedChangelogSection {
    pub(crate) heading: String,
    pub(crate) items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedChangelog {
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) sections: Vec<ParsedChangelogSection>,
}

/// Strip Markdown inline formatting (links, commit links, bold/code markers) into clean UI text.
pub(crate) fn clean_markdown_inline(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Markdown link: [label](url) -> label
        if chars[i] == '[' {
            if let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']') {
                let bracket_end = i + 1 + close_bracket;
                if bracket_end + 1 < chars.len() && chars[bracket_end + 1] == '(' {
                    if let Some(close_paren) =
                        chars[bracket_end + 2..].iter().position(|&c| c == ')')
                    {
                        let label: String = chars[i + 1..bracket_end].iter().collect();
                        out.push_str(&label);
                        i = bracket_end + 2 + close_paren + 1;
                        continue;
                    }
                }
            }
        }
        // Bold markers `**`
        if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            continue;
        }
        // Inline backticks
        if chars[i] == '`' {
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }

    // Remove trailing release-please commit hash / PR link parentheticals such as
    // " (566a82d)" or " (#123)" when they sit at the very end of a bullet.
    let mut trimmed = out.trim().to_owned();
    loop {
        let stripped = strip_trailing_ref_paren(&trimmed);
        if stripped.len() == trimmed.len() {
            break;
        }
        trimmed = stripped.trim_end().to_owned();
    }
    trimmed
}

fn strip_trailing_ref_paren(text: &str) -> &str {
    let Some(without_close) = text.strip_suffix(')') else {
        return text;
    };
    let Some(open_idx) = without_close.rfind('(') else {
        return text;
    };
    let inner = without_close[open_idx + 1..].trim();
    let is_hex_sha =
        (7..=40).contains(&inner.len()) && inner.bytes().all(|b| b.is_ascii_hexdigit());
    let is_pr_ref = inner
        .strip_prefix('#')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()));
    if is_hex_sha || is_pr_ref {
        text[..open_idx].trim_end()
    } else {
        text
    }
}

fn parse_release_heading(raw_heading: &str) -> (String, String) {
    let cleaned = clean_markdown_inline(raw_heading);
    let cleaned = cleaned.trim();
    // Handle patterns like:
    // - "[3.0.0] (2026-11-26)" -> version "3.0.0", date "2026-11-26"
    // - "3.0.0 (2026-11-26)" -> version "3.0.0", date "2026-11-26"
    // - "[2.0.2]" -> version "2.0.2", date ""
    // - "Nightly (2026-11-26 · 566a82d)" -> title "Nightly", subtitle "2026-11-26 · 566a82d"
    if let Some((head, tail)) = cleaned.split_once('(') {
        let left = head.trim().trim_matches(|c| c == '[' || c == ']').trim();
        let right = tail.trim_end_matches(')').trim();
        return (left.to_owned(), right.to_owned());
    }
    let left = cleaned.trim_matches(|c| c == '[' || c == ']').trim();
    (left.to_owned(), String::new())
}

/// Parse the top-most release entry from a Markdown changelog (`CHANGELOG.md` or
/// a CI-generated nightly changelog).
pub(crate) fn parse_latest_changelog(
    markdown: &str,
    pkg_version: &str,
    git_hash: Option<&str>,
    build_channel: BuildChannel,
) -> ParsedChangelog {
    let mut header_label = String::new();
    let mut header_date = String::new();
    let mut in_first_release = false;
    let mut saw_any_release_header = false;
    let mut sections: Vec<ParsedChangelogSection> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_items: Vec<String> = Vec::new();

    let flush_section = |heading: &mut Option<String>,
                         items: &mut Vec<String>,
                         out: &mut Vec<ParsedChangelogSection>| {
        if !items.is_empty() {
            out.push(ParsedChangelogSection {
                heading: heading.take().unwrap_or_else(|| "Changes".to_owned()),
                items: std::mem::take(items),
            });
        } else {
            heading.take();
        }
    };

    for raw_line in markdown.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            if saw_any_release_header {
                break;
            }
            saw_any_release_header = true;
            in_first_release = true;
            let (label, date) = parse_release_heading(rest);
            header_label = label;
            header_date = date;
            continue;
        }
        if line.starts_with("# ") {
            continue;
        }
        if saw_any_release_header && !in_first_release {
            break;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            flush_section(&mut current_heading, &mut current_items, &mut sections);
            let h = clean_markdown_inline(rest);
            if !h.is_empty() {
                current_heading = Some(h);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("* ").or_else(|| line.strip_prefix("- ")) {
            let cleaned = clean_markdown_inline(rest);
            if !cleaned.is_empty() {
                current_items.push(cleaned);
            }
        }
    }
    flush_section(&mut current_heading, &mut current_items, &mut sections);

    let short_hash = git_hash.map(str::trim).filter(|h| !h.is_empty());
    let title = match build_channel {
        BuildChannel::Nightly => {
            if header_label.to_ascii_lowercase().contains("nightly") {
                format!("What's New in {header_label}")
            } else {
                "What's New in Nightly".to_owned()
            }
        }
        BuildChannel::Stable | BuildChannel::Dev => {
            let ver = if !header_label.is_empty() {
                header_label.trim_start_matches('v')
            } else {
                pkg_version.trim_start_matches('v')
            };
            format!("What's New in v{ver}")
        }
    };

    let subtitle = match build_channel {
        BuildChannel::Nightly => match (!header_date.is_empty(), short_hash) {
            (true, Some(h)) if !header_date.contains(h) => format!("{header_date} · {h}"),
            (true, _) => header_date,
            (false, Some(h)) => format!("Commit {h} · Base build"),
            (false, None) => "Rolling prerelease from main".to_owned(),
        },
        BuildChannel::Stable => {
            if !header_date.is_empty() {
                format!("Released {header_date}")
            } else {
                format!("Version {pkg_version}")
            }
        }
        BuildChannel::Dev => match (!header_date.is_empty(), short_hash) {
            (true, Some(h)) => format!("Local dev build ({h}) · Last changelog: {header_date}"),
            (false, Some(h)) => format!("Local dev build ({h})"),
            (true, None) => format!("Local dev build · Last changelog: {header_date}"),
            (false, None) => "Local development build".to_owned(),
        },
    };

    ParsedChangelog {
        title,
        subtitle,
        sections,
    }
}

pub(crate) fn whats_new_release_url(pkg_version: &str, build_channel: BuildChannel) -> String {
    match build_channel {
        BuildChannel::Nightly => SUBOMI_NIGHTLY_RELEASE_URL.to_owned(),
        BuildChannel::Stable => format!(
            "https://github.com/eyalmazuz/Subomi/releases/tag/v{}",
            pkg_version.trim_start_matches('v')
        ),
        BuildChannel::Dev => SUBOMI_ALL_RELEASES_URL.to_owned(),
    }
}

pub(crate) fn populate_whats_new(app: &SubomiApp) {
    let channel = BuildChannel::current();
    let version = env!("CARGO_PKG_VERSION");
    let git_hash = option_env!("SUBOMI_GIT_HASH");
    let parsed = parse_latest_changelog(EMBEDDED_CHANGELOG_MD, version, git_hash, channel);

    let slint_sections: Vec<ChangelogSection> = parsed
        .sections
        .into_iter()
        .map(|sec| {
            let items_model: ModelRc<SharedString> = Rc::new(VecModel::from(
                sec.items
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            ))
            .into();
            ChangelogSection {
                heading: sec.heading.into(),
                items: items_model,
            }
        })
        .collect();

    app.set_whats_new_title(parsed.title.into());
    app.set_whats_new_subtitle(parsed.subtitle.into());
    app.set_whats_new_sections(Rc::new(VecModel::from(slint_sections)).into());
    app.set_whats_new_release_url(whats_new_release_url(version, channel).into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_please_markdown_and_strips_links() {
        let md = "\
# Changelog

## [3.0.0](https://github.com/eyalmazuz/Subomi/compare/v2.0.2...v3.0.0) (2026-11-26)

### Features

* **ui:** add keyboard subtitle scanning ([#42](https://github.com/eyalmazuz/Subomi/issues/42)) ([566a82d](https://github.com/eyalmazuz/Subomi/commit/566a82d))
* pure-Rust `manga-ocr` worker

### Bug Fixes

* **popup:** fix `Ctrl+Down` scroll and focus highlight ([1234abc](https://github.com/eyalmazuz/Subomi/commit/1234abc))

## [2.0.2] (2026-10-01)

### Features

* older release item that should not appear
";
        let parsed = parse_latest_changelog(md, "3.0.0", Some("566a82d"), BuildChannel::Stable);
        assert_eq!(parsed.title, "What's New in v3.0.0");
        assert_eq!(parsed.subtitle, "Released 2026-11-26");
        assert_eq!(parsed.sections.len(), 2);
        assert_eq!(parsed.sections[0].heading, "Features");
        assert_eq!(
            parsed.sections[0].items,
            vec![
                "ui: add keyboard subtitle scanning",
                "pure-Rust manga-ocr worker",
            ]
        );
        assert_eq!(parsed.sections[1].heading, "Bug Fixes");
        assert_eq!(
            parsed.sections[1].items,
            vec!["popup: fix Ctrl+Down scroll and focus highlight"]
        );
    }

    #[test]
    fn parses_nightly_changelog_and_urls() {
        let md = "\
## Nightly (2026-11-26 · 566a82d)

### Changes since v3.0.0

- feat(ui): add What's New modal (566a82d)
- fix(updater): support nightly channel (#99)
";
        let parsed = parse_latest_changelog(md, "3.0.0", Some("566a82d"), BuildChannel::Nightly);
        assert_eq!(parsed.title, "What's New in Nightly");
        assert_eq!(parsed.subtitle, "2026-11-26 · 566a82d");
        assert_eq!(parsed.sections.len(), 1);
        assert_eq!(parsed.sections[0].heading, "Changes since v3.0.0");
        assert_eq!(
            parsed.sections[0].items,
            vec![
                "feat(ui): add What's New modal",
                "fix(updater): support nightly channel",
            ]
        );
        assert_eq!(
            whats_new_release_url("3.0.0", BuildChannel::Nightly),
            SUBOMI_NIGHTLY_RELEASE_URL
        );
        assert_eq!(
            whats_new_release_url("3.0.0", BuildChannel::Stable),
            "https://github.com/eyalmazuz/Subomi/releases/tag/v3.0.0"
        );
        assert_eq!(
            whats_new_release_url("3.0.0", BuildChannel::Dev),
            SUBOMI_ALL_RELEASES_URL
        );
    }

    #[test]
    fn populate_whats_new_loads_embedded_changelog_without_auto_opening() {
        i_slint_backend_testing::init_no_event_loop();
        let app = SubomiApp::new().unwrap();
        populate_whats_new(&app);
        assert!(!app.get_whats_new_open());
        assert!(app.get_whats_new_title().as_str().starts_with("What's New"));
        assert!(!app.get_whats_new_release_url().is_empty());
        assert!(
            slint::Model::row_count(&app.get_whats_new_sections()) >= 1,
            "embedded CHANGELOG.md should populate at least one What's New section"
        );
    }
}
