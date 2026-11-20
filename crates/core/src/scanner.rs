//! Picks out the escape sequences we handle ourselves from the shell's
//! output, before the terminal emulator sees it.
//!
//! The emulator library understands the standard sequences but not shell
//! integration (OSC 7, OSC 133) or inline images (OSC 1337, kitty's APC
//! protocol). The scanner splits the byte stream into ordinary output, which
//! is passed on untouched, and those sequences, which are handed over whole.
//! Order is preserved, so an image is handled exactly where it appeared.
//!
//! Sequences can be split across reads; the scanner keeps the partial
//! sequence between calls.

/// A piece of the output stream.
#[derive(Debug, PartialEq, Eq)]
pub enum Chunk<'a> {
    /// Ordinary output for the terminal emulator.
    Output(&'a [u8]),
    /// The payload of an OSC sequence we handle (without `ESC ]` and the
    /// terminator), e.g. `7;file://host/path`.
    Osc(&'a [u8]),
    /// The payload of an APC sequence (without `ESC _` and `ESC \`).
    Apc(&'a [u8]),
}

/// OSC numbers handled by us instead of the emulator.
const OSC_HANDLED: [&[u8]; 3] = [b"7", b"133", b"1337"];

/// Sequences longer than this are dropped rather than buffered without end.
const MAX_SEQUENCE: usize = 48 * 1024 * 1024;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Ordinary output.
    Ground,
    /// Saw `ESC`; the next byte decides what it starts.
    Escape,
    /// Inside `ESC ]`, reading the number to decide whether it is ours.
    OscNumber,
    /// Inside an OSC we handle, collecting its payload.
    Osc,
    /// Inside an OSC we handle, just saw `ESC` (expecting `\`).
    OscEscape,
    /// Inside `ESC _`, collecting the payload.
    Apc,
    ApcEscape,
}

pub struct Scanner {
    state: State,
    /// Bytes of a sequence in progress. While undecided (`Escape`,
    /// `OscNumber`) these are the raw bytes, to be replayed as output if the
    /// sequence turns out not to be ours; afterwards, the payload.
    pending: Vec<u8>,
    /// Set when a sequence outgrew `MAX_SEQUENCE`; the rest is discarded.
    overflow: bool,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Self {
        Self { state: State::Ground, pending: Vec::new(), overflow: false }
    }

    /// Splits `bytes` into chunks, calling `sink` for each in order.
    pub fn feed<'a>(&'a mut self, bytes: &'a [u8], mut sink: impl FnMut(Chunk<'_>)) {
        // Start of the run of ordinary output not yet handed to the sink.
        let mut output_start = 0;
        let mut i = 0;
        while i < bytes.len() {
            let byte = bytes[i];
            match self.state {
                State::Ground => {
                    // Skip ahead to the next ESC in one go.
                    match bytes[i..].iter().position(|b| *b == ESC) {
                        Some(offset) => {
                            i += offset;
                            if i > output_start {
                                sink(Chunk::Output(&bytes[output_start..i]));
                            }
                            self.pending.clear();
                            self.pending.push(ESC);
                            self.state = State::Escape;
                        }
                        None => i = bytes.len() - 1,
                    }
                }
                State::Escape => match byte {
                    b']' => {
                        self.pending.push(byte);
                        self.state = State::OscNumber;
                    }
                    b'_' => {
                        self.pending.clear();
                        self.overflow = false;
                        self.state = State::Apc;
                    }
                    _ => {
                        // Some other escape sequence: give the ESC back.
                        sink(Chunk::Output(&[ESC]));
                        self.state = State::Ground;
                        output_start = i;
                        continue;
                    }
                },
                State::OscNumber => match byte {
                    b'0'..=b'9' if self.pending.len() < 8 => self.pending.push(byte),
                    b';' | BEL | ESC if OSC_HANDLED.contains(&&self.pending[2..]) => {
                        // Ours: keep the number as the start of the payload.
                        self.pending.drain(..2);
                        self.overflow = false;
                        self.state = State::Osc;
                        continue;
                    }
                    _ => {
                        // Not ours: replay what we held back.
                        sink(Chunk::Output(&self.pending));
                        self.state = State::Ground;
                        output_start = i;
                        continue;
                    }
                },
                State::Osc => match byte {
                    BEL => {
                        self.finish(&mut sink, false);
                        output_start = i + 1;
                    }
                    ESC => self.state = State::OscEscape,
                    _ => self.push(byte),
                },
                State::Apc => match byte {
                    ESC => self.state = State::ApcEscape,
                    _ => self.push(byte),
                },
                State::OscEscape | State::ApcEscape => {
                    let apc = self.state == State::ApcEscape;
                    self.finish(&mut sink, apc);
                    if byte == b'\\' {
                        output_start = i + 1;
                    } else {
                        // The ESC started a new sequence instead.
                        self.pending.clear();
                        self.pending.push(ESC);
                        self.state = State::Escape;
                        continue;
                    }
                }
            }
            i += 1;
        }
        if self.state == State::Ground && output_start < bytes.len() {
            sink(Chunk::Output(&bytes[output_start..]));
        }
    }

    fn push(&mut self, byte: u8) {
        if self.pending.len() < MAX_SEQUENCE {
            self.pending.push(byte);
        } else {
            self.overflow = true;
        }
    }

    /// Hands a completed sequence to the sink and returns to ordinary output.
    fn finish(&mut self, sink: &mut impl FnMut(Chunk<'_>), apc: bool) {
        if !self.overflow {
            sink(if apc { Chunk::Apc(&self.pending) } else { Chunk::Osc(&self.pending) });
        }
        self.pending.clear();
        self.overflow = false;
        self.state = State::Ground;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `parts` one after another and renders the chunks as text:
    /// output as-is, sequences as `<osc:..>` / `<apc:..>`.
    fn scan(parts: &[&[u8]]) -> String {
        let mut scanner = Scanner::new();
        let mut out = String::new();
        for part in parts {
            scanner.feed(part, |chunk| match chunk {
                Chunk::Output(b) => out.push_str(&String::from_utf8_lossy(b)),
                Chunk::Osc(b) => out.push_str(&format!("<osc:{}>", String::from_utf8_lossy(b))),
                Chunk::Apc(b) => out.push_str(&format!("<apc:{}>", String::from_utf8_lossy(b))),
            });
        }
        out
    }

    #[test]
    fn plain_output_passes_through() {
        assert_eq!(scan(&[b"hello ", b"world"]), "hello world");
    }

    #[test]
    fn other_escape_sequences_are_untouched() {
        assert_eq!(scan(&[b"a\x1b[31mb\x1b]0;title\x07c\x1b]8;;x\x1b\\d"]), "a\x1b[31mb\x1b]0;title\x07c\x1b]8;;x\x1b\\d");
        // OSC 70 and 13 must not be mistaken for 7 and 133.
        assert_eq!(scan(&[b"\x1b]70;x\x07\x1b]13;y\x07"]), "\x1b]70;x\x07\x1b]13;y\x07");
    }

    #[test]
    fn handled_osc_is_extracted_with_either_terminator() {
        assert_eq!(scan(&[b"a\x1b]7;file://h/p\x07b"]), "a<osc:7;file://h/p>b");
        assert_eq!(scan(&[b"a\x1b]133;A\x1b\\b"]), "a<osc:133;A>b");
        assert_eq!(scan(&[b"\x1b]1337;File=:AAAA\x07"]), "<osc:1337;File=:AAAA>");
    }

    #[test]
    fn apc_is_extracted() {
        assert_eq!(scan(&[b"x\x1b_Gf=100;AAAA\x1b\\y"]), "x<apc:Gf=100;AAAA>y");
    }

    #[test]
    fn sequences_split_at_every_position_give_the_same_result() {
        let input: &[u8] = b"ab\x1b]133;C;cmdline_url=ls\x07cd\x1b[1mef\x1b_Ga=q\x1b\\gh\x1b]2;t\x07\x1b";
        let whole = scan(&[input]);
        assert_eq!(whole, "ab<osc:133;C;cmdline_url=ls>cd\x1b[1mef<apc:Ga=q>gh\x1b]2;t\x07");
        for split in 1..input.len() {
            assert_eq!(scan(&[&input[..split], &input[split..]]), whole, "split at {split}");
        }
        // One byte at a time.
        let bytes: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(scan(&bytes), whole);
    }

    #[test]
    fn escape_inside_a_sequence_that_is_not_a_terminator_starts_over() {
        assert_eq!(scan(&[b"\x1b]133;A\x1b[1mx"]), "<osc:133;A>\x1b[1mx");
    }
}
