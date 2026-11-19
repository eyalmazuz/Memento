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

//! Platform MeCab tagger option-byte formatting helper.

/// Format the MeCab option bytes without interpreting Windows' 8.3 paths as
/// UTF-8. `GetShortPathNameA` returns raw bytes, which must be passed through
/// unchanged even when the local ANSI code page is not UTF-8.
pub fn format_mecab_tagger_arg(
    is_windows: bool,
    is_bundle: bool,
    path_a: &[u8],
    path_b: &[u8],
) -> Vec<u8> {
    if is_windows {
        let mut arg = Vec::with_capacity(8 + path_a.len() + path_b.len());
        arg.extend_from_slice(b" -d ");
        arg.extend_from_slice(path_a);
        arg.extend_from_slice(b" -r ");
        arg.extend_from_slice(path_b);
        arg
    } else if is_bundle {
        let mut arg = Vec::with_capacity(27 + path_a.len() * 2);
        arg.extend_from_slice(b"-r ");
        arg.extend_from_slice(path_a);
        arg.extend_from_slice(b"ipadic/dicrc -d ");
        arg.extend_from_slice(path_a);
        arg.extend_from_slice(b"ipadic");
        arg
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_mecab_tagger_arg_cases() {
        // Windows
        assert_eq!(
            format_mecab_tagger_arg(true, false, b"C:/PROGRA~1/ipadic", b"C:/PROGRA~1/ipadic/dicrc"),
            b" -d C:/PROGRA~1/ipadic -r C:/PROGRA~1/ipadic/dicrc"
        );
        // Windows takes precedence if both flags are true
        assert_eq!(
            format_mecab_tagger_arg(true, true, b"C:/ipadic", b"C:/ipadic/dicrc"),
            b" -d C:/ipadic -r C:/ipadic/dicrc"
        );
        // Bundle
        assert_eq!(
            format_mecab_tagger_arg(false, true, b"/app/share/mecab/", b""),
            b"-r /app/share/mecab/ipadic/dicrc -d /app/share/mecab/ipadic"
        );
        assert_eq!(
            format_mecab_tagger_arg(false, true, b"", b""),
            b"-r ipadic/dicrc -d ipadic"
        );
        // Standard Linux
        assert_eq!(
            format_mecab_tagger_arg(false, false, b"/usr/share/mecab", b"/etc/dicrc"),
            b""
        );
        assert_eq!(
            format_mecab_tagger_arg(false, false, b"", b""),
            b""
        );

        // Windows 8.3 paths from GetShortPathNameA may be in an ANSI code
        // page. The bytes must reach MeCab unchanged.
        assert_eq!(
            format_mecab_tagger_arg(true, false, b"C:/\xff/dic", b"C:/\xfe/dicrc"),
            b" -d C:/\xff/dic -r C:/\xfe/dicrc"
        );
    }
}
