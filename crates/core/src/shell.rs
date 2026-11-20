//! Shell integration: what the shell tells us about itself.
//!
//! With the integration script loaded (see `shell_integration`), the shell
//! reports its working directory (OSC 7) and marks where prompts and
//! commands begin and end (OSC 133). That lets the app know, without
//! guessing, which folder a pane is in and what command it is running.

use std::path::PathBuf;
use std::time::Instant;

/// What we currently know about the shell in a session.
#[derive(Clone, Debug, Default)]
pub struct ShellState {
    /// True once the shell has sent any integration sequence.
    pub integrated: bool,
    /// Working directory as reported by the shell.
    pub cwd: Option<PathBuf>,
    /// The command line being run, while one is running.
    pub command: Option<String>,
    /// When the running command started.
    pub command_started: Option<Instant>,
    /// Exit status of the last finished command.
    pub last_exit: Option<i32>,
}

/// An OSC 133 mark.
#[derive(Debug, PartialEq, Eq)]
pub enum Mark {
    /// `A`: a prompt is about to be drawn.
    PromptStart,
    /// `B`: the prompt is done; what follows is typed by the user.
    PromptEnd,
    /// `C`: a command started running. Carries the command line if sent.
    CommandStart(Option<String>),
    /// `D`: the command finished, with its exit status if sent.
    CommandEnd(Option<i32>),
}

/// Parses an OSC 133 payload such as `133;C;cmdline_url=ls%20-la`.
pub fn parse_mark(payload: &[u8]) -> Option<Mark> {
    let mut fields = payload.split(|b| *b == b';');
    if fields.next()? != b"133" {
        return None;
    }
    match fields.next()? {
        b"A" => Some(Mark::PromptStart),
        b"B" => Some(Mark::PromptEnd),
        b"C" => {
            let command = fields
                .find_map(|field| field.strip_prefix(b"cmdline_url="))
                .map(|encoded| String::from_utf8_lossy(&percent_decode(encoded)).into_owned())
                .filter(|command| !command.trim().is_empty());
            Some(Mark::CommandStart(command))
        }
        b"D" => {
            let status = fields.next().and_then(|f| std::str::from_utf8(f).ok()?.parse().ok());
            Some(Mark::CommandEnd(status))
        }
        _ => None,
    }
}

/// Parses an OSC 7 payload such as `7;file://hostname/Users/me/my%20dir`.
pub fn parse_cwd(payload: &[u8]) -> Option<PathBuf> {
    let url = payload.strip_prefix(b"7;")?;
    let path = match url.strip_prefix(b"file://") {
        // Skip the hostname: the path starts at the next slash.
        Some(rest) => &rest[rest.iter().position(|b| *b == b'/')?..],
        None => url,
    };
    if path.first() != Some(&b'/') {
        return None;
    }
    String::from_utf8(percent_decode(path)).ok().map(PathBuf::from)
}

/// Decodes `%XX` escapes; anything malformed is kept as-is.
pub fn percent_decode(input: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%'
            && i + 2 < input.len()
            && let (Some(high), Some(low)) = (hex(input[i + 1]), hex(input[i + 2]))
        {
            out.push(high * 16 + low);
            i += 3;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_working_directory() {
        assert_eq!(parse_cwd(b"7;file://mac.local/Users/me/my%20dir"), Some("/Users/me/my dir".into()));
        assert_eq!(parse_cwd(b"7;file:///tmp"), Some("/tmp".into()));
        assert_eq!(parse_cwd(b"7;/tmp/x"), Some("/tmp/x".into()));
        assert_eq!(parse_cwd(b"7;file://host"), None);
        assert_eq!(parse_cwd(b"7;relative"), None);
    }

    #[test]
    fn parses_marks() {
        assert_eq!(parse_mark(b"133;A"), Some(Mark::PromptStart));
        assert_eq!(parse_mark(b"133;B"), Some(Mark::PromptEnd));
        assert_eq!(parse_mark(b"133;C"), Some(Mark::CommandStart(None)));
        assert_eq!(
            parse_mark(b"133;C;cmdline_url=git%20commit%20-m%20%22a%3Bb%22"),
            Some(Mark::CommandStart(Some("git commit -m \"a;b\"".into())))
        );
        assert_eq!(parse_mark(b"133;D;130"), Some(Mark::CommandEnd(Some(130))));
        assert_eq!(parse_mark(b"133;D"), Some(Mark::CommandEnd(None)));
        assert_eq!(parse_mark(b"133;Z"), None);
    }

    #[test]
    fn percent_decoding_tolerates_bad_input() {
        assert_eq!(percent_decode(b"a%2Fb"), b"a/b");
        assert_eq!(percent_decode(b"100%"), b"100%");
        assert_eq!(percent_decode(b"%zz%4"), b"%zz%4");
        assert_eq!(percent_decode(b"%C3%B1"), "ñ".as_bytes());
    }
}
