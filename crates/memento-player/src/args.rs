//! Pure UTF-8 command-line argument handling for Memento.

use crate::controller::CliOption;

const MAX_TREE_DEPTH: usize = 64;

/// One media/playlist load entry parsed from UTF-8 CLI arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistLoad {
    pub path: String,
    pub options: Vec<String>,
}

/// Complete parsed command-line invocation for Memento.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedArgs {
    pub show_help: bool,
    pub show_version: bool,
    pub global_options: Vec<CliOption>,
    pub loads: Vec<PlaylistLoad>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseArgsError {
    InvalidBrackets,
    MaxDepthExceeded,
}

pub const HELP_TEXT: &str = "\
Usage: memento [options] [url|path]

For more information about commandline arguments, see https://mpv.io/manual/
";

pub fn help_text() -> &'static str {
    HELP_TEXT
}

pub fn version_text(version: &str) -> String {
    format!("memento {version}")
}

/// Quote the value after the first `=` with double quotes, escaping embedded
/// `"` and `\` characters so mpv's `loadfile` option parser never breaks on
/// values containing quotes or trailing backslashes.
fn quote_arg_str(arg: &str) -> String {
    let Some((key, val)) = arg.split_once('=') else {
        return arg.to_owned();
    };
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push_str(key);
    quoted.push_str("=\"");
    for ch in val.chars() {
        if ch == '"' || ch == '\\' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

struct LoadFileNode<'a> {
    entries: Vec<FileEntry<'a>>,
    options: Vec<String>,
}

enum FileEntry<'a> {
    File(&'a str),
    Child(LoadFileNode<'a>),
}

fn build_args_tree<'a>(
    args: &[&'a str],
    index: &mut usize,
    depth: usize,
) -> Result<LoadFileNode<'a>, ParseArgsError> {
    if depth == 0 {
        return Err(ParseArgsError::MaxDepthExceeded);
    }

    let mut node = LoadFileNode {
        entries: Vec::new(),
        options: Vec::new(),
    };
    while let Some(&arg) = args.get(*index) {
        if arg == "--}" {
            *index += 1;
            break;
        }
        if arg == "--{" {
            *index += 1;
            node.entries
                .push(FileEntry::Child(build_args_tree(args, index, depth - 1)?));
        } else {
            if let Some(opt) = arg.strip_prefix("--") {
                node.options.push(quote_arg_str(opt));
            } else {
                node.entries.push(FileEntry::File(arg));
            }
            *index += 1;
        }
    }
    Ok(node)
}

fn flatten_tree<'a>(
    node: &'a LoadFileNode<'_>,
    inherited: &mut Vec<&'a str>,
    loads: &mut Vec<PlaylistLoad>,
) {
    let previous_len = inherited.len();
    inherited.extend(node.options.iter().map(String::as_str));

    for entry in &node.entries {
        match entry {
            FileEntry::File(filename) => loads.push(PlaylistLoad {
                path: (*filename).to_owned(),
                options: inherited.iter().map(|&opt| opt.to_owned()).collect(),
            }),
            FileEntry::Child(child) => flatten_tree(child, inherited, loads),
        }
    }
    inherited.truncate(previous_len);
}

fn parse_load_slice(args: &[&str], out: &mut Vec<PlaylistLoad>) -> Result<(), ParseArgsError> {
    let mut index = 0;
    let root = build_args_tree(args, &mut index, MAX_TREE_DEPTH)?;
    flatten_tree(&root, &mut Vec::new(), out);
    Ok(())
}

