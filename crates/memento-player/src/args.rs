//! Pure command-line argument handling for `MpvController::loadArgs`.
//!
//! Strings remain UTF-16 code units throughout: Qt's `QString` comparisons,
//! `startsWith`, `mid`, and `insert` do not repair unpaired surrogates.

const OPEN_GROUP: &[u16] = &[b'-' as u16, b'-' as u16, b'{' as u16];
const CLOSE_GROUP: &[u16] = &[b'-' as u16, b'-' as u16, b'}' as u16];
const OPTION_PREFIX: &[u16] = &[b'-' as u16, b'-' as u16];
const MAX_TREE_DEPTH: usize = 64;

/// One call to `loadFile(filename, true, options)`, in invocation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadFileArgs {
    pub filename: Vec<u16>,
    pub options: Vec<Vec<u16>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseArgsError {
    InvalidBrackets,
    MaxDepthExceeded,
}

/// Check the exact `--{`/`--}` bracket balance, ignoring argv[0].
///
/// This deliberately does not enforce the tree's depth limit; that is checked
/// when parsing, just as in the original command-line loader.
pub fn args_valid(args: &[Vec<u16>]) -> bool {
    let mut depth = 0usize;
    for arg in args.iter().skip(1) {
        if arg.as_slice() == OPEN_GROUP {
            depth += 1;
        } else if arg.as_slice() == CLOSE_GROUP {
            if depth == 0 {
                return false;
            }
            depth -= 1;
        }
    }
    depth == 0
}

/// Insert double quotes immediately after the first `=`, then at the end.
///
/// Like `MpvController::quoteArg`, this neither escapes existing quotes nor
/// alters the code units before or after the first equals sign.
pub fn quote_arg(arg: &[u16]) -> Vec<u16> {
    let Some(index) = arg.iter().position(|&unit| unit == b'=' as u16) else {
        return arg.to_vec();
    };

    let mut quoted = Vec::with_capacity(arg.len() + 2);
    quoted.extend_from_slice(&arg[..=index]);
    quoted.push(b'"' as u16);
    quoted.extend_from_slice(&arg[index + 1..]);
    quoted.push(b'"' as u16);
    quoted
}

/// Interpret the complete argv list (including argv[0]) as file/option pairs.
///
/// The root has depth 64, so at most 63 nested `--{` groups are accepted.
/// Options at a level apply to every file there, even files appearing before
/// the option or before/after a child group. Child groups inherit all ancestor
/// options; sibling groups never inherit one another's options. A literal
/// `--` is an empty option, *not* a filename or group marker.
///
/// An empty list or one containing only argv[0] produces no loads; callers
/// implementing `loadArgs` must separately return false for those inputs.
pub fn parse_load_args(args: &[Vec<u16>]) -> Result<Vec<LoadFileArgs>, ParseArgsError> {
    if !args_valid(args) {
        return Err(ParseArgsError::InvalidBrackets);
    }

    let mut index = 1;
    let root = build_args_tree(args, &mut index, MAX_TREE_DEPTH)?;
    let mut loads = Vec::new();
    flatten_tree(&root, &mut Vec::new(), &mut loads);
    Ok(loads)
}

struct LoadFileNode<'a> {
    entries: Vec<FileEntry<'a>>,
    options: Vec<Vec<u16>>,
}

