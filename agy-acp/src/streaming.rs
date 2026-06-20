use rusqlite::Connection;
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::db::{is_narration, show_narration, show_tool_output};
use crate::protobuf::{extract_text_from_step_payload, extract_tool_from_step_payload, extract_tool_output_text, is_tool_step_type, tool_call_title};
use crate::types::{JsonRpcNotification, StreamingState};

/// Poll the SQLite DB for new text since `base_step_idx` and emit streaming deltas.
/// Returns notification JSON lines to send through the output channel.
pub fn poll_streaming_delta(
    conversations_dir: &PathBuf,
    snapshot: Option<&HashSet<String>>,
    session_id: &str,
    state: &Arc<Mutex<StreamingState>>,
) -> Vec<String> {
    // Try to bind conversation_id if not yet bound
    {
        let mut guard = state.lock().unwrap();
        if guard.conversation_id.is_none() {
            if let Some(before) = snapshot {
                let after: HashSet<String> = fs::read_dir(conversations_dir)
                    .ok()
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .filter_map(|e| {
                                let path = e.path();
                                if path.extension().map(|x| x == "db").unwrap_or(false) {
                                    path.file_stem().map(|s| s.to_string_lossy().to_string())
                                } else {
                                    None
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let mut created: Vec<_> = after.difference(before).collect();
                if created.len() == 1 {
                    guard.conversation_id = Some(created.remove(0).clone());
                }
            }
        }
    }

    let (conversation_id, base_step_idx) = {
        let guard = state.lock().unwrap();
        (guard.conversation_id.clone(), guard.base_step_idx)
    };

    let Some(conversation_id) = conversation_id else {
        return Vec::new();
    };

    let db_path = conversations_dir.join(format!("{}.db", conversation_id));
    let conn = match Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut stmt = match conn.prepare(
        "SELECT idx, step_type, step_payload FROM steps WHERE idx > ?1 AND (step_type = 15 OR step_type IN (5,7,8,9,17,21,33,101,138)) ORDER BY idx"
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let rows: Vec<(i64, i64, Vec<u8>)> = stmt
        .query_map([base_step_idx], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .ok()
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    let mut guard = state.lock().unwrap();
    let mut notifications = Vec::new();

    for (idx, step_type, payload) in rows {
        guard.last_step_idx = guard.last_step_idx.max(idx);

        if step_type == 15 {
            let Some(text) = extract_text_from_step_payload(&payload) else {
                continue;
            };

            let emitted = guard.emitted_len.get(&idx).copied().unwrap_or(0);
            if text.len() <= emitted {
                continue;
            }

            if !show_narration() && is_narration(&text) {
                guard.emitted_len.insert(idx, text.len());
                continue;
            }

            let new_text = &text[emitted..];
            guard.emitted_len.insert(idx, text.len());

            if !new_text.is_empty() {
                notifications.push(
                    serde_json::to_string(&JsonRpcNotification {
                        jsonrpc: "2.0",
                        method: "session/update".to_string(),
                        params: json!({
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": { "type": "text", "text": new_text },
                            },
                        }),
                    })
                    .unwrap(),
                );
            }
        } else if is_tool_step_type(step_type) && !guard.emitted_tool_steps.contains(&idx) {
            if let Some((name, input)) = extract_tool_from_step_payload(&payload) {
                guard.emitted_tool_steps.insert(idx);
                let title = tool_call_title(&name, &input);
                let tool_call_id = format!("agy-{}-{}", idx, step_type);

                let mut start_update = json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": tool_call_id,
                    "title": title,
                });
                if let Some(input) = &input {
                    start_update["rawInput"] = input.clone();
                }
                notifications.push(
                    serde_json::to_string(&JsonRpcNotification {
                        jsonrpc: "2.0",
                        method: "session/update".to_string(),
                        params: json!({
                            "sessionId": session_id,
                            "update": start_update,
                        }),
                    })
                    .unwrap(),
                );

                let mut completed = json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": tool_call_id,
                    "title": title,
                    "status": "completed",
                });
                if show_tool_output() {
                    if let Some(output) = extract_tool_output_text(&payload) {
                        completed["content"] = json!([
                            { "type": "content", "content": { "type": "text", "text": output } }
                        ]);
                        completed["rawOutput"] = json!({ "text": output });
                    }
                }
                notifications.push(
                    serde_json::to_string(&JsonRpcNotification {
                        jsonrpc: "2.0",
                        method: "session/update".to_string(),
                        params: json!({
                            "sessionId": session_id,
                            "update": completed,
                        }),
                    })
                    .unwrap(),
                );
            }
        }
    }

    guard.had_updates = guard.had_updates || !notifications.is_empty();
    notifications
}
