//! Message formatting and chunking utilities.
//!
//! Adapted from hermes-agent's `base.py truncate_message()`.
//! Reference: <https://github.com/NousResearch/hermes-agent>

/// Split a long message into chunks, preserving code block boundaries.
///
/// When a split falls inside a triple-backtick code block, the fence is
/// closed at the end of the current chunk and reopened (with the original
/// language tag) at the start of the next chunk.
///
/// Set `add_indicator` to false when sending chunks live (e.g. streaming edits)
/// to avoid the "(1/N)" marker appearing in the message body.
pub fn split_message(text: &str, limit: usize, add_indicator: bool) -> Vec<String> {
    if text.len() <= limit {
        return vec![text.to_string()];
    }

    let indicator_len = 10; // " (XX/XX)"
    let fence_close = "\n```";

    let mut chunks: Vec<String> = Vec::new();
    let mut remaining = text;
    // When the previous chunk ended mid-code-block, this holds the
    // language tag so we can reopen the fence in the next chunk.
    let mut carry_lang: Option<String> = None;

    while !remaining.is_empty() {
        // If continuing a code block, prepend the reopening fence.
        let prefix = carry_lang
            .as_ref()
            .map(|lang| format!("```{}\n", lang))
            .unwrap_or_default();

        // How much body text we can fit after prefix + potential fence close + indicator.
        let mut headroom = limit
            .saturating_sub(indicator_len)
            .saturating_sub(prefix.len())
            .saturating_sub(fence_close.len());

        // Edge case: limit is too small to hold anything useful.
        if headroom < 1 {
            headroom = limit / 2;
        }

        // Everything fits in one final chunk.
        if prefix.len() + remaining.len() <= limit.saturating_sub(indicator_len) {
            chunks.push(prefix.to_string() + remaining);
            break;
        }

        // Find a natural split point: prefer newline, then space, then byte boundary.
        let region = &remaining[..headroom.min(remaining.len())];
        let mut split_at = region.rfind('\n').unwrap_or(0);

        if split_at < headroom / 2 {
            split_at = region.rfind(' ').unwrap_or(0);
        }
        if split_at < 1 {
            split_at = headroom;
        }
        split_at = split_at.min(remaining.len());

        // Avoid splitting inside an inline code span (`...`).
        // If the text before split_at has an odd number of backticks,
        // the split would fall inside an unclosed inline code block.
        let candidate = &remaining[..split_at];
        let backtick_count = candidate.chars().filter(|&c| c == '`').count();
        if backtick_count % 2 == 1 {
            // Find the last unescaped backtick and split before it.
            if let Some(last_bt) = candidate.rfind('`') {
                let safe_space = candidate[..last_bt].rfind(' ').unwrap_or(0);
                let safe_nl = candidate[..last_bt].rfind('\n').unwrap_or(0);
                let safe_split = safe_space.max(safe_nl);
                if safe_split > headroom / 4 {
                    split_at = safe_split;
                }
            }
        }

        let chunk_body = &remaining[..split_at];
        remaining = remaining[split_at..].trim_start();

        let full_chunk = format!("{}{}", prefix, chunk_body);

        // Walk chunk_body to determine if we're ending inside a code block.
        let mut in_code = carry_lang.is_some();
        let mut lang = carry_lang.clone().unwrap_or_default();
        for line in chunk_body.lines() {
            let stripped = line.trim();
            if stripped.starts_with("```") {
                if in_code {
                    in_code = false;
                    lang.clear();
                } else {
                    in_code = true;
                    lang = stripped[3..]
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_string();
                }
            }
        }

        if in_code {
            // Close the orphaned fence so this chunk is valid on its own.
            chunks.push(full_chunk + fence_close);
            carry_lang = Some(lang);
        } else {
            carry_lang = None;
            chunks.push(full_chunk);
        }
    }

    // Append (N/N) indicators when the response spans multiple messages.
    if add_indicator && chunks.len() > 1 {
        let total = chunks.len();
        chunks = chunks
            .into_iter()
            .enumerate()
            .map(|(i, chunk)| format!("{} ({}/{})", chunk, i + 1, total))
            .collect();
    }

    chunks
}

/// Streaming-friendly split — no indicators, for live Discord edits.
pub fn split_message_for_streaming(text: &str, limit: usize) -> Vec<String> {
    split_message(text, limit, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_under_limit() {
        let text = "hello";
        let chunks = split_message(text, 200, true);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello");
    }

    #[test]
    fn test_simple_split() {
        let text = "hello\nworld\nfoo\nbar";
        let chunks = split_message(text, 5, true);
        assert!(chunks.len() > 1);
    }

    #[test]
    fn test_code_block_preserved() {
        let text = "hello\n```python\nx = 1\ny = 2\n```\nworld";
        let chunks = split_message(text, 20, true);
        let all: String = chunks.join("");
        assert_eq!(
            all.matches("```").count() % 2,
            0,
            "fences should be balanced: {:?}",
            chunks
        );
    }

    #[test]
    fn test_very_long_line() {
        let limit = 2000;
        let text = "a".repeat(5000);
        let chunks = split_message(&text, limit, true);
        assert!(chunks.len() > 1);
        let indicator_len = 10; // " (XX/XX)"
        for chunk in &chunks {
            assert!(
                chunk.len() <= limit + indicator_len,
                "chunk len {} exceeds limit+indicator ({}+{})",
                chunk.len(),
                limit,
                indicator_len
            );
        }
    }

    #[test]
    fn test_indicators_added() {
        // Force multi-chunk with small limit.
        let text = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10";
        let chunks = split_message(text, 50, true);
        assert!(chunks.len() > 1, "should be split: {:?}", chunks);
        let total = chunks.len();
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(
                chunk.ends_with(&format!(" ({}/{})", i + 1, total)),
                "chunk {} should end with ({}/{}): {:?}",
                i,
                i + 1,
                total,
                chunk
            );
        }
    }

    #[test]
    fn test_inline_code_not_split() {
        // Verify fences stay balanced even with inline code.
        let text = "hello\n```python\nx = 1\n```\n`inline code here`\nworld";
        let chunks = split_message(text, 30, true);
        let all: String = chunks.join("");
        let fence_count = all.matches("```").count();
        assert!(
            fence_count % 2 == 0,
            "all triple-backtick fences should be balanced, got {} in {:?}",
            fence_count,
            chunks
        );
    }
}
