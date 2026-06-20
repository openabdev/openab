use serde_json::Value;

/// Extract text from a step_payload protobuf: top-level field 20 (sub-message) → field 1 (string).
pub fn extract_text_from_step_payload(blob: &[u8]) -> Option<String> {
    let field_20 = get_proto_field(blob, 20)?;
    let field_1 = get_proto_field(&field_20, 1)?;
    String::from_utf8(field_1).ok()
}

/// Extract the first length-delimited field with the given number from a protobuf blob.
pub fn get_proto_field(blob: &[u8], target: u64) -> Option<Vec<u8>> {
    let mut i = 0;
    while i < blob.len() {
        let (tag, consumed) = read_varint(&blob[i..])?;
        i += consumed;
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        match wire_type {
            0 => { let (_, c) = read_varint(&blob[i..])?; i += c; }
            2 => {
                let (len, c) = read_varint(&blob[i..])?;
                i += c;
                let len = len as usize;
                if i + len > blob.len() { return None; }
                if field_number == target {
                    return Some(blob[i..i + len].to_vec());
                }
                i += len;
            }
            5 => { i += 4; }
            1 => { i += 8; }
            _ => return None,
        }
    }
    None
}

/// Read a protobuf varint, returning (value, bytes_consumed).
pub fn read_varint(buf: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0;
    for (i, &byte) in buf.iter().enumerate() {
        if shift >= 70 {
            return None;
        }
        result |= ((byte & 0x7F) as u64) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
    }
    None
}

/// Get a text (UTF-8 string) from a protobuf field.
pub fn get_proto_text(blob: &[u8], target: u64) -> Option<String> {
    let bytes = get_proto_field(blob, target)?;
    String::from_utf8(bytes).ok()
}

/// Check if a step_type represents a tool call.
pub fn is_tool_step_type(step_type: i64) -> bool {
    matches!(step_type, 5 | 7 | 8 | 9 | 17 | 21 | 33 | 101 | 138)
}

/// Extract tool name and input from a tool step payload.
pub fn extract_tool_from_step_payload(blob: &[u8]) -> Option<(String, Option<Value>)> {
    let tool = get_proto_field(blob, 5)?;
    let call = get_proto_field(&tool, 4)?;
    let name = get_proto_text(&call, 2)
        .or_else(|| get_proto_text(&call, 9))
        .filter(|n| !n.is_empty())?;
    let input = get_proto_text(&call, 3)
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    Some((name, input))
}

/// Extract a human-readable preview of a tool's output/result from the step payload.
///
/// agy stores the tool *call* (name + input + metadata) under field 5, and the
/// tool *result* under type-specific top-level render fields (e.g. field 14 for
/// `view_file` → file content, field 15 for `list_dir` → directory entries).
/// Rather than hardcode every tool's schema, we collect readable string leaves
/// from all top-level length-delimited fields *except* the call wrapper (field 5),
/// which yields a usable preview for any tool type.
pub fn extract_tool_output_text(blob: &[u8]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    collect_output_strings(blob, true, &mut parts);
    if parts.is_empty() {
        return None;
    }
    let mut text = parts.join("\n");
    const MAX: usize = 6000;
    if text.len() > MAX {
        let mut end = MAX;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n… (kesildi)");
    }
    Some(text)
}

/// Recursively collect readable UTF-8 string leaves from a protobuf blob.
/// At the top level, the tool-call wrapper (field 5) is skipped — that is the
/// input side, surfaced separately via `rawInput`.
fn collect_output_strings(blob: &[u8], top_level: bool, out: &mut Vec<String>) {
    let mut i = 0;
    while i < blob.len() {
        let Some((tag, c)) = read_varint(&blob[i..]) else { break; };
        i += c;
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        match wire_type {
            0 => {
                let Some((_, c)) = read_varint(&blob[i..]) else { break; };
                i += c;
            }
            2 => {
                let Some((len, c)) = read_varint(&blob[i..]) else { break; };
                i += c;
                let len = len as usize;
                if i + len > blob.len() {
                    break;
                }
                let chunk = &blob[i..i + len];
                i += len;
                if top_level && field_number == 5 {
                    continue;
                }
                if let Ok(s) = std::str::from_utf8(chunk) {
                    let total = s.chars().count();
                    let printable = s
                        .chars()
                        .filter(|ch| ch.is_alphanumeric() || ch.is_ascii_graphic() || matches!(ch, ' ' | '\n' | '\t' | '\r'))
                        .count();
                    if total > 0 && printable as f64 >= total as f64 * 0.85 {
                        let t = s.trim();
                        if !t.is_empty() {
                            out.push(t.to_string());
                        }
                        continue;
                    }
                }
                collect_output_strings(chunk, false, out);
            }
            5 => i += 4,
            1 => i += 8,
            _ => break,
        }
    }
}

/// Derive a short title for a tool call based on name and input.
pub fn tool_call_title(name: &str, input: &Option<Value>) -> String {
    if let Some(input) = input {
        for key in ["path", "file", "AbsolutePath", "FilePath", "TargetFile"] {
            if let Some(path) = input.get(key).and_then(|v| v.as_str()) {
                return format!("{}: {}", name, path);
            }
        }
        for key in ["query", "command", "text", "CommandLine", "Target"] {
            if let Some(val) = input.get(key).and_then(|v| v.as_str()) {
                let truncated: String = val.chars().take(60).collect();
                return format!("{}: {}", name, truncated);
            }
        }
    }
    name.to_string()
}
