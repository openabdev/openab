use unicode_segmentation::UnicodeSegmentation;

/// Byte length of the first grapheme cluster of `s` (0 if empty). Used to guarantee
/// forward progress when a single grapheme is wider than the target width.
fn first_grapheme_len(s: &str) -> usize {
    s.graphemes(true).next().map_or(0, str::len)
}

/// Byte index at which to cut `s` so the prefix is at most `max_chars` Unicode scalar
/// values **without splitting a grapheme cluster** (emoji, ZWJ sequences, regional-
/// indicator flags, VS16, combining marks all stay whole). When `word_wrap` and the cut
/// would land mid-word, it backtracks to just after the last whitespace in the prefix so
/// words / CJK runs are not broken mid-token. Returns `0` when not even the first
/// grapheme fits in `max_chars` (the caller decides whether to flush or force it).
fn split_point(s: &str, max_chars: usize, word_wrap: bool) -> usize {
    let mut chars = 0usize;
    let mut byte = 0usize;
    let mut last_ws_byte = 0usize; // byte index just past the last whitespace grapheme
    for (start, g) in s.grapheme_indices(true) {
        let g_chars = g.chars().count();
        if chars + g_chars > max_chars {
            break;
        }
        chars += g_chars;
        byte = start + g.len();
        if g.chars().all(char::is_whitespace) {
            last_ws_byte = byte;
        }
    }
    if word_wrap && byte < s.len() && last_ws_byte > 0 {
        return last_ws_byte;
    }
    byte
}

