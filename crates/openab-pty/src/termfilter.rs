//! Terminal-capability response filtering at the PTY boundary
//! (client → PTY direction only).
//!
//! Adopted from OpenDray's `terminal_capabilities.go` (ADR §6). Browser
//! emulators answer capability queries on their own: xterm.js replies to Device
//! Attributes, Cursor Position Report and Device Status Report requests without
//! the user typing anything. Those replies arrive on the same channel as
//! keystrokes, and an Ink-based CLI reading them during startup mis-parses them
//! as input — the known Ink-CLI startup breakage. Filtering at this one
//! chokepoint protects every client emulator instead of asking each of them to
//! behave.
//!
//! Only the responses are stripped. Everything a human or a mouse can produce
//! passes through byte-for-byte, which is why the finals we strip are limited to
//! `c` (DA), `n` (DSR) and `R` (CPR): no key, mouse, focus, bracketed-paste or
//! Kitty-keyboard encoding a terminal sends to an application uses them.
//!
//! **Chunk-scoped by design.** The filter holds no cross-call state: an escape
//! sequence split across two WebSocket frames passes through unfiltered. The
//! alternative — carrying a partial sequence to the next chunk — would delay a
//! bare <kbd>Esc</kbd> keypress until the *next* keystroke, breaking vi-style
//! editors, and no timeout is available at this layer. Emulators write each
//! capability reply with a single send, so the split case is rare, and the cost
//! of missing it is the status quo rather than a new failure.

use std::borrow::Cow;

/// Upper bound on how far we scan for a sequence terminator. A longer run is
/// treated as not-a-sequence and passed through, so a malformed or hostile
/// stream cannot make the scanner walk an unbounded distance per byte.
const MAX_SEQUENCE_LEN: usize = 256;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Stateless, chunk-scoped capability-response filter with strip counters for
/// the abuse/observability metrics.
#[derive(Debug, Default)]
pub struct TermFilter {
    stripped_sequences: u64,
    stripped_bytes: u64,
}

impl TermFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of capability responses stripped so far.
    pub fn stripped_sequences(&self) -> u64 {
        self.stripped_sequences
    }

    /// Number of bytes removed so far.
    pub fn stripped_bytes(&self) -> u64 {
        self.stripped_bytes
    }

    /// Filter one client→PTY chunk. Returns the input untouched (borrowed, no
    /// allocation) when there is nothing to strip, which is the common case for
    /// ordinary typing.
    pub fn filter<'a>(&mut self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        if !chunk.contains(&ESC) {
            return Cow::Borrowed(chunk);
        }

        let mut out: Option<Vec<u8>> = None;
        let mut i = 0;
        let mut copied_upto = 0;
        while i < chunk.len() {
            if chunk[i] != ESC {
                i += 1;
                continue;
            }
            match classify(&chunk[i..]) {
                Some(len) => {
                    let out = out.get_or_insert_with(|| Vec::with_capacity(chunk.len()));
                    out.extend_from_slice(&chunk[copied_upto..i]);
                    self.stripped_sequences += 1;
                    self.stripped_bytes += len as u64;
                    i += len;
                    copied_upto = i;
                }
                // Not a capability response: skip the ESC and keep scanning from
                // the next byte, so an SS3 function key (`ESC O P`) or a bare
                // ESC keypress is untouched.
                None => i += 1,
            }
        }

        match out {
            Some(mut out) => {
                out.extend_from_slice(&chunk[copied_upto..]);
                Cow::Owned(out)
            }
            None => Cow::Borrowed(chunk),
        }
    }
}

/// If `s` starts with a terminal-capability *response*, return its length.
fn classify(s: &[u8]) -> Option<usize> {
    debug_assert_eq!(s.first(), Some(&ESC));
    match s.get(1)? {
        b'[' => classify_csi(s),
        b']' => classify_osc(s),
        b'P' => classify_dcs(s),
        _ => None,
    }
}

