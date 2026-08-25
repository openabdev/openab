//! Turn Envelope — constrain a reply into the versioned envelope the broker's
//! structured delivery path parses (ADR: `docs/adr/structured-delivery.md`).
//!
//! This is the producing half of that ADR; `openab-core`'s `structured_delivery`
//! module is the consuming half. Together they let one turn become several chat
//! messages ("bubbles") instead of one wall of text.
//!
//! # Why a tool, not a prompt instruction
//!
//! Asking a model to "reply with JSON matching this schema" works most of the
//! time, and the times it does not are exactly the times it matters: a fenced
//! block, a stray sentence before the object, a trailing comma. The broker
//! handles all of those by falling back to plain text — but a fallback on every
//! other turn is not a feature.
//!
//! Declaring the envelope as a **tool's input schema** moves the guarantee into
//! the provider API: the model emits a `tool_use` block whose input is validated
//! against the schema before it ever reaches us. The agent then renders the
//! envelope itself. Nothing about JSON formatting is left to the model.
//!
//! It also costs no new machinery — [`ToolDef`] and `LlmEvent::ToolUse` already
//! exist for every provider this agent supports.
//!
//! # What the model is asked for vs what goes on the wire
//!
//! The tool takes the smallest thing a model can reliably produce: an array of
//! strings and one enum. Everything mechanical — the schema identifier, bubble
//! ids, the `next` object shape — is filled in by [`render`], so the model
//! cannot get it wrong.
//!
//! ```text
//! model produces           →  wire format
//! { "messages": ["on it",     { "schema": "openab.turn.v1",
//!                "found it"],   "messages": [
//!   "next": "stop" }               { "id": "bubble_1", "text": "on it" },
//!                                  { "id": "bubble_2", "text": "found it" }],
//!                                "next": { "type": "stop" } }
//! ```
//!
//! # Opt-in
//!
//! Disabled unless a schema is configured, in which case the tool is never
//! registered and the agent behaves exactly as it did before. See
//! [`configured_schema`].

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::llm::ToolDef;

/// The only envelope schema this agent produces. Follows the repo's identifier
/// convention (`openab.sender.v1`, `openab.gateway.reply.v1`).
pub const SCHEMA_V1: &str = "openab.turn.v1";

/// Tool name the model calls to speak. Deliberately plain: it appears in the
/// model's tool list beside `read` / `write` / `bash`.
pub const REPLY_TOOL: &str = "reply";

/// Default cap on bubbles per turn, matching the broker's `[delivery]
/// max_bubbles` default. A turn that wants more has stopped composing beats.
pub const DEFAULT_MAX_BUBBLES: usize = 4;

/// Which envelope schema this run produces, if any.
///
/// `None` — the default — means the reply tool is not registered and the agent
/// answers in plain text exactly as before.
///
/// Resolution is env-over-config, matching [`crate::config::AgentConfig`]:
/// `OPENAB_AGENT_TURN_ENVELOPE` wins over `config.json`'s `turn_envelope`, so a
/// pod's injected env stays authoritative over a baked image. An empty or
/// whitespace-only value counts as unset (an env var cleared to `""` disables
/// the feature rather than configuring an unnameable schema).
pub fn configured_schema() -> Option<String> {
    let raw = if let Ok(v) = std::env::var("OPENAB_AGENT_TURN_ENVELOPE") {
        let v = v.trim().to_string();
        (!v.is_empty()).then_some(v)
    } else {
        crate::config::AgentConfig::load_or_default()
            .turn_envelope
            .and_then(|v| {
                let v = v.trim().to_string();
                (!v.is_empty()).then_some(v)
            })
    }?;
    if raw != SCHEMA_V1 {
        // Still honoured — a later schema version should not need a new binary.
        // But a typo here is otherwise invisible: the agent would emit envelopes
        // the broker rejects, and every turn would silently fall back to plain
        // text with no clue why.
        tracing::warn!(
            configured = %raw,
            known = SCHEMA_V1,
            "turn envelope schema is not the one this build knows;              the broker will reject these turns unless its [delivery] schema matches"
        );
    }
    Some(raw)
}