/// Parse full CLI arguments (including `argv[0]`), extracting `--help`/`-h`,
/// `--version`/`-V`, `-v` (verbose mpv flag), global `--<mpv-opt>=...` /
/// `--no-<opt>` / `--<flag>` options, and ordered playlist entries with
/// optional nested `--{` / `--}` per-file options.
pub fn parse_args<I, S>(args: I) -> Result<ParsedArgs, ParseArgsError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let raw: Vec<S> = args.into_iter().collect();
    if raw.len() <= 1 {
        return Ok(ParsedArgs::default());
    }

    let mut out = ParsedArgs::default();
    let mut load_args: Vec<&str> = Vec::with_capacity(raw.len() - 1);

    let mut depth = 0usize;
    let mut end_of_options: Option<usize> = None;
    for arg in raw.iter().skip(1).map(S::as_ref) {
        if end_of_options.is_some() {
            load_args.push(arg);
            continue;
        }
        if arg == "--" && depth == 0 {
            end_of_options = Some(load_args.len());
            continue;
        }
        if arg == "--{" {
            depth += 1;
            load_args.push(arg);
            continue;
        }
        if arg == "--}" {
            if depth == 0 {
                return Err(ParseArgsError::InvalidBrackets);
            }
            depth -= 1;
            load_args.push(arg);
            continue;
        }
        if depth > 0 {
            // Preserve per-file options and paths inside `--{ ... --}` for tree flattening.
            load_args.push(arg);
            continue;
        }

        if arg == "--help" || arg == "-h" {
            out.show_help = true;
        } else if arg == "--version" || arg == "-V" {
            out.show_version = true;
        } else if arg == "-v" {
            out.global_options.push(CliOption {
                name: "v".to_owned(),
                value: "yes".to_owned(),
            });
        } else if let Some(rest) = arg.strip_prefix("--") {
            if let Some((name, val)) = rest.split_once('=') {
                if !name.is_empty() {
                    out.global_options.push(CliOption {
                        name: name.to_owned(),
                        value: val.to_owned(),
                    });
                }
            } else if let Some(negated) = rest.strip_prefix("no-") {
                if !negated.is_empty() {
                    out.global_options.push(CliOption {
                        name: negated.to_owned(),
                        value: "no".to_owned(),
                    });
                }
            } else if !rest.is_empty() {
                out.global_options.push(CliOption {
                    name: rest.to_owned(),
                    value: "yes".to_owned(),
                });
            }
        } else {
            load_args.push(arg);
        }
    }

    if depth != 0 {
        return Err(ParseArgsError::InvalidBrackets);
    }

    if let Some(split_idx) = end_of_options {
        // When `--` ended options, reconstruct remaining args verbatim as file loads
        // alongside any earlier grouped loads without misinterpreting `--foo` files.
        parse_load_slice(&load_args[..split_idx], &mut out.loads)?;
        for &path in &load_args[split_idx..] {
            out.loads.push(PlaylistLoad {
                path: path.to_owned(),
                options: Vec::new(),
            });
        }
        return Ok(out);
    }

    parse_load_slice(&load_args, &mut out.loads)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(path: &str, options: &[&str]) -> PlaylistLoad {
        PlaylistLoad {
            path: path.to_owned(),
            options: options.iter().map(|&opt| opt.to_owned()).collect(),
        }
    }

    #[test]
    fn nested_groups_inherit_all_parent_options_in_source_order() {
        let parsed = parse_args([
            "program",
            "--{",
            "first",
            "--root=early",
            "--{",
            "inside-before",
            "--inner=middle",
            "--{",
            "deep",
            "--deep=last",
            "--}",
            "inside-after",
            "--}",
            "--{",
            "sibling",
            "--sibling=yes",
            "--}",
            "last",
            "--root=late",
            "--}",
        ])
        .unwrap();
        assert_eq!(
            parsed.loads,
            vec![
                load("first", &["root=\"early\"", "root=\"late\""]),
                load(
                    "inside-before",
                    &["root=\"early\"", "root=\"late\"", "inner=\"middle\""]
                ),
                load(
                    "deep",
                    &[
                        "root=\"early\"",
                        "root=\"late\"",
                        "inner=\"middle\"",
                        "deep=\"last\"",
                    ]
                ),
                load(
                    "inside-after",
                    &["root=\"early\"", "root=\"late\"", "inner=\"middle\""]
                ),
                load(
                    "sibling",
                    &["root=\"early\"", "root=\"late\"", "sibling=\"yes\""]
                ),
                load("last", &["root=\"early\"", "root=\"late\""]),
            ]
        );
    }

    #[test]
    fn invalid_closings_and_unclosed_groups_are_rejected() {
        for argv in [
            vec!["program", "--}"],
            vec!["program", "--{", "--}", "--}"],
            vec!["program", "file", "--{"],
            vec!["program", "--}", "--{"],
        ] {
            assert_eq!(parse_args(argv), Err(ParseArgsError::InvalidBrackets));
        }
    }

    #[test]
    fn root_depth_64_allows_63_groups_but_not_64_or_65() {
        for depth in [63, 64, 65] {
            let mut argv = vec!["program"];
            argv.extend(std::iter::repeat_n("--{", depth));
            argv.push("file");
            argv.extend(std::iter::repeat_n("--}", depth));
            if depth == 63 {
                assert_eq!(parse_args(argv).unwrap().loads, vec![load("file", &[])]);
            } else {
                assert_eq!(parse_args(argv), Err(ParseArgsError::MaxDepthExceeded));
            }
        }
    }

    #[test]
    fn quote_arg_escapes_quotes_and_backslashes_after_first_equals() {
        assert_eq!(quote_arg_str("k=a=b\"c\\"), "k=\"a=b\\\"c\\\\\"");
        assert_eq!(quote_arg_str("flag"), "flag");
    }

    #[test]
    fn parse_args_handles_help_version_mpv_options_and_playlists() {
        let parsed = parse_args([
            "memento",
            "--help",
            "--version",
            "--hwdec=vaapi",
            "--no-sub-visibility",
            "--pause",
            "ep1.mkv",
            "--{",
            "--start=12.5",
            "ep2.mkv",
            "--}",
            "ep3.mkv",
        ])
        .unwrap();
        assert!(parsed.show_help);
        assert!(parsed.show_version);
        assert_eq!(
            parsed.global_options,
            vec![
                CliOption {
                    name: "hwdec".into(),
                    value: "vaapi".into()
                },
                CliOption {
                    name: "sub-visibility".into(),
                    value: "no".into()
                },
                CliOption {
                    name: "pause".into(),
                    value: "yes".into()
                },
            ]
        );
        assert_eq!(
            parsed.loads,
            vec![
                PlaylistLoad {
                    path: "ep1.mkv".into(),
                    options: vec![]
                },
                PlaylistLoad {
                    path: "ep2.mkv".into(),
                    options: vec!["start=\"12.5\"".into()]
                },
                PlaylistLoad {
                    path: "ep3.mkv".into(),
                    options: vec![]
                },
            ]
        );

        let short = parse_args(["memento", "-h", "-V", "-v", "--", "--not-an-option.mkv"]).unwrap();
        assert!(short.show_help);
        assert!(short.show_version);
        assert_eq!(
            short.global_options,
            vec![CliOption {
                name: "v".into(),
                value: "yes".into()
            }]
        );
        assert_eq!(
            short.loads,
            vec![PlaylistLoad {
                path: "--not-an-option.mkv".into(),
                options: vec![]
            }]
        );

        let verbose_only = parse_args(["memento", "-v", "ep1.mkv"]).unwrap();
        assert!(!verbose_only.show_version);
        assert_eq!(
            verbose_only.global_options,
            vec![CliOption {
                name: "v".into(),
                value: "yes".into()
            }]
        );
        assert_eq!(
            verbose_only.loads,
            vec![PlaylistLoad {
                path: "ep1.mkv".into(),
                options: vec![]
            }]
        );

        assert_eq!(
            parse_args(["memento", "--{", "a.mkv"]),
            Err(ParseArgsError::InvalidBrackets)
        );

        let negated_eq = parse_args(["memento", "--no-flag=yes", "--no-"]).unwrap();
        assert_eq!(
            negated_eq.global_options,
            vec![CliOption {
                name: "no-flag".into(),
                value: "yes".into(),
            }]
        );
    }
}
