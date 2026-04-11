/// Split text into chunks at line boundaries, each <= limit Unicode characters (UTF-8 safe).
/// Discord's message limit counts Unicode characters, not bytes.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len: usize = 0;

    for line in text.split('\n') {
        let line_chars = line.chars().count();
        // +1 for the newline
        if !current.is_empty() && current_len + line_chars + 1 > limit {
            chunks.push(current);
            current = String::new();
            current_len = 0;
        }
        if !current.is_empty() {
            current.push('\n');
            current_len += 1;
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

/// Truncate a string to at most `limit` Unicode characters.
/// Discord's message limit counts Unicode characters, not bytes.
pub fn truncate_chars(s: &str, limit: usize) -> &str {
    match s.char_indices().nth(limit) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