/// Whether this run emits bubbles **as it decides on them** (sequential) rather
/// than planning them all into one envelope.
///
/// Only meaningful when [`configured_schema`] is set. In sequential mode the
/// `reply` tool stops being terminal: it delivers immediately and the model
/// keeps working, so a later bubble can reflect a tool result an earlier one
/// triggered. That costs one model call per bubble, which is why it is opt-in
/// on top of the envelope rather than the default.
///
/// The broker must be in `[delivery] mode = "sequential"` to match.
pub fn sequential_enabled() -> bool {
    matches!(
        std::env::var("OPENAB_AGENT_SEQUENTIAL_BUBBLES")
            .map(|v| v.trim().to_lowercase())
            .as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Bubble cap for this run. `OPENAB_AGENT_MAX_BUBBLES`, else
/// [`DEFAULT_MAX_BUBBLES`]. Keep it at or below the broker's `max_bubbles`:
/// the broker **rejects** an over-cap turn outright rather than truncating it,
/// so a mismatch turns good replies into plain-text fallbacks.
pub fn max_bubbles() -> usize {
    match std::env::var("OPENAB_AGENT_MAX_BUBBLES") {
        Ok(v) => match v.trim().parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!(
                    value = %v,
                    "OPENAB_AGENT_MAX_BUBBLES is not a positive integer; \
                     using {DEFAULT_MAX_BUBBLES}"
                );
                DEFAULT_MAX_BUBBLES
            }
        },
        Err(_) => DEFAULT_MAX_BUBBLES,
    }
}

/// The reply tool the model calls instead of answering in free text.
///
/// The two modes need different tools, not the same tool used differently:
///
/// - **envelope** — terminal. One call carries every bubble of the turn, plus
///   `next` for `stop` / `wait` / `silent`.
/// - **sequential** — not terminal. Each call delivers immediately and the model
///   keeps going, so there is nothing for `next` to say: the turn ends when the
///   model stops calling tools, and staying silent means never calling this one.
///   Offering `next` here would invite the model to declare an intention the
///   loop does not honour.
pub fn reply_tool_def(max_bubbles: usize, sequential: bool) -> ToolDef {
    let shared = format!(
        "Say something to the user. This is the ONLY way to speak to them — text \
         written outside this tool is not delivered. Each entry in `messages` is \
         sent as its own chat message, in order, so use more than one only for \
         deliberate conversational beats (\"on it\" → the answer). Keep multi-line \
         content in ONE entry: a line break is not a message boundary. At most \
         {max_bubbles} messages per turn."
    );
    let mut properties = json!({
        "messages": {
            "type": "array",
            "description": "Chat messages to send, in order. Usually one.",
            "items": { "type": "string" },
            "minItems": 0,
            "maxItems": max_bubbles
        }
    });
    let description = if sequential {
        format!(
            "{shared} These are delivered the moment you call this, so you can \
             send a short acknowledgement, run a tool, and then send what you \
             found. Call it again whenever you have something more to say. To \
             say nothing at all, simply never call it."
        )
    } else {
        properties["next"] = json!({
            "type": "string",
            "description":
                "stop: done. wait: done, expecting the user to reply. silent: \
                 send nothing at all (use when the right move is to say nothing — \
                 routine noise, or a message that does not need you).",
            "enum": ["stop", "wait", "silent"]
        });
        format!("{shared} This ends your turn.")
    };
    ToolDef {
        name: REPLY_TOOL.to_string(),
        description,
        input_schema: json!({
            "type": "object",
            "properties": properties,
            "required": ["messages"]
        }),
    }
}

/// Extract the bubble texts from a validated `reply` tool input.
///
/// Shared by both modes: the envelope path renders these into JSON, the
/// sequential path emits them one at a time. Blank entries are dropped rather
/// than rejected — losing a whole reply over a stray empty string is the worse
/// outcome.
pub fn bubbles_from_tool_input(input: &Value) -> Result<Vec<String>> {
    let messages = input
        .get("messages")
        .ok_or_else(|| anyhow!("reply tool input has no `messages`"))?
        .as_array()
        .ok_or_else(|| anyhow!("reply tool `messages` is not an array"))?;

    let mut out = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        let text = message
            .as_str()
            .ok_or_else(|| anyhow!("reply tool `messages[{index}]` is not a string"))?;
        if text.trim().is_empty() {
            tracing::warn!(index, "dropping empty reply message");
            continue;
        }
        out.push(text.to_string());
    }
    Ok(out)
}