enum FileEntry<'a> {
    File(&'a [u16]),
    Child(LoadFileNode<'a>),
}

fn build_args_tree<'a>(
    args: &'a [Vec<u16>],
    index: &mut usize,
    depth: usize,
) -> Result<LoadFileNode<'a>, ParseArgsError> {
    // This is checked before examining the next token: even an empty group
    // beyond the limit exceeds C++ buildArgsTree's recursion depth.
    if depth == 0 {
        return Err(ParseArgsError::MaxDepthExceeded);
    }

    let mut node = LoadFileNode {
        entries: Vec::new(),
        options: Vec::new(),
    };
    while let Some(arg) = args.get(*index) {
        if arg.as_slice() == CLOSE_GROUP {
            *index += 1;
            break;
        }
        if arg.as_slice() == OPEN_GROUP {
            *index += 1;
            node.entries
                .push(FileEntry::Child(build_args_tree(args, index, depth - 1)?));
        } else {
            if arg.starts_with(OPTION_PREFIX) {
                node.options.push(quote_arg(&arg[OPTION_PREFIX.len()..]));
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
    inherited: &mut Vec<&'a [u16]>,
    loads: &mut Vec<LoadFileArgs>,
) {
    let previous_len = inherited.len();
    inherited.extend(node.options.iter().map(Vec::as_slice));

    for entry in &node.entries {
        match entry {
            FileEntry::File(filename) => loads.push(LoadFileArgs {
                filename: filename.to_vec(),
                options: inherited.iter().map(|option| option.to_vec()).collect(),
            }),
            FileEntry::Child(child) => flatten_tree(child, inherited, loads),
        }
    }
    inherited.truncate(previous_len);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    fn args(values: &[&str]) -> Vec<Vec<u16>> {
        values.iter().map(|value| utf16(value)).collect()
    }

    fn load(filename: &str, options: &[&str]) -> LoadFileArgs {
        LoadFileArgs {
            filename: utf16(filename),
            options: options.iter().map(|option| utf16(option)).collect(),
        }
    }

    #[test]
    fn nested_groups_inherit_all_parent_options_in_source_order() {
        let argv = args(&[
            "program",
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
        ]);
        assert_eq!(
            parse_load_args(&argv),
            Ok(vec![
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
            ])
        );
    }

    #[test]
    fn invalid_closings_and_unclosed_groups_are_rejected() {
        for argv in [
            args(&["program", "--}"]),
            args(&["program", "--{", "--}", "--}"]),
            args(&["program", "file", "--{"]),
            args(&["program", "--}", "--{"]),
        ] {
            assert!(!args_valid(&argv));
            assert_eq!(parse_load_args(&argv), Err(ParseArgsError::InvalidBrackets));
        }
    }

    #[test]
    fn root_depth_64_allows_63_groups_but_not_64_or_65() {
        for depth in [63, 64, 65] {
            let mut argv = vec![utf16("program")];
            argv.extend((0..depth).map(|_| utf16("--{")));
            argv.push(utf16("file"));
            argv.extend((0..depth).map(|_| utf16("--}")));
            assert!(args_valid(&argv));
            if depth == 63 {
                assert_eq!(parse_load_args(&argv), Ok(vec![load("file", &[])]));
            } else {
                assert_eq!(parse_load_args(&argv), Err(ParseArgsError::MaxDepthExceeded));
            }
        }
    }

    #[test]
    fn quote_arg_preserves_raw_utf16_and_quotes_only_first_equals() {
        let raw = [0xd800, b'=' as u16, 0xdc00, b'=' as u16, b'"' as u16];
        assert_eq!(
            quote_arg(&raw),
            [0xd800, b'=' as u16, b'"' as u16, 0xdc00, b'=' as u16, b'"' as u16, b'"' as u16]
        );
        assert_eq!(quote_arg(&[0xd800, 0xdc00]), [0xd800, 0xdc00]);

        let mut argv = args(&["program"]);
        argv.push(vec![0xd800]);
        argv.push(vec![b'-' as u16, b'-' as u16, b'x' as u16, b'=' as u16, 0xdc00]);
        assert_eq!(
            parse_load_args(&argv),
            Ok(vec![LoadFileArgs {
                filename: vec![0xd800],
                options: vec![vec![b'x' as u16, b'=' as u16, b'"' as u16, 0xdc00, b'"' as u16]],
            }])
        );
    }

    #[test]
    fn exact_brackets_and_literal_dash_dash_option() {
        let argv = args(&["--{", "--", "--{x", "--}x", "not--option", "file"]);
        assert!(args_valid(&argv)); // argv[0] is ignored, even if it resembles a bracket.
        assert_eq!(
            parse_load_args(&argv),
            Ok(vec![
                load("not--option", &["", "{x", "}x"]),
                load("file", &["", "{x", "}x"]),
            ])
        );
        assert_eq!(parse_load_args(&args(&["program"])), Ok(vec![]));
    }
}
