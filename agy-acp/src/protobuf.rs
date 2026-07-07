use serde_json::Value;

/// Extract text from a step_payload protobuf: top-level field 20 (sub-message) → field 1 (string).
pub fn extract_text_from_step_payload(blob: &[u8]) -> Option<String> {
    let field_20 = get_proto_field(blob, 20)?;
    let field_1 = get_proto_field(&field_20, 1)?;
    if let Some(field_1_sub) = get_proto_field(&field_1, 1) {
        if let Ok(text) = String::from_utf8(field_1_sub) {
            return Some(text);
        }
    }
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

/// Derive a short title for a tool call based on name and input.
pub fn tool_call_title(name: &str, input: &Option<Value>) -> String {
    if let Some(input) = input {
        for key in ["path", "file", "AbsolutePath", "FilePath"] {
            if let Some(path) = input.get(key).and_then(|v| v.as_str()) {
                return format!("{}: {}", name, path);
            }
        }
        for key in ["query", "command", "text"] {
            if let Some(val) = input.get(key).and_then(|v| v.as_str()) {
                let truncated: String = val.chars().take(60).collect();
                return format!("{}: {}", name, truncated);
            }
        }
    }
    name.to_string()
}
