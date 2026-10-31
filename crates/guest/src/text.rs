//! Small, explicit parsers for terminal output and version strings.

use serde_json::Value;

/// Removes CSI escape sequences (`ESC [ params intermediates final`).
pub fn strip_csi(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\x1b'
            && chars.get(i + 1) == Some(&'[')
            && let Some(end) = csi_end(&chars, i + 2)
        {
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn csi_end(chars: &[char], mut i: usize) -> Option<usize> {
    while chars.get(i).is_some_and(|c| ('0'..='?').contains(c)) {
        i += 1;
    }
    while chars.get(i).is_some_and(|c| (' '..='/').contains(c)) {
        i += 1;
    }
    chars
        .get(i)
        .filter(|c| ('@'..='~').contains(*c))
        .map(|_| i + 1)
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The first `major.minor.patch` not preceded by a digit. With `suffix`, a
/// pre-release or build suffix (`-beta.1`, `+build`) is kept, and the match
/// must end on a word boundary.
pub fn find_version(text: &str, suffix: bool) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    (0..chars.len()).find_map(|start| {
        if start > 0 && chars[start - 1].is_ascii_digit() {
            return None;
        }
        let mut end = start;
        for part in 0..3 {
            if part > 0 {
                if chars.get(end) != Some(&'.') {
                    return None;
                }
                end += 1;
            }
            let digits = chars[end..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            if digits == 0 {
                return None;
            }
            end += digits;
        }
        if suffix {
            if matches!(chars.get(end), Some('-' | '+')) {
                let run = chars[end + 1..]
                    .iter()
                    .take_while(|c| word(**c) || **c == '.' || **c == '-')
                    .count();
                // Backtrack to a word boundary, as the regex would.
                let kept = (0..=run)
                    .rev()
                    .find(|n| *n > 0 && word(chars[end + n]))
                    .unwrap_or(0);
                if kept > 0 {
                    end += 1 + kept;
                }
            }
            if chars.get(end).is_some_and(|c| word(*c)) {
                return None;
            }
        }
        Some(chars[start..end].iter().collect())
    })
}

pub fn parse_version(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.split('.').map(|part| part.parse::<u64>().ok());
    let version = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(version)
}

/// Unicode "Other" categories: controls, formatting (including bidirectional
/// overrides and zero-width characters), private use and noncharacters.
/// Unassigned code points are not tracked without the Unicode tables.
fn hidden(c: char) -> bool {
    let u = c as u32;
    c.is_control()
        || matches!(
            u,
            0xAD | 0x600..=0x605
                | 0x61C
                | 0x6DD
                | 0x70F
                | 0x890..=0x891
                | 0x8E2
                | 0x180E
                | 0x200B..=0x200F
                | 0x202A..=0x202E
                | 0x2060..=0x206F
                | 0xFEFF
                | 0xFFF9..=0xFFFB
                | 0x110BD
                | 0x110CD
                | 0x13430..=0x1343F
                | 0x1BCA0..=0x1BCA3
                | 0x1D173..=0x1D17A
                | 0xE0001
                | 0xE0020..=0xE007F
                | 0xE000..=0xF8FF
                | 0xF0000..=0x10FFFF
                | 0xFDD0..=0xFDEF
        )
        || (u & 0xFFFE) == 0xFFFE
}

/// Account text that is safe to display: a non-blank string of at most
/// `maximum` characters with no hidden or control characters.
pub fn display_text(value: Option<&Value>, maximum: usize) -> Option<String> {
    let text = value?.as_str()?;
    if text.trim().is_empty() || text.chars().count() > maximum || text.chars().any(hidden) {
        return None;
    }
    Some(text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn csi_sequences_are_removed_and_other_escapes_kept() {
        assert_eq!(strip_csi("\x1b[32mgreen\x1b[0m done"), "green done");
        assert_eq!(strip_csi("\x1b[?25lcursor"), "cursor");
        assert_eq!(strip_csi("keep \x1b]8;;x\x07"), "keep \x1b]8;;x\x07");
        assert_eq!(strip_csi("trailing \x1b["), "trailing \x1b[");
    }

    #[test]
    fn versions_match_the_first_release_triplet() {
        assert_eq!(
            find_version("codex-cli 0.165.0", false).as_deref(),
            Some("0.165.0")
        );
        assert_eq!(
            find_version("2.1.308 (Claude Code)", false).as_deref(),
            Some("2.1.308")
        );
        assert_eq!(
            find_version("gh version 2.103.3 (2026-01-01)", false).as_deref(),
            Some("2.103.3")
        );
        assert_eq!(find_version("v10.2.1.4", false).as_deref(), Some("10.2.1"));
        assert_eq!(find_version("no version", false), None);
        assert_eq!(find_version("t3 v0.1.2\n", true).as_deref(), Some("0.1.2"));
        assert_eq!(
            find_version("t3 v0.1.2-beta.1", true).as_deref(),
            Some("0.1.2-beta.1")
        );
        assert_eq!(
            find_version("t3 v0.1.2-beta.", true).as_deref(),
            Some("0.1.2-beta")
        );
        assert_eq!(find_version("t3 v0.1.2+", true).as_deref(), Some("0.1.2"));
        assert_eq!(find_version("0.1.2abc", true), None);
        assert_eq!(parse_version("2.40.0"), Some([2, 40, 0]));
        assert_eq!(parse_version("2.40"), None);
        assert!(parse_version("0.165.0") >= Some([0, 159, 0]));
    }

    #[test]
    fn control_sequences_and_oversized_identities_are_not_displayed() {
        for value in [
            json!("bad\nemail"),
            json!("\x1b[31mred"),
            json!("wrong\u{202e}example.com"),
            json!("zero\u{200b}width"),
            json!("x".repeat(321)),
            json!("   "),
            json!(null),
            json!(42),
        ] {
            assert_eq!(display_text(Some(&value), 320), None, "{value}");
        }
        assert_eq!(display_text(None, 320), None);
        assert_eq!(
            display_text(Some(&json!(" dev@example.com ")), 320).as_deref(),
            Some("dev@example.com")
        );
        assert_eq!(
            display_text(Some(&json!("é".repeat(320))), 320).map(|t| t.chars().count()),
            Some(320)
        );
    }
}
