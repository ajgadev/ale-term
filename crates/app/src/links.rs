//! Finding things to open in terminal text: web addresses and file paths.
//!
//! Pure text logic: given a line of characters and a position in it, work
//! out whether that position is inside a link and where the link starts and
//! ends. The app handles hovering, underlining and opening.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Something to hand to the system as-is (`https://...`, `mailto:...`).
    Url(String),
    /// An existing file or folder.
    File(PathBuf),
}

/// A link found in a line: the character range it covers and what it opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub start: usize,
    /// Exclusive.
    pub end: usize,
    pub target: Target,
}

/// URL schemes we are willing to open. Deliberately short: opening arbitrary
/// schemes from terminal output could launch arbitrary apps.
const SCHEMES: [&str; 4] = ["https://", "http://", "file://", "mailto:"];

fn is_url_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '\\' | '^')
}

/// Drops punctuation that usually belongs to the surrounding sentence, and
/// closing brackets that have no opening partner inside the link.
fn trim_end(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start {
        let last = chars[end - 1];
        let unbalanced = |open: char, close: char| {
            let text = &chars[start..end];
            text.iter().filter(|c| **c == close).count() > text.iter().filter(|c| **c == open).count()
        };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' => true,
            ')' => unbalanced('(', ')'),
            ']' => unbalanced('[', ']'),
            _ => false,
        };
        if !drop {
            break;
        }
        end -= 1;
    }
    end
}

/// Finds a URL covering position `at`.
pub fn url_at(chars: &[char], at: usize) -> Option<Link> {
    let text: String = chars.iter().collect();
    // Map byte offsets back to character positions as we go.
    let mut search_from = 0;
    while search_from < text.len() {
        let (byte_start, scheme) = SCHEMES
            .iter()
            .filter_map(|scheme| text[search_from..].find(scheme).map(|i| (i + search_from, scheme)))
            .min_by_key(|(i, _)| *i)?;
        let start = text[..byte_start].chars().count();
        let mut end = start;
        while end < chars.len() && is_url_char(chars[end]) {
            end += 1;
        }
        let end = trim_end(chars, start, end);
        if start > at {
            return None;
        }
        if at < end && end - start > scheme.chars().count() {
            let url: String = chars[start..end].iter().collect();
            return Some(Link { start, end, target: Target::Url(url) });
        }
        search_from = byte_start + scheme.len();
    }
    None
}

fn is_path_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !matches!(c, '"' | '\'' | '`' | '<' | '>' | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '=' | ',' | ';')
}

/// Finds a path to an existing file or folder covering position `at`.
///
/// Relative paths are resolved against `cwd`. A trailing `:line` or
/// `:line:column` (as compilers print) is accepted and ignored. `exists`
/// is passed in so tests do not touch the disk.
pub fn path_at(chars: &[char], at: usize, cwd: Option<&Path>, exists: impl Fn(&Path) -> bool) -> Option<Link> {
    if at >= chars.len() || !is_path_char(chars[at]) {
        return None;
    }
    let mut start = at;
    while start > 0 && is_path_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < chars.len() && is_path_char(chars[end]) {
        end += 1;
    }
    let end = trim_end(chars, start, end);
    if at >= end {
        return None;
    }

    let token: String = chars[start..end].iter().collect();
    // Something that is only a word is too likely to be a coincidence.
    if !token.contains('/') && !token.contains('.') {
        return None;
    }
    // Try the whole token, then without `:line:col` suffixes.
    let mut candidate = token.as_str();
    loop {
        let expanded = match candidate.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest)),
            None if Path::new(candidate).is_absolute() => Some(PathBuf::from(candidate)),
            None => cwd.map(|cwd| cwd.join(candidate)),
        };
        if let Some(path) = expanded.filter(|path| exists(path)) {
            let length = candidate.chars().count();
            if at < start + length {
                return Some(Link { start, end: start + length, target: Target::File(path) });
            }
            return None;
        }
        match candidate.rsplit_once(':') {
            Some((rest, suffix)) if !rest.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) => {
                candidate = rest;
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    fn url(text: &str, at: usize) -> Option<String> {
        url_at(&chars(text), at).map(|link| match link.target {
            Target::Url(url) => url,
            Target::File(_) => unreachable!(),
        })
    }

    #[test]
    fn finds_urls_under_the_position() {
        let text = "see https://example.com/a?b=1 and http://x.org.";
        assert_eq!(url(text, 4).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url(text, 28).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url(text, 29), None);
        assert_eq!(url(text, 0), None);
        // Trailing sentence punctuation is not part of the link.
        assert_eq!(url(text, 40).as_deref(), Some("http://x.org"));
    }

    #[test]
    fn brackets_are_kept_only_when_balanced() {
        assert_eq!(url("(https://a.com/x)", 5).as_deref(), Some("https://a.com/x"));
        assert_eq!(url("https://w.org/Rust_(lang)", 5).as_deref(), Some("https://w.org/Rust_(lang)"));
        assert_eq!(url("[docs](https://a.com/b).", 10).as_deref(), Some("https://a.com/b"));
    }

    #[test]
    fn handles_non_ascii_and_bare_schemes() {
        assert_eq!(url("ñandú → https://a.com/ñ ok", 10).as_deref(), Some("https://a.com/ñ"));
        assert_eq!(url("https:// nothing", 2), None);
        assert_eq!(url("ssh://host", 2), None);
    }

    #[test]
    fn finds_existing_paths() {
        let exists = |path: &Path| path == Path::new("/repo/src/main.rs") || path == Path::new("/etc/hosts");
        let cwd = Some(Path::new("/repo"));
        let find = |text: &str, at| path_at(&chars(text), at, cwd, exists);

        let link = find("error at src/main.rs:12:5: oops", 12).unwrap();
        assert_eq!(link.target, Target::File("/repo/src/main.rs".into()));
        assert_eq!((link.start, link.end), (9, 20));
        // On the line number part: not a link.
        assert_eq!(find("error at src/main.rs:12:5: oops", 21), None);

        assert_eq!(find("cat /etc/hosts.", 6).unwrap().target, Target::File("/etc/hosts".into()));
        assert_eq!(find("no such/file.txt here", 4), None);
        assert_eq!(find("plainword", 3), None);
        assert_eq!(find("a  b", 1), None);
    }
}
