/// Split text into chunks at line boundaries, each <= limit Unicode characters (UTF-8 safe).
/// Discord's message limit counts Unicode characters, not bytes.
///
/// Fenced code blocks (``` ... ```) are handled specially: if a split falls inside a
/// code block, the current chunk is closed with ``` and the next chunk is reopened with
/// ```, so each chunk renders correctly in Discord.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len: usize = 0;
    let mut in_code_fence = false;

    for line in text.split('\n') {
        let line_chars = line.chars().count();
        let is_fence_marker = line.starts_with("```");

        // +1 for the newline
        if !current.is_empty() && current_len + line_chars + 1 > limit {
            if in_code_fence && !is_fence_marker {
                // Close the open code fence so this chunk renders correctly.
                current.push_str("\n```");
            }
            chunks.push(current);
            current = String::new();
            current_len = 0;
            if in_code_fence && !is_fence_marker {
                // Reopen the code fence in the new chunk.
                // The newline separator below will join it to the first content line.
                current.push_str("```");
                current_len = 3;
            }
        }

        if !current.is_empty() {
            current.push('\n');
            current_len += 1;
        }

        if is_fence_marker {
            in_code_fence = !in_code_fence;
        }

        // If a single line exceeds limit, hard-split on char boundaries
        if line_chars > limit {
            for ch in line.chars() {
                if current_len + 1 > limit {
                    chunks.push(current);
                    current = String::new();
                    current_len = 0;
                }
                current.push(ch);
                current_len += 1;
            }
        } else {
            current.push_str(line);
            current_len += line_chars;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_under_limit_returns_single_chunk() {
        let text = "hello world";
        assert_eq!(split_message(text, 2000), vec![text.to_string()]);
    }

    #[test]
    fn split_code_fence_closed_and_reopened_across_chunks() {
        // Build a fenced code block whose lines exceed the limit when combined.
        // Each data line is 100 chars; 21 lines = 2121 chars inside the fence,
        // forcing a split mid-block.
        let row = format!("| {} |\n", "x".repeat(95)); // 100 chars per row
        let mut text = String::from("```\n");
        for _ in 0..21 {
            text.push_str(&row);
        }
        text.push_str("```\n");

        let chunks = split_message(&text, 2000);
        assert!(chunks.len() >= 2, "expected multiple chunks");
        for (i, chunk) in chunks.iter().enumerate() {
            let fence_count = chunk.lines().filter(|l| l.starts_with("```")).count();
            assert_eq!(
                fence_count % 2,
                0,
                "chunk {i} has unmatched code fences:\n{chunk}"
            );
        }
    }

    #[test]
    fn split_does_not_corrupt_content_outside_fence() {
        let mut text = String::new();
        for i in 0..30 {
            text.push_str(&format!("Line number {i} with some padding text here.\n"));
        }
        let original_lines: Vec<&str> = text.lines().collect();
        let chunks = split_message(&text, 200);
        let rejoined: Vec<&str> = chunks.iter().flat_map(|c| c.lines()).collect();
        assert_eq!(original_lines, rejoined);
    }
}

/// Truncate a string to at most `limit` Unicode characters.
/// Discord's message limit counts Unicode characters, not bytes.
pub fn truncate_chars(s: &str, limit: usize) -> &str {
    match s.char_indices().nth(limit) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
