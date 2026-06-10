//! MCP sampling (server-initiated LLM calls) — text-only baseline.
//!
//! When a server sends `sampling/createMessage`, it is borrowing the client's
//! model. We route that request back to our own [`crate::llm::LlmProvider`]
//! (the user's already-authenticated Anthropic / codex provider) and return
//! the completion (spec §390, rows 385-413).
//!
//! Scope is deliberately the text-only baseline: tool-enabled sampling
//! (`sampling.tools` — `tools[]`/`toolChoice`/`stopReason:"toolUse"`), the
//! interactive human-in-the-loop consent UI (rows 403/404/406), and per-server
//! rate limiting (row 409) are tracked as known gaps. The non-interactive
//! approval gate below is the headless-friendly stand-in for the consent UI.

use rmcp::model::{CreateMessageResult, ErrorCode, ErrorData, Role, SamplingMessage};

use crate::llm::{ContentBlock, LlmEvent, Message};

/// Approval policy for inbound sampling requests, read from
/// `OPENAB_AGENT_SAMPLING_APPROVAL` (default `ask`).
///
/// There is no interactive consent surface in headless mode (that is the HITL
/// known gap), so the policy fails closed: only `allow` actually serves a
/// request; `ask` (the default) and `deny` both reject. This realizes the
/// locked sampling decision's env-var approval without an interactive prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingApproval {
    Allow,
    Deny,
    Ask,
}

impl SamplingApproval {
    pub fn from_env() -> Self {
        Self::parse(
            std::env::var("OPENAB_AGENT_SAMPLING_APPROVAL")
                .ok()
                .as_deref(),
        )
    }

    fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("allow") => Self::Allow,
            Some("deny") => Self::Deny,
            // unset / empty / "ask" / anything unrecognized → fail closed
            _ => Self::Ask,
        }
    }
}

/// JSON-RPC error code the spec assigns to a user-rejected sampling request
/// (row 405: `-1` user rejected).
const USER_REJECTED: i32 = -1;

fn rejected(message: &'static str) -> ErrorData {
    ErrorData::new(ErrorCode(USER_REJECTED), message, None)
}

/// Gate an inbound request on the approval policy. Only [`SamplingApproval::Allow`]
/// proceeds; the others return the rejection error to send back to the server.
pub fn approval_gate(policy: SamplingApproval) -> Result<(), ErrorData> {
    match policy {
        SamplingApproval::Allow => Ok(()),
        SamplingApproval::Deny => {
            Err(rejected("sampling denied by OPENAB_AGENT_SAMPLING_APPROVAL=deny"))
        }
        SamplingApproval::Ask => Err(rejected(
            "sampling requires OPENAB_AGENT_SAMPLING_APPROVAL=allow (no interactive consent surface in headless mode)",
        )),
    }
}

/// Convert inbound sampling messages into our provider's [`Message`] type.
///
/// Text-only: a message carrying any non-text content block (image / audio /
/// tool-use / tool-result) is rejected with `-32602`, because the text-only
/// baseline cannot faithfully relay those — they ship with the `sampling.tools`
/// extension (known gap). A message's multiple text blocks are joined with `\n`
/// so block boundaries survive (#969 F7) — bare concatenation would fuse the
/// last word of one block to the first of the next. (Contrast `collect_text`,
/// which intentionally concatenates without a separator: those are streaming
/// token fragments of one block, not separate blocks.)
pub fn convert_messages(messages: &[SamplingMessage]) -> Result<Vec<Message>, ErrorData> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let mut parts: Vec<&str> = Vec::new();
        for block in m.content.iter() {
            match block.as_text() {
                Some(t) => parts.push(t.text.as_str()),
                None => {
                    return Err(ErrorData::invalid_params(
                        "text-only sampling: non-text content blocks are not supported",
                        None,
                    ))
                }
            }
        }
        out.push(Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: parts.join("\n"),
            }],
        });
    }
    Ok(out)
}

