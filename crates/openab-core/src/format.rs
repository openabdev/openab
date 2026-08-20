use std::fmt;
use unicode_segmentation::UnicodeSegmentation;

/// Internal measurement used by the final-content splitter. Wire capabilities
/// map to this type without collapsing byte-based limits into character counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextBudget {
    Characters(usize),
    Bytes(usize),
    Utf16Bytes(usize),
    Unlimited,
}

impl TextBudget {
    fn max(self) -> Option<usize> {
        match self {
            Self::Characters(max) | Self::Bytes(max) | Self::Utf16Bytes(max) => Some(max),
            Self::Unlimited => None,
        }
    }

    pub(crate) fn measure(self, value: &str) -> usize {
        match self {
            Self::Characters(_) => value.chars().count(),
            Self::Bytes(_) => value.len(),
            Self::Utf16Bytes(_) => value.encode_utf16().count().saturating_mul(2),
            Self::Unlimited => 0,
        }
    }

    fn scalar_cost(self, value: char) -> usize {
        match self {
            Self::Characters(_) => 1,
            Self::Bytes(_) => value.len_utf8(),
            Self::Utf16Bytes(_) => value.len_utf16().saturating_mul(2),
            Self::Unlimited => 0,
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::Characters(_) => "characters",
            Self::Bytes(_) => "bytes",
            Self::Utf16Bytes(_) => "UTF-16 bytes",
            Self::Unlimited => "unlimited",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SplitMessageError {
    unit: &'static str,
    max: usize,
    required: usize,
}

impl SplitMessageError {
    fn new(budget: TextBudget, max: usize, required: usize) -> Self {
        Self {
            unit: budget.unit(),
            max,
            required,
        }
    }
}

impl fmt::Display for SplitMessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "message cannot be split within a {} budget of {} (smallest required unit costs {})",
            self.unit, self.max, self.required
        )
    }
}

impl std::error::Error for SplitMessageError {}

/// Last-resort scalar-boundary split used only when one extended grapheme is
/// wider than the whole budget. Returns an error when even one Unicode scalar
/// cannot fit, because emitting invalid UTF-8 or an oversized chunk is unsafe.
fn scalar_split_point(
    value: &str,
    max: usize,
    budget: TextBudget,
) -> Result<usize, SplitMessageError> {
    let mut used = 0usize;
    let mut byte = 0usize;
    let mut first_cost = 0usize;
    for (start, scalar) in value.char_indices() {
        let cost = budget.scalar_cost(scalar);
        if first_cost == 0 {
            first_cost = cost;
        }
        if used.saturating_add(cost) > max {
            break;
        }
        used += cost;
        byte = start + scalar.len_utf8();
    }
    if byte == 0 && !value.is_empty() {
        Err(SplitMessageError::new(budget, max, first_cost))
    } else {
        Ok(byte)
    }
}

/// Byte index at which to cut `value` without splitting an extended grapheme.
/// When `word_wrap` is true, prefer the last whitespace boundary in the fitting
/// prefix. Returns zero when the first grapheme does not fit.
fn split_point(value: &str, max: usize, word_wrap: bool, budget: TextBudget) -> usize {
    let mut used = 0usize;
    let mut byte = 0usize;
    let mut last_ws_byte = 0usize;
    for (start, grapheme) in value.grapheme_indices(true) {
        let cost = budget.measure(grapheme);
        if used.saturating_add(cost) > max {
            break;
        }
        used += cost;
        byte = start + grapheme.len();
        if grapheme.chars().all(char::is_whitespace) {
            last_ws_byte = byte;
        }
    }
    if word_wrap && byte < value.len() && last_ws_byte > 0 {
        return last_ws_byte;
    }
    byte
}

/// Compatibility wrapper for callers whose platform limit is measured in
/// Unicode scalar values. A zero legacy limit is clamped to one so malformed
/// configuration cannot create an infinite loop.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    match split_message_with_budget(text, TextBudget::Characters(limit.max(1))) {
        Ok(chunks) => chunks,
        // A positive character budget can fit every Unicode scalar. Retain a
        // content-preserving fallback if that internal invariant ever regresses.
        Err(_) => text.chars().map(|value| value.to_string()).collect(),
    }
}