/// `CSI ... c` (Device Attributes), `CSI ... n` (Device Status Report) and
/// `CSI ... R` (Cursor Position Report) are responses. Every other final byte
/// belongs to input a terminal legitimately sends: `A`–`D` arrows, `~` tilde
/// keys and bracketed paste, `M`/`m` mouse, `I`/`O` focus, `u` Kitty keyboard.
fn classify_csi(s: &[u8]) -> Option<usize> {
    let mut i = 2;
    while i < s.len() && i <= MAX_SEQUENCE_LEN {
        let b = s[i];
        match b {
            // Parameter bytes (including the `?`, `>`, `<`, `=` private
            // markers) and intermediates.
            0x20..=0x3f => i += 1,
            // Final byte.
            0x40..=0x7e => {
                return if matches!(b, b'c' | b'n' | b'R') {
                    Some(i + 1)
                } else {
                    None
                };
            }
            // Anything else (control byte, 8-bit) means this is not a
            // well-formed CSI sequence; leave it alone.
            _ => return None,
        }
    }
    None
}

/// OSC color reports: `OSC Ps ; rgb:<...> ST|BEL`, e.g. the `OSC 10`/`OSC 11`
/// foreground/background answers and `OSC 4 ; n ; rgb:` palette answers.
/// Deliberately narrow: only payloads carrying an `rgb:`/`rgba:` reply are
/// stripped, so any other OSC use passes untouched.
fn classify_osc(s: &[u8]) -> Option<usize> {
    let (payload, len) = string_sequence(s, 2)?;
    let mut j = 0;
    // Ps ;  (one or more numeric groups)
    while j < payload.len() && payload[j].is_ascii_digit() {
        j += 1;
    }
    if j == 0 || payload.get(j) != Some(&b';') {
        return None;
    }
    if payload[j..].starts_with(b";rgb:") || payload[j..].starts_with(b";rgba:") {
        return Some(len);
    }
    // `OSC 4 ; <index> ; rgb:` — one extra numeric group.
    let mut k = j + 1;
    while k < payload.len() && payload[k].is_ascii_digit() {
        k += 1;
    }
    if k > j + 1 && (payload[k..].starts_with(b";rgb:") || payload[k..].starts_with(b";rgba:")) {
        return Some(len);
    }
    None
}

/// DCS report shapes: tertiary DA (`DCS ! | ...`), XTVERSION (`DCS > | ...`),
/// DECRQSS answers (`DCS 0$r` / `DCS 1$r`) and XTGETTCAP answers
/// (`DCS 0+r` / `DCS 1+r`). Other DCS payloads are passed through rather than
/// assumed to be replies.
fn classify_dcs(s: &[u8]) -> Option<usize> {
    let (payload, len) = string_sequence(s, 2)?;
    let mut j = 0;
    while j < payload.len() && payload[j].is_ascii_digit() {
        j += 1;
    }
    let rest = &payload[j..];
    if rest.starts_with(b"!|")
        || rest.starts_with(b">|")
        || rest.starts_with(b"$r")
        || rest.starts_with(b"+r")
        || rest.starts_with(b"+R")
    {
        Some(len)
    } else {
        None
    }
}