/// The system-prompt section describing how to speak. Mirrors
/// `mcp::format_system_prompt_appendix` — appended only when the feature is on.
pub fn system_prompt_appendix(max_bubbles: usize) -> String {
    format!(
        "\n\n## Replying\n\n\
         Use the `{REPLY_TOOL}` tool for everything you say to the user. Text you \
         write outside a tool call is never delivered — it is thinking-out-loud \
         only, and the user does not see it.\n\n\
         - Default to a single message. Reach for a second only when the beat is \
         deliberate: an acknowledgement before a slow lookup, or a punchline that \
         lands better on its own.\n\
         - Never split one sentence across messages.\n\
         - Never split multi-line or copy-ready content (an address, a command, a \
         code block) just because it contains line breaks — that belongs in one \
         message.\n\
         - At most {max_bubbles} messages.\n\
         - Use `next: \"silent\"` with an empty `messages` when the right move is to \
         say nothing.\n\
         - Never claim an action succeeded before a tool result confirms it."
    )
}

/// Render a validated `reply` tool input into the wire envelope.
///
/// The schema identifier, bubble ids and the `next` object shape are all filled
/// in here, so the model cannot get them wrong. Returns the serialized JSON the
/// agent hands back as its turn output.
///
/// Errors when `input` is not an object or `messages` is not an array of
/// strings — a provider that honoured the tool schema cannot produce either, so
/// this guards against a provider that silently does not.
pub fn render(input: &Value, schema: &str, max_bubbles: usize) -> Result<String> {
    let texts = bubbles_from_tool_input(input)?;

    let next = input.get("next").and_then(|v| v.as_str()).unwrap_or("stop");
    let next = match next {
        "stop" | "wait" | "silent" => next,
        other => {
            // The schema constrains this to an enum; a provider that ignores it
            // should not cost the user their reply.
            tracing::warn!(
                value = other,
                "reply tool returned an unknown `next`; treating as stop"
            );
            "stop"
        }
    };

    let bubbles: Vec<Value> = texts
        .iter()
        .enumerate()
        .map(|(i, text)| json!({ "id": format!("bubble_{}", i + 1), "text": text }))
        .collect();

    if bubbles.len() > max_bubbles {
        // Truncating would silently drop what the model meant to say, and the
        // broker rejects an over-cap envelope anyway. Fail loudly instead.
        return Err(anyhow!(
            "reply tool returned {} messages, over the {max_bubbles} cap",
            bubbles.len()
        ));
    }

    // An empty `messages` only makes sense alongside `silent`. Anything else is
    // a turn that would deliver nothing without saying so.
    if bubbles.is_empty() && next != "silent" {
        return Err(anyhow!(
            "reply tool returned no messages and next != silent"
        ));
    }

    Ok(json!({
        "schema": schema,
        "messages": bubbles,
        "next": { "type": next },
    })
    .to_string())
}

/// Wrap a plain-text answer into a single-bubble envelope.
///
/// Used when the envelope is enabled but the model finished a turn without
/// calling [`REPLY_TOOL`]. The broker would fall back to plain text on its own,
/// with the same visible result — but an agent that declared itself to be in
/// envelope mode should keep producing envelopes, so the broker's fallback path
/// stays reserved for genuine faults.
///
/// Returns `None` for empty text: there is nothing to wrap, and an envelope with
/// no bubbles and `next: stop` is invalid by design.
pub fn wrap_plain_text(text: &str, schema: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(
        json!({
            "schema": schema,
            "messages": [{ "id": "bubble_1", "text": text }],
            "next": { "type": "stop" },
        })
        .to_string(),
    )
}

