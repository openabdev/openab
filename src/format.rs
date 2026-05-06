/// Split text into chunks at line boundaries, each <= limit Unicode characters (UTF-8 safe).
/// Discord's message limit counts Unicode characters, not bytes.
///
/// Fenced code blocks (``` ... ```) are handled specially: if a split falls inside a
/// code block, the current chunk is closed with ``` and the next chunk is reopened with
/// the original opener (preserving language tag), so each chunk renders correctly.
///
/// Invariant: every returned chunk satisfies `chunk.chars().count() <= limit`.
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
                // Fenced hard-split: each mid chunk = opener\n + chars + \n```
                let opener_len = opener.chars().count();
                let mut chars = line.chars().peekable();

                // Fill remaining space in current chunk first.
                let avail_first = if current_len > 0 {
                    limit.saturating_sub(current_len + CLOSE_COST)
                } else {
                    capacity
                };
                for _ in 0..avail_first {
                    if let Some(ch) = chars.next() {
                        current.push(ch);
                        current_len += 1;
                    } else {
                        break;
                    }
                }

                while chars.peek().is_some() {
                    // Close current fenced chunk.
                    current.push_str("\n```");
                    chunks.push(std::mem::take(&mut current));
                    // Reopen.
                    current.push_str(opener);
                    current.push('\n');
                    current_len = opener_len + 1;
                    for _ in 0..capacity {
                        if let Some(ch) = chars.next() {
                            current.push(ch);
                            current_len += 1;
                        } else {
                            break;
                        }
                    }
                }
            } else {
                // Plain hard-split (no fence or limit too small for fence wrapping).
                for ch in line.chars() {
                    if current_len >= limit {
                        chunks.push(std::mem::take(&mut current));
                        current_len = 0;
                    }
                    current.push(ch);
                    current_len += 1;
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
                fence_count % 2 == 0,
                "chunk {i} has unbalanced fences ({fence_count}):\n{chunk}"
            );
        }
    }
}