/// Split final content according to an exact platform budget. Fenced code blocks
/// are closed and reopened around splits, with those synthetic markers charged
/// to the same budget as the content.
pub(crate) fn split_message_with_budget(
    text: &str,
    budget: TextBudget,
) -> Result<Vec<String>, SplitMessageError> {
    let Some(limit) = budget.max() else {
        return Ok(vec![text.to_string()]);
    };
    if text.is_empty() {
        return Ok(vec![String::new()]);
    }
    if limit == 0 {
        let required = text
            .chars()
            .next()
            .map_or(1, |value| budget.scalar_cost(value));
        return Err(SplitMessageError::new(budget, limit, required));
    }
    if budget.measure(text) <= limit {
        return Ok(vec![text.to_string()]);
    }

    let newline_cost = budget.measure("\n");
    let close_marker = "\n```";
    let close_cost = budget.measure(close_marker);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    let mut fence_opener: Option<String> = None;

    for line in text.split('\n') {
        let line_len = budget.measure(line);
        let is_fence_line = line.starts_with("```");
        let opens_fence = is_fence_line && fence_opener.is_none();
        let close_reserve = if opens_fence || (fence_opener.is_some() && !is_fence_line) {
            close_cost
        } else {
            0
        };

        if !current.is_empty()
            && current_len
                .saturating_add(newline_cost)
                .saturating_add(line_len)
                .saturating_add(close_reserve)
                > limit
        {
            if let Some(ref opener) = fence_opener {
                // Close the active block before every split, including when an
                // unusually long original closing-fence line caused the split.
                current.push_str(close_marker);
                chunks.push(std::mem::take(&mut current));
                current.push_str(opener);
                current_len = budget.measure(opener);

                if is_fence_line {
                    fence_opener = None;
                    current.push('\n');
                    current_len = current_len.saturating_add(newline_cost);
                    current.push_str(line);
                    current_len = current_len.saturating_add(line_len);
                    continue;
                } else if current_len
                    .saturating_add(newline_cost)
                    .saturating_add(line_len)
                    .saturating_add(close_cost)
                    <= limit
                {
                    current.push('\n');
                    current_len += newline_cost;
                    current.push_str(line);
                    current_len += line_len;
                    continue;
                }
            } else {
                chunks.push(std::mem::take(&mut current));
                current_len = 0;
            }
        }

        if !current.is_empty() {
            current.push('\n');
            current_len = current_len.saturating_add(newline_cost);
        }

        if is_fence_line {
            if fence_opener.is_some() {
                fence_opener = None;
            } else {
                let required = line_len.saturating_add(close_cost);
                if required > limit {
                    return Err(SplitMessageError::new(budget, limit, required));
                }
                fence_opener = Some(line.to_string());
            }
        }

        let effective_avail = if fence_opener.is_some() {
            limit.saturating_sub(current_len.saturating_add(close_cost))
        } else {
            limit.saturating_sub(current_len)
        };
        if line_len > effective_avail {
            let overhead = fence_opener.as_ref().map_or(0, |opener| {
                budget
                    .measure(opener)
                    .saturating_add(newline_cost)
                    .saturating_add(close_cost)
            });
            let capacity = limit.saturating_sub(overhead);
            if let Some(opener) = fence_opener.as_ref() {
                if capacity == 0 {
                    let scalar_cost = line
                        .chars()
                        .next()
                        .map_or(1, |value| budget.scalar_cost(value));
                    return Err(SplitMessageError::new(
                        budget,
                        limit,
                        overhead.saturating_add(scalar_cost),
                    ));
                }

                let opener_len = budget.measure(opener);
                let mut rest = line;
                let avail_first = if current_len > 0 {
                    limit.saturating_sub(current_len.saturating_add(close_cost))
                } else {
                    capacity
                };
                let cut = split_point(rest, avail_first, false, budget);
                current.push_str(&rest[..cut]);
                current_len = current_len.saturating_add(budget.measure(&rest[..cut]));
                rest = &rest[cut..];

                while !rest.is_empty() {
                    current.push_str(close_marker);
                    chunks.push(std::mem::take(&mut current));
                    current.push_str(opener);
                    current.push('\n');
                    current_len = opener_len.saturating_add(newline_cost);
                    let mut cut = split_point(rest, capacity, false, budget);
                    if cut == 0 {
                        cut = match scalar_split_point(rest, capacity, budget) {
                            Ok(cut) => cut,
                            Err(error) => {
                                return Err(SplitMessageError::new(
                                    budget,
                                    limit,
                                    overhead.saturating_add(error.required),
                                ));
                            }
                        };
                    }
                    current.push_str(&rest[..cut]);
                    current_len = current_len.saturating_add(budget.measure(&rest[..cut]));
                    rest = &rest[cut..];
                }
            } else {
                let mut rest = line;
                while !rest.is_empty() {
                    let avail = limit.saturating_sub(current_len);
                    let mut cut = split_point(rest, avail, true, budget);
                    if cut == 0 {
                        if current.is_empty() {
                            cut = scalar_split_point(rest, avail, budget)?;
                        } else {
                            chunks.push(std::mem::take(&mut current));
                            current_len = 0;
                            continue;
                        }
                    }
                    current.push_str(&rest[..cut]);
                    current_len = current_len.saturating_add(budget.measure(&rest[..cut]));
                    rest = &rest[cut..];
                    if !rest.is_empty() {
                        chunks.push(std::mem::take(&mut current));
                        current_len = 0;
                    }
                }
            }
        } else {
            current.push_str(line);
            current_len = current_len.saturating_add(line_len);
        }
    }

    if !current.is_empty() {
        if fence_opener.is_some() {
            current.push_str(close_marker);
        }
        chunks.push(current);
    }

    if let Some(oversized) = chunks
        .iter()
        .map(|chunk| budget.measure(chunk))
        .find(|measured| *measured > limit)
    {
        return Err(SplitMessageError::new(budget, limit, oversized));
    }
    Ok(chunks)
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

    fn assert_budget_invariant(chunks: &[String], budget: TextBudget, limit: usize) {
        for (index, chunk) in chunks.iter().enumerate() {
            let measured = budget.measure(chunk);
            assert!(
                measured <= limit,
                "chunk {index} measures {measured}, exceeds {limit}: {chunk:?}"
            );
        }
    }

    fn split_for_test(text: &str, budget: TextBudget) -> Vec<String> {
        match split_message_with_budget(text, budget) {
            Ok(chunks) => chunks,
            Err(error) => panic!("expected split success: {error}"),
        }
    }

    fn split_error_for_test(text: &str, budget: TextBudget) -> SplitMessageError {
        match split_message_with_budget(text, budget) {
            Ok(chunks) => panic!("expected split failure, got {} chunks", chunks.len()),
            Err(error) => error,
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
    fn closing_fence_with_suffix_keeps_every_split_chunk_balanced() {
        let text = "```\naaaaaa\n``` x";
        let budget = TextBudget::Characters(15);
        let chunks = split_for_test(text, budget);
        assert_eq!(chunks.len(), 2);
        assert_budget_invariant(&chunks, budget, 15);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.matches('a').count())
                .sum::<usize>(),
            6
        );
        assert_eq!(
            chunks
                .iter()
                .filter(|chunk| chunk.lines().any(|line| line == "``` x"))
                .count(),
            1
        );
        for chunk in chunks {
            let fences = chunk.lines().filter(|line| line.starts_with("```")).count();
            assert!(fences.is_multiple_of(2), "unbalanced chunk: {chunk:?}");
        }
    }

    #[test]
    fn fence_overhead_that_cannot_fit_fails_closed() {
        let no_content_capacity = split_error_for_test("```\nx\n```", TextBudget::Characters(8));
        assert_eq!(no_content_capacity.max, 8);
        assert_eq!(no_content_capacity.required, 9);

        let oversized_opener = split_error_for_test("```rust\nx\n```", TextBudget::Characters(10));
        assert_eq!(oversized_opener.max, 10);
        assert_eq!(oversized_opener.required, 11);
    }

    #[test]
    fn prose_splits_before_an_opener_that_needs_close_reserve() {
        let text = "aaaaa\n```\nx\n```";
        let budget = TextBudget::Characters(10);
        let chunks = split_for_test(text, budget);
        assert_eq!(chunks.len(), 2);
        assert_budget_invariant(&chunks, budget, 10);
        assert_eq!(chunks[0], "aaaaa");
        assert_eq!(chunks[1], "```\nx\n```");
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
        // Limits >= the widest grapheme (the 7-scalar ZWJ family) so every grapheme fits
        // and none is split. Graphemes WIDER than the limit are the last-resort codepoint
        // split, covered by `oversized_grapheme_still_respects_limit`.
        for limit in [8, 13, 20] {
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

    #[test]
    fn oversized_grapheme_still_respects_limit() {
        // A single grapheme wider than the limit must still be bounded to <= limit
        // (last-resort codepoint split) so every chunk stays deliverable; content is
        // preserved byte-exact.
        let family = "👨‍👩‍👧‍👦"; // one grapheme, 7 scalar values
        for limit in [2, 3, 5] {
            let chunks = split_message(family, limit);
            assert_length_invariant(&chunks, limit);
            assert_eq!(chunks.concat(), family, "content lost at limit {limit}");
        }
    }

    #[test]
    fn oversized_grapheme_with_mention_reserve() {
        // Discord path: the caller reduces the limit by a mention-footer reserve before
        // calling split_message; the reduced limit must still hold even when a single
        // grapheme is wider than it, so the footer's reserved capacity is never eaten.
        let text = format!("❤️{fam}{fam}", fam = "👨‍👩‍👧‍👦");
        let limit = 10;
        let reserve = 4;
        let effective = limit - reserve; // 6
        let chunks = split_message(&text, effective);
        assert_length_invariant(&chunks, effective);
        assert_eq!(chunks.concat(), text, "content lost with mention reserve");
    }

    #[test]
    fn utf16_budget_counts_bmp_and_supplementary_scalars_exactly() {
        let text = "A🙂B🙂C🙂D";
        let budget = TextBudget::Utf16Bytes(10);
        let chunks = split_for_test(text, budget);
        assert_budget_invariant(&chunks, budget, 10);
        assert_eq!(chunks.concat(), text);
        assert_eq!(budget.measure("A"), 2);
        assert_eq!(budget.measure("🙂"), 4);
        assert!(chunks.len() > 1);
    }

    #[test]
    fn utf8_byte_budget_differs_from_utf16_budget() {
        let text = "éé🙂abc";
        let byte_budget = TextBudget::Bytes(6);
        let utf16_budget = TextBudget::Utf16Bytes(6);
        let byte_chunks = split_for_test(text, byte_budget);
        let utf16_chunks = split_for_test(text, utf16_budget);
        assert_budget_invariant(&byte_chunks, byte_budget, 6);
        assert_budget_invariant(&utf16_chunks, utf16_budget, 6);
        assert_eq!(byte_chunks.concat(), text);
        assert_eq!(utf16_chunks.concat(), text);
        assert_ne!(byte_chunks, utf16_chunks);
    }

    #[test]
    fn mixed_unicode_exact_budgets_preserve_content_and_bounds() {
        let text = "A你e\u{301}🙂👨‍👩‍👧‍👦 Z".repeat(5);
        let budgets = [
            TextBudget::Characters(4),
            TextBudget::Bytes(4),
            TextBudget::Utf16Bytes(4),
        ];
        for budget in budgets {
            let chunks = split_for_test(&text, budget);
            let limit = budget.max().unwrap_or_default();
            assert_budget_invariant(&chunks, budget, limit);
            assert_eq!(chunks.concat(), text);
        }
    }

    #[test]
    fn teams_decimal_utf16_budget_is_exact_at_supplementary_boundary() {
        let text = format!("{}🙂", "a".repeat(39_999));
        let budget = TextBudget::Utf16Bytes(80_000);
        let chunks = split_for_test(&text, budget);
        assert_eq!(chunks.len(), 2);
        assert_budget_invariant(&chunks, budget, 80_000);
        assert_eq!(chunks.concat(), text);
        assert_eq!(budget.measure(&chunks[0]), 79_998);
        assert_eq!(budget.measure(&chunks[1]), 4);
    }

    #[test]
    fn utf16_fenced_chunks_charge_synthetic_markers() {
        let content = "🙂".repeat(20);
        let text = format!("```rust\n{content}\n```");
        let budget = TextBudget::Utf16Bytes(48);
        let chunks = split_for_test(&text, budget);
        assert_budget_invariant(&chunks, budget, 48);
        assert!(chunks.len() > 1);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.matches('🙂').count())
                .sum::<usize>(),
            20
        );
        for chunk in chunks {
            let fences = chunk.lines().filter(|line| line.starts_with("```")).count();
            assert!(fences.is_multiple_of(2), "unbalanced chunk: {chunk:?}");
        }
    }

    #[test]
    fn budget_split_keeps_graphemes_when_they_fit() {
        let family = "👨‍👩‍👧‍👦";
        let text = format!("{family} {family} {family}");
        let one_family = TextBudget::Utf16Bytes(usize::MAX).measure(family);
        let budget = TextBudget::Utf16Bytes(one_family + 2);
        let chunks = split_for_test(&text, budget);
        assert_budget_invariant(&chunks, budget, one_family + 2);
        let flattened: Vec<&str> = chunks
            .iter()
            .flat_map(|chunk| chunk.graphemes(true))
            .collect();
        let original: Vec<&str> = text.graphemes(true).collect();
        assert_eq!(flattened, original);
    }

    #[test]
    fn unlimited_budget_returns_one_unchanged_chunk() {
        let text = "```rust\nfn main() {}\n```\n🙂".repeat(100);
        assert_eq!(split_for_test(&text, TextBudget::Unlimited), vec![text]);
    }

    #[test]
    fn impossible_budget_fails_without_invalid_utf8_or_oversize() {
        let utf16 = split_error_for_test("🙂", TextBudget::Utf16Bytes(2));
        assert_eq!(utf16.max, 2);
        assert_eq!(utf16.required, 4);

        let utf8 = split_error_for_test("é", TextBudget::Bytes(1));
        assert_eq!(utf8.max, 1);
        assert_eq!(utf8.required, 2);

        let zero = split_error_for_test("a", TextBudget::Characters(0));
        assert_eq!(zero.max, 0);
        assert_eq!(zero.required, 1);
    }
}