/// Where a bubble goes the moment the agent decides on it (sequential mode).
///
/// Implemented by the ACP layer, which owns the session id and the single
/// stdout writer. Kept as a trait so the agent loop can be tested without one.
pub trait BubbleSink: Send + Sync {
    /// Deliver one bubble now. `id` correlates the delivery in logs; it carries
    /// no meaning for the user.
    ///
    /// An `Err` means the host is gone — the caller stops emitting rather than
    /// finishing a reply nobody will receive.
    fn emit(&self, id: &str, text: &str) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(rendered: &str) -> Value {
        serde_json::from_str(rendered).unwrap()
    }

    #[test]
    fn render_fills_in_schema_ids_and_next_shape() {
        let out = render(
            &json!({ "messages": ["on it", "found it"], "next": "stop" }),
            SCHEMA_V1,
            4,
        )
        .unwrap();
        assert_eq!(
            parse(&out),
            json!({
                "schema": "openab.turn.v1",
                "messages": [
                    { "id": "bubble_1", "text": "on it" },
                    { "id": "bubble_2", "text": "found it" }
                ],
                "next": { "type": "stop" }
            })
        );
    }

    #[test]
    fn render_defaults_next_to_stop() {
        let out = render(&json!({ "messages": ["hey"] }), SCHEMA_V1, 4).unwrap();
        assert_eq!(parse(&out)["next"], json!({ "type": "stop" }));
    }

    #[test]
    fn render_keeps_newlines_inside_one_bubble() {
        // The load-bearing property of the whole feature.
        let out = render(&json!({ "messages": ["alpha\nbeta\ngamma"] }), SCHEMA_V1, 4).unwrap();
        let v = parse(&out);
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
        assert_eq!(v["messages"][0]["text"], "alpha\nbeta\ngamma");
    }

    #[test]
    fn render_allows_silent_with_no_messages() {
        let out = render(&json!({ "messages": [], "next": "silent" }), SCHEMA_V1, 4).unwrap();
        let v = parse(&out);
        assert_eq!(v["next"], json!({ "type": "silent" }));
        assert!(v["messages"].as_array().unwrap().is_empty());
    }

    #[test]
    fn render_rejects_an_empty_turn_that_is_not_silent() {
        assert!(render(&json!({ "messages": [], "next": "stop" }), SCHEMA_V1, 4).is_err());
    }

    #[test]
    fn render_drops_blank_messages_and_renumbers() {
        let out = render(
            &json!({ "messages": ["first", "   ", "second"] }),
            SCHEMA_V1,
            4,
        )
        .unwrap();
        let v = parse(&out);
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        // Ids stay contiguous — the broker requires them unique, and a gap
        // would advertise a message that was never sent.
        assert_eq!(msgs[0]["id"], "bubble_1");
        assert_eq!(msgs[1]["id"], "bubble_2");
        assert_eq!(msgs[1]["text"], "second");
    }

    #[test]
    fn render_rejects_over_cap_rather_than_truncating() {
        let err = render(
            &json!({ "messages": ["a", "b", "c", "d", "e"] }),
            SCHEMA_V1,
            4,
        )
        .unwrap_err();
        assert!(err.to_string().contains("over the 4 cap"), "got: {err}");
    }

    #[test]
    fn render_coerces_an_unknown_next_to_stop() {
        // Schema says enum; a provider that ignores it must not cost the reply.
        let out = render(
            &json!({ "messages": ["hey"], "next": "escalate" }),
            SCHEMA_V1,
            4,
        )
        .unwrap();
        assert_eq!(parse(&out)["next"], json!({ "type": "stop" }));
    }

    #[test]
    fn render_rejects_malformed_tool_input() {
        assert!(render(&json!({}), SCHEMA_V1, 4).is_err());
        assert!(render(&json!({ "messages": "hey" }), SCHEMA_V1, 4).is_err());
        assert!(render(&json!({ "messages": [42] }), SCHEMA_V1, 4).is_err());
    }