/// Split text into chunks at line boundaries, each <= limit Unicode characters (UTF-8 safe).
/// Discord's message limit counts Unicode characters, not bytes.
///
/// Fenced code blocks (``` ... ```) are handled specially: if a split falls inside a
/// code block, the current chunk is closed with ``` and the next chunk is reopened with
/// the original opener (preserving language tag), so each chunk renders correctly.
///
/// Hard-splitting an over-long line breaks on **grapheme cluster** boundaries (never
/// mid-emoji / ZWJ sequence / combining mark / CJK codepoint); outside code fences it
/// also prefers whitespace boundaries so words stay intact.
///
/// Invariant: every returned chunk satisfies `chunk.chars().count() <= limit`, except a
/// single grapheme cluster wider than `limit` (pathological), which is emitted whole.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len: usize = 0;
    // When inside a fenced code block, holds the full opener line (e.g. "```rust").
    let mut fence_opener: Option<String> = None;

    // Cost of appending "\n```" to close a fence before emitting a chunk.
    const CLOSE_COST: usize = 4; // '\n' + '`' + '`' + '`'

    for line in text.split('\n') {
        let line_chars = line.chars().count();
        let is_fence_line = line.starts_with("```");

        // Determine overhead that must be reserved when inside a fence.
        let close_reserve = if fence_opener.is_some() && !is_fence_line {
            CLOSE_COST
        } else {
            0
        };

        // Check whether appending this line (+ newline separator + close reserve) overflows.
        if !current.is_empty() && current_len + 1 + line_chars + close_reserve > limit {
            // Emit current chunk, closing fence if needed.
            if let Some(ref opener) = fence_opener {
                if !is_fence_line {
                    current.push_str("\n```");
                }
                chunks.push(std::mem::take(&mut current));
                // Reopen fence in next chunk with full opener (preserves language tag).
                current.push_str(opener);
                current_len = opener.chars().count();

                if is_fence_line {
                    // The closing fence marker itself triggers the split.
                    fence_opener = None;
                    current.push('\n');
                    current_len += 1;
                    current.push_str(line);
                    current_len += line_chars;
                    continue;
                } else if current_len + 1 + line_chars + CLOSE_COST <= limit {
                    // Line fits in the reopened chunk (with room for \n + line + close marker).
                    current.push('\n');
                    current_len += 1;
                    current.push_str(line);
                    current_len += line_chars;
                    continue;
                }
                // Otherwise: line doesn't fit even in a fresh reopened chunk.
                // Fall through to the normal line-processing logic below,
                // which will hit the hard-split path if line_chars > limit,
                // or the normal append path otherwise.
            } else {
                chunks.push(std::mem::take(&mut current));
                current_len = 0;
            }
        }

        // Newline separator between lines within a chunk.
        if !current.is_empty() {
            current.push('\n');
            current_len += 1;
        }

        // Track fence state.
        if is_fence_line {
            if fence_opener.is_some() {
                fence_opener = None;
            } else {
                fence_opener = Some(line.to_string());
            }
        }

        // Hard-split: single line exceeds available space.
        // This triggers when the line itself is longer than limit, OR when the
        // line doesn't fit in the current chunk even after accounting for fence
        // close overhead (e.g. after a reopen where opener already consumed space).
        let effective_avail = if fence_opener.is_some() {
            limit.saturating_sub(current_len + CLOSE_COST)
        } else {
            limit.saturating_sub(current_len)
        };
        if line_chars > effective_avail {
            let overhead = if let Some(ref opener) = fence_opener {
                // opener + '\n' at start, '\n```' at end
                opener.chars().count() + 1 + CLOSE_COST
            } else {
                0
            };
            // If limit can't even fit overhead, fall back to unfenced hard-split.
            let capacity = limit.saturating_sub(overhead);
            if let Some(opener) = fence_opener.as_ref().filter(|_| capacity > 0) {
                // Fenced hard-split: each mid chunk = opener\n + chars + \n```.
                // Grapheme-safe (never split an emoji / ZWJ / combining mark); no
                // word-wrap — code must not be reflowed at spaces.
                let opener_len = opener.chars().count();
                let mut rest = line;

                // Fill remaining space in current chunk first.
                let avail_first = if current_len > 0 {
                    limit.saturating_sub(current_len + CLOSE_COST)
                } else {
                    capacity
                };
                let cut = split_point(rest, avail_first, false);
                current.push_str(&rest[..cut]);
                current_len += rest[..cut].chars().count();
                rest = &rest[cut..];

                while !rest.is_empty() {
                    // Close current fenced chunk.
                    current.push_str("\n```");
                    chunks.push(std::mem::take(&mut current));
                    // Reopen.
                    current.push_str(opener);
                    current.push('\n');
                    current_len = opener_len + 1;
                    let mut cut = split_point(rest, capacity, false);
                    if cut == 0 {
                        cut = first_grapheme_len(rest); // grapheme wider than capacity
                    }
                    current.push_str(&rest[..cut]);
                    current_len += rest[..cut].chars().count();
                    rest = &rest[cut..];
                }
            } else {
                // Plain hard-split (no fence or limit too small for fence wrapping).
                // Grapheme-safe + prefer whitespace boundaries so words / CJK / emoji
                // stay intact.
                let mut rest = line;
                while !rest.is_empty() {
                    let avail = limit.saturating_sub(current_len);
                    let mut cut = split_point(rest, avail, true);
                    if cut == 0 {
                        if current.is_empty() {
                            cut = first_grapheme_len(rest); // grapheme wider than limit
                        } else {
                            // Nothing more fits in this chunk — flush and retry fresh.
                            chunks.push(std::mem::take(&mut current));
                            current_len = 0;
                            continue;
                        }
                    }
                    current.push_str(&rest[..cut]);
                    current_len += rest[..cut].chars().count();
                    rest = &rest[cut..];
                    if !rest.is_empty() {
                        chunks.push(std::mem::take(&mut current));
                        current_len = 0;
                    }
                }
            }
        } else {
            current.push_str(line);
            current_len += line_chars;
        }
    }

    if !current.is_empty() {
        // Close any trailing open fence.
        if fence_opener.is_some() {
            current.push_str("\n```");
        }
        chunks.push(current);
    }
    chunks
}