/// Scan a string-terminated sequence (OSC/DCS) starting at `body` within `s`.
/// Returns the payload and the total sequence length including the terminator.
fn string_sequence(s: &[u8], body: usize) -> Option<(&[u8], usize)> {
    let mut i = body;
    let limit = s.len().min(MAX_SEQUENCE_LEN);
    while i < limit {
        if s[i] == BEL {
            return Some((&s[body..i], i + 1));
        }
        if s[i] == ESC && s.get(i + 1) == Some(&b'\\') {
            return Some((&s[body..i], i + 2));
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(input: &[u8]) -> Vec<u8> {
        TermFilter::new().filter(input).to_vec()
    }

    #[test]
    fn strips_primary_and_secondary_device_attributes() {
        assert!(filtered(b"\x1b[?62;22c").is_empty());
        assert!(filtered(b"\x1b[>0;276;0c").is_empty());
        assert!(filtered(b"\x1b[?1;2c").is_empty());
    }

    #[test]
    fn strips_cursor_position_report() {
        assert!(filtered(b"\x1b[24;80R").is_empty());
        assert!(filtered(b"\x1b[?24;80;1R").is_empty(), "DECXCPR");
    }

    #[test]
    fn strips_device_status_report() {
        assert!(filtered(b"\x1b[0n").is_empty());
        assert!(filtered(b"\x1b[?83n").is_empty());
    }

    #[test]
    fn strips_tertiary_da_and_xtversion_dcs() {
        assert!(filtered(b"\x1bP!|7E565445\x1b\\").is_empty());
        assert!(filtered(b"\x1bP>|xterm.js(5.3.0)\x1b\\").is_empty());
        assert!(filtered(b"\x1bP1$r0;1m\x1b\\").is_empty(), "DECRQSS answer");
        assert!(
            filtered(b"\x1bP1+r544e=787465726d\x07").is_empty(),
            "XTGETTCAP"
        );
    }

    #[test]
    fn strips_osc_color_reports() {
        assert!(filtered(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07").is_empty());
        assert!(filtered(b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\").is_empty());
        assert!(filtered(b"\x1b]4;1;rgb:cd00/0000/0000\x07").is_empty());
    }

    #[test]
    fn strips_response_embedded_in_typing() {
        // The realistic case: the emulator's auto-answer lands in the middle of
        // the user's keystrokes.
        assert_eq!(filtered(b"ls\x1b[?62;22c -la\r"), b"ls -la\r");
    }

    #[test]
    fn strips_multiple_responses_in_one_chunk() {
        assert_eq!(filtered(b"a\x1b[0nb\x1b[24;80Rc"), b"abc");
    }

    #[test]
    fn passes_plain_text_untouched_without_allocating() {
        let mut f = TermFilter::new();
        assert!(matches!(f.filter(b"echo hello\r"), Cow::Borrowed(_)));
        assert_eq!(f.stripped_sequences(), 0);
    }

    #[test]
    fn passes_keyboard_input_unchanged() {
        for input in [
            &b"\x1b[A"[..],              // up arrow
            b"\x1b[B",                   // down
            b"\x1b[1;5C",                // ctrl+right
            b"\x1b[3~",                  // delete
            b"\x1bOP",                   // F1 via SS3
            b"\x1b",                     // bare Esc keypress
            b"\x1b\x1b",                 // Esc Esc (vi users do this)
            b"\x03",                     // Ctrl-C
            b"\x1b[97;5u",               // Kitty keyboard protocol
            b"\x1b[200~pasted\x1b[201~", // bracketed paste
        ] {
            assert_eq!(filtered(input), input, "must pass through: {input:?}");
        }
    }

    #[test]
    fn passes_mouse_and_focus_reports_unchanged() {
        for input in [
            &b"\x1b[<0;10;5M"[..], // SGR mouse press
            b"\x1b[<0;10;5m",      // SGR mouse release (lowercase m, not n)
            b"\x1b[I",             // focus in
            b"\x1b[O",             // focus out
        ] {
            assert_eq!(filtered(input), input, "must pass through: {input:?}");
        }
    }

    #[test]
    fn passes_literal_letters_that_match_response_finals() {
        // `c`, `n` and `R` only matter after a CSI introducer.
        assert_eq!(filtered(b"cat n R\r"), b"cat n R\r");
    }

    #[test]
    fn passes_unrecognised_osc_and_dcs_untouched() {
        let osc_title = b"\x1b]0;my title\x07";
        assert_eq!(filtered(osc_title), osc_title);
        let dcs_other = b"\x1bPtmux;something\x1b\\";
        assert_eq!(filtered(dcs_other), dcs_other);
    }

    #[test]
    fn passes_unterminated_sequences_untouched() {
        // Split across chunks: documented pass-through rather than holding the
        // bytes back (which would delay a bare Esc).
        assert_eq!(filtered(b"\x1b[?62;22"), b"\x1b[?62;22");
        assert_eq!(filtered(b"\x1b]11;rgb:1e1e"), b"\x1b]11;rgb:1e1e");
        assert_eq!(filtered(b"\x1bP!|7E56"), b"\x1bP!|7E56");
    }

    #[test]
    fn bounded_scan_does_not_strip_overlong_runs() {
        let mut input = Vec::from(&b"\x1b["[..]);
        input.extend_from_slice(&vec![b'1'; MAX_SEQUENCE_LEN + 8]);
        input.push(b'c');
        assert_eq!(
            filtered(&input),
            input,
            "past the scan bound: leave it alone"
        );
    }

    #[test]
    fn counters_track_what_was_removed() {
        let mut f = TermFilter::new();
        let out = f.filter(b"x\x1b[0n\x1b[24;80Ry");
        assert_eq!(out.as_ref(), b"xy");
        assert_eq!(f.stripped_sequences(), 2);
        assert_eq!(f.stripped_bytes(), 4 + 8);
    }
}