    #[test]
    fn render_output_satisfies_the_broker_contract() {
        // Cross-check against openab-core's parser expectations: exact schema
        // string, unique non-empty ids, non-empty text, tagged `next`.
        let out = render(
            &json!({ "messages": ["a", "b"], "next": "wait" }),
            SCHEMA_V1,
            4,
        )
        .unwrap();
        let v = parse(&out);
        assert_eq!(v["schema"], SCHEMA_V1);
        assert_eq!(v["next"]["type"], "wait");
        let ids: Vec<&str> = v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["bubble_1", "bubble_2"]);
        // No unknown top-level keys — the broker uses deny_unknown_fields.
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["messages", "next", "schema"]);
    }

    /// The shared contract fixture. `openab-agent` is its own workspace, so the
    /// broker's parser cannot be called from here — both sides pin the same
    /// bytes instead. If this test fails, the envelope format changed and
    /// `openab-core`'s `structured_delivery` tests must be checked too.
    const CONTRACT_FIXTURE: &str = include_str!("../../docs/fixtures/turn-envelope-v1.json");

    #[test]
    fn render_matches_the_shared_contract_fixture() {
        let out = render(
            &json!({
                "messages": ["on it", "your flight moved to 8pm\ngate B12"],
                "next": "stop"
            }),
            SCHEMA_V1,
            4,
        )
        .unwrap();
        let produced: Value = serde_json::from_str(&out).unwrap();
        let expected: Value = serde_json::from_str(CONTRACT_FIXTURE).unwrap();
        assert_eq!(
            produced, expected,
            "render() no longer produces the envelope the broker is tested against"
        );
    }

    #[test]
    fn wrap_plain_text_produces_a_single_bubble() {
        let out = wrap_plain_text("  just words  ", SCHEMA_V1).unwrap();
        let v = parse(&out);
        assert_eq!(v["messages"][0]["text"], "just words");
        assert_eq!(v["next"], json!({ "type": "stop" }));
    }

    #[test]
    fn wrap_plain_text_returns_none_for_empty() {
        assert!(wrap_plain_text("   \n ", SCHEMA_V1).is_none());
    }

    #[test]
    fn sequential_reply_tool_drops_the_next_field() {
        // In sequential mode the loop, not the model, decides when the turn
        // ends — so `next` would be an intention nothing honours.
        let def = reply_tool_def(4, true);
        assert!(def.input_schema["properties"]["next"].is_null());
        assert!(def.input_schema["properties"]["messages"].is_object());
        assert!(
            def.description.contains("delivered the moment"),
            "the description must tell the model these are sent immediately"
        );
    }

    #[test]
    fn envelope_reply_tool_keeps_the_next_field() {
        let def = reply_tool_def(4, false);
        assert_eq!(
            def.input_schema["properties"]["next"]["enum"],
            json!(["stop", "wait", "silent"])
        );
        assert!(def.description.contains("ends your turn"));
    }

    #[test]
    fn bubbles_from_tool_input_drops_blanks() {
        let texts = bubbles_from_tool_input(&json!({ "messages": ["one", "  ", "two"] })).unwrap();
        assert_eq!(texts, vec!["one", "two"]);
    }

    #[test]
    fn bubbles_from_tool_input_rejects_malformed() {
        assert!(bubbles_from_tool_input(&json!({})).is_err());
        assert!(bubbles_from_tool_input(&json!({ "messages": 42 })).is_err());
        assert!(bubbles_from_tool_input(&json!({ "messages": [1] })).is_err());
    }

    #[test]
    fn reply_tool_schema_caps_messages() {
        let def = reply_tool_def(3, false);
        assert_eq!(def.name, REPLY_TOOL);
        assert_eq!(def.input_schema["properties"]["messages"]["maxItems"], 3);
        assert_eq!(
            def.input_schema["properties"]["next"]["enum"],
            json!(["stop", "wait", "silent"])
        );
        assert_eq!(def.input_schema["required"], json!(["messages"]));
    }

    #[test]
    fn system_prompt_appendix_names_the_tool_and_the_cap() {
        let text = system_prompt_appendix(3);
        assert!(text.contains(REPLY_TOOL));
        assert!(text.contains("At most 3 messages"));
    }
}
