use rusqlite::Connection;
use std::path::PathBuf;

use crate::protobuf::extract_text_from_step_payload;

/// Check if narration should be shown.
pub fn show_narration() -> bool {
    if let Ok(v) = std::env::var("OPENAB_SHOW_NARRATION") {
        return v == "1" || v.to_lowercase() == "true";
    }
    if let Ok(v) = std::env::var("OPENAB_TOOL_DISPLAY") {
        return v.to_lowercase() == "full";
    }
    false
}

/// Check if tool call output/result should be attached to tool_call updates.
///
/// Enabled by `AGY_SHOW_TOOL_OUTPUT=1` (or `true`), or by the shared
/// `OPENAB_TOOL_DISPLAY=full` switch. When off, only the tool title + rawInput
/// are sent (current default behaviour).
pub fn show_tool_output() -> bool {
    if let Ok(v) = std::env::var("AGY_SHOW_TOOL_OUTPUT") {
        return v == "1" || v.eq_ignore_ascii_case("true");
    }
    if let Ok(v) = std::env::var("OPENAB_TOOL_DISPLAY") {
        return v.eq_ignore_ascii_case("full");
    }
    false
}


/// A part is considered narration if every non-empty line starts with "I will".
pub fn is_narration(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return false;
    }
    lines.iter().all(|l| l.trim_start().starts_with("I will"))
}

/// Filter out leading narration from response parts.
#[allow(dead_code)]
pub fn filter_narration(parts: &[String]) -> String {
    if show_narration() || parts.len() <= 1 {
        return parts.join("\n");
    }
    let first_content = parts.iter().position(|p| !is_narration(p)).unwrap_or(parts.len() - 1);
    parts[first_content..].join("\n")
}

/// Read the latest response from the SQLite conversation DB.
/// Returns (response_text, max_step_idx) or None if reading fails.
#[allow(dead_code)]
pub fn read_response_from_db(conversations_dir: &PathBuf, conversation_id: &str, after_step_idx: i64) -> Option<(String, i64)> {
    let db_path = conversations_dir.join(format!("{}.db", conversation_id));
    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).ok()?;

    let table_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='steps'",
        [],
        |row| row.get(0),
    ).unwrap_or(false);
    if !table_exists {
        eprintln!("[agy-acp] WARN: steps table not found in {}.db — schema changed?", conversation_id);
        return None;
    }

    let mut stmt = conn.prepare(
        "SELECT idx, step_payload FROM steps WHERE idx > ?1 AND step_type = 15 ORDER BY idx"
    ).ok()?;
    let rows: Vec<(i64, Vec<u8>)> = stmt.query_map([after_step_idx], |row| {
        Ok((row.get(0)?, row.get(1)?))
    }).ok()?.filter_map(|r| r.ok()).collect();

    let mut max_idx = after_step_idx;
    let mut response_parts: Vec<String> = Vec::new();
    for (idx, payload) in &rows {
        max_idx = max_idx.max(*idx);
        if let Some(text) = extract_text_from_step_payload(payload) {
            if !text.is_empty() {
                response_parts.push(text);
            }
        }
    }
    if response_parts.is_empty() {
        if !rows.is_empty() {
            let payload_sizes: Vec<usize> = rows.iter().map(|(_, p)| p.len()).collect();
            eprintln!(
                "[agy-acp] WARN: {} new steps found (payload sizes: {:?}) but none had extractable text \
                 (field 20.1 missing — schema change?)",
                rows.len(), payload_sizes
            );
        }
        return None;
    }
    let filtered = filter_narration(&response_parts);
    Some((filtered, max_idx))
}