/// Shorten a prompt into a thread title: collapse GitHub URLs and cap at 40 chars.
pub fn shorten_thread_name(prompt: &str) -> String {
    use std::sync::LazyLock;
    static GH_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"https?://github\.com/([^/]+/[^/]+)/(issues|pull)/(\d+)").unwrap()
    });
    // Strip @(role) and @(user) placeholders left by resolve_mentions()
    let cleaned = prompt.replace("@(role)", "").replace("@(user)", "");
    let shortened = GH_RE.replace_all(cleaned.trim(), "$1#$3");
    let name: String = shortened.chars().take(40).collect();
    if name.len() < shortened.len() {
        format!("{name}...")
    } else {
        name
    }
}

/// Truncate a string to at most `limit` Unicode characters, keeping the tail
/// (most recent output) for better streaming UX.
pub fn truncate_chars_tail(s: &str, limit: usize) -> String {
    let total = s.chars().count();
    if total <= limit {
        return s.to_string();
    }
    s.chars().skip(total - limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: assert every chunk respects the limit.
    fn assert_length_invariant(chunks: &[String], limit: usize) {
        for (i, chunk) in chunks.iter().enumerate() {
            let len = chunk.chars().count();
            assert!(
                len <= limit,
                "chunk {i} has {len} chars, exceeds limit {limit}:\n{chunk}"
            );
        }
    }

    #[test]
    fn no_split_under_limit() {
        let text = "hello\nworld";
        let chunks = split_message(text, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], text);
    }

    #[test]
    fn plain_text_split_respects_limit() {
        let text = "aaaa\nbbbb\ncccc\ndddd";
        let chunks = split_message(text, 10);
        assert_length_invariant(&chunks, 10);
        assert!(chunks.len() > 1);
    }

    #[test]
    fn fenced_split_preserves_language_tag() {
        // ```rust\n + 1990 chars of content + \n```  — should split
        let content_line = "x".repeat(1990);
        let text = format!("```rust\n{content_line}\nanother line here\n```");
        let chunks = split_message(&text, 2000);
        assert_length_invariant(&chunks, 2000);
        // First chunk should start with ```rust
        assert!(chunks[0].starts_with("```rust"));
        // If split happened, second chunk should reopen with ```rust
        if chunks.len() > 1 {
            assert!(
                chunks[1].starts_with("```rust"),
                "second chunk should reopen with language tag: {}",
                &chunks[1][..chunks[1].len().min(20)]
            );
        }
    }

    #[test]
    fn fenced_split_close_overhead_budgeted() {
        // Construct a fenced block where content + close marker would overflow
        // without proper budgeting.
        // limit=50, opener="```" (3), close="\n```" (4)
        // Available for content per chunk: 50 - 3 - 1 - 4 = 42 (with opener+newline+close)
        let line1 = "a".repeat(40);
        let line2 = "b".repeat(40);
        let text = format!("```\n{line1}\n{line2}\n```");
        let chunks = split_message(&text, 50);
        assert_length_invariant(&chunks, 50);
    }

    #[test]
    fn reopen_path_no_overflow() {
        // Regression: limit=2000, fenced block with a 1996-char line.
        // Old code would produce 2004-char chunk due to reopen + extra \n.
        let content = "x".repeat(1990);
        let text = format!("```rust\n{content}\nshort\n```");
        let chunks = split_message(&text, 2000);
        assert_length_invariant(&chunks, 2000);
    }

    #[test]
    fn hard_split_fenced_respects_limit() {
        // A single very long line inside a fence.
        let long_line = "x".repeat(100);
        let text = format!("```\n{long_line}\n```");
        let chunks = split_message(&text, 20);
        assert_length_invariant(&chunks, 20);
        // All content should be present
        let total_x: usize = chunks
            .iter()
            .map(|c| c.chars().filter(|&ch| ch == 'x').count())
            .sum();
        assert_eq!(total_x, 100);
    }

    #[test]
    fn hard_split_plain_respects_limit() {
        let long_line = "y".repeat(50);
        let text = format!("before\n{long_line}\nafter");
        let chunks = split_message(&text, 10);
        assert_length_invariant(&chunks, 10);
    }

    #[test]
    fn closing_fence_triggers_split() {
        // The closing ``` itself pushes over the limit.
        let content = "a".repeat(44);
        // "```\n" + 44 chars + "\n```" = 3 + 1 + 44 + 1 + 3 = 52
        let text = format!("```\n{content}\n```");
        let chunks = split_message(&text, 50);
        assert_length_invariant(&chunks, 50);
    }

    #[test]
    fn multi_fence_blocks() {
        let text = "text\n```python\ncode1\ncode2\n```\nmore text\n```js\ncode3\n```";
        let chunks = split_message(text, 25);
        assert_length_invariant(&chunks, 25);
    }

    #[test]
    fn fence_balance_across_chunks() {
        // Every chunk should have balanced fences (even number of ``` lines).
        let content = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = format!("```\n{content}\n```");
        let chunks = split_message(&text, 30);
        assert_length_invariant(&chunks, 30);
        for (i, chunk) in chunks.iter().enumerate() {
            let fence_count = chunk.lines().filter(|l| l.starts_with("```")).count();
            assert!(
                fence_count.is_multiple_of(2),
                "chunk {i} has unbalanced fences ({fence_count}):\n{chunk}"
            );
        }
    }

    #[test]
    fn grapheme_clusters_never_split() {
        use unicode_segmentation::UnicodeSegmentation;
        // multi-codepoint graphemes a char-based split would break: astral emoji,
        // ZWJ family, flag, VS16, astral CJK, plus plain BMP chars.
        let graphemes = ["🎉", "👨‍👩‍👧‍👦", "🇹🇼", "❤️", "𠀀", "a", "你", "🙂"];
        let line: String = graphemes.iter().copied().cycle().take(48).collect();
        for limit in [5, 8, 13, 20] {
            let chunks = split_message(&line, limit);
            // No grapheme split: flattening chunk graphemes reproduces the original
            // grapheme sequence exactly (a split grapheme would re-segment differently).
            let flat: Vec<&str> = chunks.iter().flat_map(|c| c.graphemes(true)).collect();
            let orig: Vec<&str> = line.graphemes(true).collect();
            assert_eq!(flat, orig, "grapheme cluster split at limit {limit}");
        }
    }

    #[test]
    fn plain_hard_split_prefers_whitespace() {
        // A long run of short words: chunks should break at spaces, not mid-word.
        let line = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo";
        let chunks = split_message(line, 20);
        assert_length_invariant(&chunks, 20);
        assert!(chunks.len() > 1);
        let rejoined = chunks.iter().map(|c| c.trim()).collect::<Vec<_>>().join(" ");
        assert_eq!(
            rejoined.split_whitespace().collect::<Vec<_>>(),
            line.split_whitespace().collect::<Vec<_>>(),
            "words were broken or lost by the hard-split"
        );
    }

    #[test]
    fn cjk_hard_split_breaks_between_characters() {
        // A long CJK run (no whitespace) must break on codepoint/grapheme bounds and
        // preserve every character.
        let line: String = std::iter::repeat_n('好', 50).collect();
        let chunks = split_message(&line, 10);
        assert_length_invariant(&chunks, 10);
        assert_eq!(chunks.concat(), line);
    }

    #[test]
    fn fenced_hard_split_grapheme_safe() {
        // Emoji inside a code fence: grapheme-safe, all content preserved.
        let content: String = std::iter::repeat("🎉❤️你").take(20).collect();
        let text = format!("```\n{content}\n```");
        let chunks = split_message(&text, 20);
        assert_length_invariant(&chunks, 20);
        let party: usize = chunks.iter().map(|c| c.matches('🎉').count()).sum();
        assert_eq!(party, 20, "emoji lost across fenced hard-split");
    }
}