/// Fold the provider's event stream into a single assistant text response.
/// Errors (`-32603`) if the provider surfaced an [`LlmEvent::Error`]. `Stop`
/// and any stray `ToolUse` events are ignored — we pass no tools, so a
/// well-behaved provider emits none.
pub fn collect_text(events: Vec<LlmEvent>) -> Result<String, ErrorData> {
    let mut text = String::new();
    for ev in events {
        match ev {
            LlmEvent::Text(t) => text.push_str(&t),
            LlmEvent::Error(e) => {
                return Err(ErrorData::internal_error(
                    format!("sampling provider error: {e}"),
                    None,
                ))
            }
            LlmEvent::ToolUse { .. } | LlmEvent::Stop => {}
        }
    }
    Ok(text)
}

/// Build a text-only [`CreateMessageResult`]: role `assistant`, stop reason
/// `endTurn`, tagged with the model that produced it.
pub fn build_result(text: String, model: &str) -> CreateMessageResult {
    CreateMessageResult::new(SamplingMessage::assistant_text(text), model.to_string())
        .with_stop_reason(CreateMessageResult::STOP_REASON_END_TURN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::SamplingMessageContent;

    #[test]
    fn approval_parse_defaults_closed() {
        assert_eq!(
            SamplingApproval::parse(Some("allow")),
            SamplingApproval::Allow
        );
        assert_eq!(
            SamplingApproval::parse(Some("ALLOW")),
            SamplingApproval::Allow
        );
        assert_eq!(
            SamplingApproval::parse(Some(" deny ")),
            SamplingApproval::Deny
        );
        assert_eq!(SamplingApproval::parse(Some("ask")), SamplingApproval::Ask);
        assert_eq!(SamplingApproval::parse(None), SamplingApproval::Ask);
        assert_eq!(SamplingApproval::parse(Some("")), SamplingApproval::Ask);
        assert_eq!(
            SamplingApproval::parse(Some("bogus")),
            SamplingApproval::Ask
        );
    }

    #[test]
    fn approval_gate_only_allows_allow() {
        assert!(approval_gate(SamplingApproval::Allow).is_ok());
        assert!(approval_gate(SamplingApproval::Deny).is_err());
        assert!(approval_gate(SamplingApproval::Ask).is_err());
    }

    #[test]
    fn convert_messages_maps_roles_and_text() {
        let msgs = vec![
            SamplingMessage::user_text("hello"),
            SamplingMessage::assistant_text("hi there"),
        ];
        let out = convert_messages(&msgs).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].role, "user");
        assert_eq!(out[1].role, "assistant");
        match &out[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn convert_messages_joins_multiple_text_blocks_with_newline() {
        // Two text blocks in one message must keep their boundary (#969 F7):
        // bare concatenation would yield "firstsecond".
        let msg = SamplingMessage::new_multiple(
            Role::User,
            vec![
                SamplingMessageContent::text("first"),
                SamplingMessageContent::text("second"),
            ],
        );
        let out = convert_messages(&[msg]).unwrap();
        assert_eq!(out.len(), 1);
        match &out[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "first\nsecond"),
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn convert_messages_rejects_non_text() {
        // A tool-result block is not text → text-only baseline rejects it.
        let msgs = vec![SamplingMessage::user_tool_result("call-1", vec![])];
        let err = convert_messages(&msgs).unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn collect_text_concatenates_and_ignores_control_events() {
        let events = vec![
            LlmEvent::Text("foo ".into()),
            LlmEvent::Text("bar".into()),
            LlmEvent::Stop,
        ];
        assert_eq!(collect_text(events).unwrap(), "foo bar");
    }

    #[test]
    fn collect_text_surfaces_provider_error() {
        let events = vec![
            LlmEvent::Text("partial".into()),
            LlmEvent::Error("boom".into()),
        ];
        let err = collect_text(events).unwrap_err();
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn build_result_is_assistant_text_end_turn() {
        let r = build_result("answer".into(), "stub-model");
        assert_eq!(r.model, "stub-model");
        assert_eq!(
            r.stop_reason.as_deref(),
            Some(CreateMessageResult::STOP_REASON_END_TURN)
        );
        assert_eq!(r.message.role, Role::Assistant);
    }
}
