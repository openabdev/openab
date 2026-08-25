//! Structured Delivery — parse a versioned JSON turn envelope into an ordered
//! list of chat bubbles (ADR: structured-delivery.md).
//!
//! Phase 0 (this module) is **purely additive**: it defines the wire schema, the
//! pure parser, and the resulting [`DeliveryPlan`]. It is NOT yet wired into
//! `AdapterRouter::stream_prompt_blocks` and does not change any runtime
//! behavior. Wiring (the `[delivery]` config switch, forced non-streaming, and
//! the per-bubble send loop) lands in Phase 1.
//!
//! # Why an envelope instead of a text marker
//!
//! The existing delivery path concatenates every `agent_message_chunk` of a turn
//! into one string and splits it only at the platform's length limit
//! ([`crate::format::split_message`]) — a split that carries no semantic bubble
//! information. An agent that wants deliberate conversational beats ("on it" →
//! "found it, your flight moved to 8pm") has no way to express them, and a
//! newline is NOT a bubble boundary (it must stay inside one message).
//!
//! A versioned JSON envelope makes bubble boundaries explicit, leaves room for a
//! turn-level `next` action, and fails closed: anything that does not parse is
//! reported as an error rather than leaking raw JSON to the user.
//!
//! # Wire format (`openab.turn.v1`)
//!
//! ```json
//! {
//!   "schema": "openab.turn.v1",
//!   "messages": [
//!     { "id": "bubble_1", "text": "bro perth is behind sydney" },
//!     { "id": "bubble_2", "text": "i'm not lying to make you feel smart" }
//!   ],
//!   "next": { "type": "stop" }
//! }
//! ```
//!
//! `next.type` is one of `stop` (end the turn), `wait` (end the turn, expect the
//! user to reply), `silent` (send nothing), or `tool` (Phase 2 — the harness,
//! never the model, decides whether the call is allowed).
//!
//! # Never leak the envelope
//!
//! Raw JSON, a truncated envelope, and the directive header must never reach the
//! user. Callers that fall back to plain text on a parse error must send
//! [`strip_envelope`]'s output, not the original buffer — see
//! [`StructuredError::found_envelope`] for which errors require that.

use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fmt;

/// The only schema identifier this module accepts by default.
pub const SCHEMA_V1: &str = "openab.turn.v1";

/// Default cap on bubbles per turn. More than this is a sign the model stopped
/// composing beats and started streaming — reject rather than truncate.
pub const DEFAULT_MAX_BUBBLES: usize = 4;

/// Default per-bubble character cap (Unicode chars, matching `split_message`).
/// A "bubble" this long is a paragraph, not a beat. The platform's own hard
/// limit is separate and still applied by the adapter at send time.
pub const DEFAULT_MAX_BUBBLE_CHARS: usize = 1200;

// --- Config enums (deserialized from `[delivery]`, mirroring TableMode) ---

/// How a turn's reply is delivered to the platform.
///
/// - `text`: the existing path — one buffer, split only at the platform limit
///   (default; existing deployments are unaffected).
/// - `structured`: the agent plans every bubble up front in a [`SCHEMA_V1`]
///   envelope; the broker parses it and sends one message per bubble.
/// - `sequential`: the agent emits each bubble as it decides on it
///   (`AcpEvent::Message`), and the broker sends each one on arrival. Costs one
///   model call per bubble, and buys the thing `structured` cannot do — a later
///   bubble that reflects a tool result the earlier one triggered.
///
/// `structured` stays the recommended default of the two: `sequential` is the
/// configurable experiment (ADR: structured-delivery.md §5.2 / Phase 4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DeliveryMode {
    #[default]
    Text,
    Structured,
    Sequential,
}

impl DeliveryMode {
    /// Whether this mode must suppress token streaming and the tool-summary
    /// prefix. True for everything but plain text: in both bubble modes the
    /// user must not see partial output before the broker decides what to send.
    pub fn is_bubbles(self) -> bool {
        matches!(self, Self::Structured | Self::Sequential)
    }
}

impl<'de> Deserialize<'de> for DeliveryMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "text" | "plain" | "off" => Ok(Self::Text),
            "structured" | "bubbles" => Ok(Self::Structured),
            "sequential" => Ok(Self::Sequential),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["text", "structured", "sequential"],
            )),
        }
    }
}

impl fmt::Display for DeliveryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text => write!(f, "text"),
            Self::Structured => write!(f, "structured"),
            Self::Sequential => write!(f, "sequential"),
        }
    }
}

/// What to do when structured mode is on but the turn does not parse.
///
/// Every policy is safe with respect to the envelope: none of them can send raw
/// or truncated JSON (see [`strip_envelope`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ParseErrorPolicy {
    /// Deliver the turn's text with any envelope fragment stripped, through the
    /// normal single-message path. The common failure is a model that simply
    /// forgot the envelope and answered in prose — the user still gets a reply.
    #[default]
    FallbackText,
    /// Deliver a fixed apology line instead of the turn's text.
    ErrorMessage,
    /// Deliver nothing. Logged, but invisible to the user.
    Silent,
}

impl<'de> Deserialize<'de> for ParseErrorPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "fallback_text" | "fallback" | "text" => Ok(Self::FallbackText),
            "error_message" | "error" => Ok(Self::ErrorMessage),
            "silent" | "drop" => Ok(Self::Silent),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["fallback_text", "error_message", "silent"],
            )),
        }
    }
}

impl fmt::Display for ParseErrorPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FallbackText => write!(f, "fallback_text"),
            Self::ErrorMessage => write!(f, "error_message"),
            Self::Silent => write!(f, "silent"),
        }
    }
}

// --- Wire types (private: the public surface is DeliveryPlan) ---

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnEnvelopeV1 {
    schema: String,
    #[serde(default)]
    messages: Vec<BubbleV1>,
    /// Omitted `next` is treated as `stop` — a missing turn-level action should
    /// not cost the user their reply.
    #[serde(default)]
    next: NextV1,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BubbleV1 {
    id: String,
    text: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum NextV1 {
    #[default]
    Stop,
    Wait,
    Silent,
    Tool {
        name: String,
        arguments: Value,
    },
}

// --- Public output ---

/// What the turn wants to do after its bubbles are delivered.
///
/// `#[non_exhaustive]` because Phase 2 may add variants (e.g. a scheduled
/// follow-up); callers must include a `_` arm.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum NextAction {
    /// End the turn.
    Stop,
    /// End the turn and wait for the user's next message. Identical to `Stop`
    /// in OpenAB (a turn always ends at `session/prompt`'s response) — kept
    /// distinct so the agent's intent survives into logs and Phase 4.
    Wait,
    /// Send nothing at all.
    Silent,
    /// The agent proposes a tool call. **A proposal only** — the harness decides
    /// whether it runs. Phase 1 records it and does nothing else.
    Tool { name: String, arguments: Value },
}

/// A validated, ready-to-send turn: bubbles in delivery order plus the
/// turn-level action.
///
/// `bubbles` is exactly what the user should see. On [`NextAction::Silent`] it
/// is always empty, so the delivery loop needs no special case.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryPlan {
    pub bubbles: Vec<String>,
    pub next: NextAction,
}

impl DeliveryPlan {
    /// Whether this plan sends nothing.
    pub fn is_silent(&self) -> bool {
        self.bubbles.is_empty()
    }
}

// --- Errors ---

/// Why a turn could not be delivered as bubbles.
///
/// The variants deliberately carry no user text and no raw JSON: they are
/// logged, never sent. What the user sees on failure is decided by
/// [`ParseErrorPolicy`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StructuredError {
    /// No envelope found in the turn at all — the model answered in prose.
    /// The only error where the turn's own text is safe to deliver verbatim.
    NotStructured,
    /// An envelope started but never closed (the turn was cut short).
    Truncated,
    /// An envelope was found but did not deserialize. Carries serde's message
    /// (field names and offsets only, never document content).
    Malformed(String),
    /// `schema` is not the identifier this deployment expects.
    SchemaMismatch { found: String },
    /// More bubbles than `max_bubbles`. Rejected rather than truncated —
    /// silently dropping the tail would read as a complete answer.
    TooManyBubbles { found: usize, max: usize },
    /// A bubble's `text` is empty or whitespace-only.
    EmptyBubble { index: usize },
    /// A bubble's `id` is empty or whitespace-only.
    BlankBubbleId { index: usize },
    /// Two bubbles share an `id` within one turn.
    DuplicateBubbleId { index: usize },
    /// A bubble exceeds `max_bubble_chars`.
    BubbleTooLong {
        index: usize,
        chars: usize,
        max: usize,
    },
    /// A well-formed envelope that would send nothing without saying `silent`.
    EmptyTurn,
}

impl StructuredError {
    /// Whether an envelope (whole or truncated) was recognised in the turn text.
    ///
    /// **This is the leak guard.** When it returns `true` the raw buffer
    /// contains JSON and must NOT be sent as-is — a `fallback_text` policy has
    /// to route through [`strip_envelope`] first. `false` means the turn was
    /// plain prose and can be delivered unchanged.
    pub fn found_envelope(&self) -> bool {
        !matches!(self, Self::NotStructured)
    }
}

impl fmt::Display for StructuredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotStructured => write!(f, "no {SCHEMA_V1} envelope in turn output"),
            Self::Truncated => write!(f, "envelope was truncated (unclosed object)"),
            Self::Malformed(msg) => write!(f, "envelope did not deserialize: {msg}"),
            Self::SchemaMismatch { found } => {
                write!(f, "unexpected schema `{found}`")
            }
            Self::TooManyBubbles { found, max } => {
                write!(f, "{found} bubbles exceeds max_bubbles={max}")
            }
            Self::EmptyBubble { index } => write!(f, "bubble {index} has empty text"),
            Self::BlankBubbleId { index } => write!(f, "bubble {index} has a blank id"),
            Self::DuplicateBubbleId { index } => write!(f, "bubble {index} reuses an id"),
            Self::BubbleTooLong { index, chars, max } => {
                write!(f, "bubble {index} is {chars} chars, max_bubble_chars={max}")
            }
            Self::EmptyTurn => write!(f, "envelope has no bubbles and next.type != silent"),
        }
    }
}

impl std::error::Error for StructuredError {}

// --- Parsing ---

/// Parse a turn's delivered text into an ordered [`DeliveryPlan`].
///
/// `text` is the turn body *after* `split_delivery` — it may carry a session
/// reset notice, a stray line of prose, or a Markdown code fence around the
/// envelope, so the envelope is located rather than assumed to be the whole
/// string (see [`find_envelope_span`]).
///
/// Follows the parameter style of [`crate::format::split_message`] and
/// `markdown::convert_tables`: plain values, no config struct, so the pure
/// parser stays testable without building a `Config`.
pub fn parse_structured(
    text: &str,
    expected_schema: &str,
    max_bubbles: usize,
    max_bubble_chars: usize,
) -> Result<DeliveryPlan, StructuredError> {
    let span = find_envelope_span(text).ok_or(StructuredError::NotStructured)?;
    if !span.complete {
        return Err(StructuredError::Truncated);
    }
    let raw = &text[span.start..span.end];

    let envelope: TurnEnvelopeV1 = serde_json::from_str(raw).map_err(|e| {
        // serde_json's Display carries field names and line/column offsets, not
        // document content — safe to log, still never sent to the user.
        StructuredError::Malformed(e.to_string())
    })?;

    if envelope.schema != expected_schema {
        return Err(StructuredError::SchemaMismatch {
            found: envelope.schema,
        });
    }

    if envelope.messages.len() > max_bubbles {
        return Err(StructuredError::TooManyBubbles {
            found: envelope.messages.len(),
            max: max_bubbles,
        });
    }

    let mut seen_ids: HashSet<&str> = HashSet::new();
    for (index, bubble) in envelope.messages.iter().enumerate() {
        let id = bubble.id.trim();
        if id.is_empty() {
            return Err(StructuredError::BlankBubbleId { index });
        }
        if !seen_ids.insert(id) {
            return Err(StructuredError::DuplicateBubbleId { index });
        }
        if bubble.text.trim().is_empty() {
            return Err(StructuredError::EmptyBubble { index });
        }
        let chars = bubble.text.chars().count();
        if max_bubble_chars > 0 && chars > max_bubble_chars {
            return Err(StructuredError::BubbleTooLong {
                index,
                chars,
                max: max_bubble_chars,
            });
        }
    }

    let next = match envelope.next {
        NextV1::Stop => NextAction::Stop,
        NextV1::Wait => NextAction::Wait,
        NextV1::Silent => NextAction::Silent,
        NextV1::Tool { name, arguments } => NextAction::Tool { name, arguments },
    };

    // `silent` wins over `messages`: the delivery loop only ever reads
    // `bubbles`, so a contradictory turn resolves here rather than at send time.
    if next == NextAction::Silent {
        if !envelope.messages.is_empty() {
            tracing::warn!(
                bubbles = envelope.messages.len(),
                "structured turn declared next.type=silent with non-empty messages; sending nothing"
            );
        }
        return Ok(DeliveryPlan {
            bubbles: Vec::new(),
            next,
        });
    }

    if envelope.messages.is_empty() {
        return Err(StructuredError::EmptyTurn);
    }

    Ok(DeliveryPlan {
        bubbles: envelope
            .messages
            .into_iter()
            .map(|b| b.text.trim().to_string())
            .collect(),
        next,
    })
}

/// Remove an envelope fragment (whole or truncated) from `text`, returning what
/// is left, trimmed.
///
/// This is what a `fallback_text` policy must send when
/// [`StructuredError::found_envelope`] is `true`. Text with no envelope-looking
/// fragment is returned unchanged, so a prose answer containing an unrelated
/// JSON snippet is not mangled.
pub fn strip_envelope(text: &str) -> String {
    match find_envelope_span(text) {
        Some(span) => {
            let mut out = String::with_capacity(text.len());
            out.push_str(&text[..span.start]);
            out.push_str(&text[span.end..]);
            out.trim().to_string()
        }
        None => text.trim().to_string(),
    }
}

/// Byte range of an envelope-looking JSON object inside a turn buffer.
struct EnvelopeSpan {
    start: usize,
    end: usize,
    /// `false` when the object was still open at end of input (truncated turn).
    complete: bool,
}

/// Locate the envelope inside `text`.
///
/// Scans once, tracking string literals and escapes so braces inside a bubble's
/// own `text` do not confuse the depth count, and records:
/// - the **last balanced** top-level `{...}` object, and
/// - a trailing **unclosed** `{` run, if the buffer ended mid-object.
///
/// An unclosed trailing object wins (it is the thing the agent was writing when
/// the turn was cut off). Each candidate must look like an envelope — contain a
/// `"schema"` or `"messages"` key — otherwise it is ignored, which keeps a JSON
/// code sample in an ordinary prose reply from being treated as a turn.
///
/// All of `{`, `}`, `"` and `\` are ASCII, and multi-byte UTF-8 continuation
/// bytes are all `>= 0x80`, so byte scanning cannot land mid-character and the
/// returned offsets are always char boundaries.
fn find_envelope_span(text: &str) -> Option<EnvelopeSpan> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut open_at: Option<usize> = None;
    let mut last_balanced: Option<(usize, usize)> = None;
    let mut in_string = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    open_at = Some(i);
                }
                depth += 1;
            }
            // A stray `}` at depth 0 is content, not structure — ignore it.
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = open_at.take() {
                        last_balanced = Some((start, i + 1));
                    }
                }
            }
            _ => {}
        }
    }

    // Truncated tail first: it is the object the turn died inside.
    if depth > 0 {
        if let Some(start) = open_at {
            if looks_like_envelope(&text[start..]) {
                return Some(EnvelopeSpan {
                    start,
                    end: text.len(),
                    complete: false,
                });
            }
        }
    }

    last_balanced.and_then(|(start, end)| {
        looks_like_envelope(&text[start..end]).then_some(EnvelopeSpan {
            start,
            end,
            complete: true,
        })
    })
}

/// Whether a JSON fragment carries the envelope's marker keys.
///
/// Deliberately key-based, not schema-value-based: a truncated turn may be cut
/// off before its `schema` value is written, and it still must not be delivered
/// as text.
fn looks_like_envelope(fragment: &str) -> bool {
    fragment.contains("\"schema\"") || fragment.contains("\"messages\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(text: &str) -> Result<DeliveryPlan, StructuredError> {
        parse_structured(
            text,
            SCHEMA_V1,
            DEFAULT_MAX_BUBBLES,
            DEFAULT_MAX_BUBBLE_CHARS,
        )
    }

    fn envelope(messages: Value, next: Value) -> String {
        json!({ "schema": SCHEMA_V1, "messages": messages, "next": next }).to_string()
    }

    // --- happy path ---

    #[test]
    fn three_bubbles_keep_their_order() {
        let plan = parse(&envelope(
            json!([
                {"id": "b1", "text": "red"},
                {"id": "b2", "text": "green"},
                {"id": "b3", "text": "blue"}
            ]),
            json!({"type": "stop"}),
        ))
        .unwrap();
        assert_eq!(plan.bubbles, vec!["red", "green", "blue"]);
        assert_eq!(plan.next, NextAction::Stop);
    }

    #[test]
    fn newlines_stay_inside_one_bubble() {
        // The load-bearing property: a newline is NOT a bubble boundary.
        let plan = parse(&envelope(
            json!([{"id": "b1", "text": "alpha\nbeta\ngamma"}]),
            json!({"type": "stop"}),
        ))
        .unwrap();
        assert_eq!(plan.bubbles.len(), 1);
        assert_eq!(plan.bubbles[0], "alpha\nbeta\ngamma");
    }

    #[test]
    fn missing_next_defaults_to_stop() {
        let raw = json!({
            "schema": SCHEMA_V1,
            "messages": [{"id": "b1", "text": "hey"}]
        })
        .to_string();
        let plan = parse(&raw).unwrap();
        assert_eq!(plan.next, NextAction::Stop);
        assert_eq!(plan.bubbles, vec!["hey"]);
    }

    #[test]
    fn wait_is_preserved_and_still_delivers() {
        let plan = parse(&envelope(
            json!([{"id": "b1", "text": "what's the address"}]),
            json!({"type": "wait"}),
        ))
        .unwrap();
        assert_eq!(plan.next, NextAction::Wait);
        assert!(!plan.is_silent());
    }

    #[test]
    fn tool_proposal_is_carried_through() {
        let plan = parse(&envelope(
            json!([{"id": "b1", "text": "on it"}]),
            json!({"type": "tool", "name": "gmail.search", "arguments": {"query": "lawyer"}}),
        ))
        .unwrap();
        assert_eq!(plan.bubbles, vec!["on it"]);
        match plan.next {
            NextAction::Tool { name, arguments } => {
                assert_eq!(name, "gmail.search");
                assert_eq!(arguments, json!({"query": "lawyer"}));
            }
            other => panic!("expected tool, got {other:?}"),
        }
    }

    #[test]
    fn bubble_text_is_trimmed() {
        let plan = parse(&envelope(
            json!([{"id": "b1", "text": "  hey  \n"}]),
            json!({"type": "stop"}),
        ))
        .unwrap();
        assert_eq!(plan.bubbles, vec!["hey"]);
    }

    // --- silent ---

    #[test]
    fn silent_sends_nothing() {
        let plan = parse(&envelope(json!([]), json!({"type": "silent"}))).unwrap();
        assert!(plan.is_silent());
        assert_eq!(plan.next, NextAction::Silent);
    }

    #[test]
    fn silent_overrides_non_empty_messages() {
        let plan = parse(&envelope(
            json!([{"id": "b1", "text": "ignore me"}]),
            json!({"type": "silent"}),
        ))
        .unwrap();
        assert!(plan.is_silent());
    }

    // --- tolerant extraction ---

    #[test]
    fn json_fence_is_tolerated() {
        let raw = format!(
            "```json\n{}\n```",
            envelope(
                json!([{"id": "b1", "text": "fenced"}]),
                json!({"type": "stop"})
            )
        );
        assert_eq!(parse(&raw).unwrap().bubbles, vec!["fenced"]);
    }

    #[test]
    fn leading_prose_is_tolerated() {
        let raw = format!(
            "sure, here you go:\n{}",
            envelope(
                json!([{"id": "b1", "text": "answer"}]),
                json!({"type": "stop"})
            )
        );
        assert_eq!(parse(&raw).unwrap().bubbles, vec!["answer"]);
    }

    #[test]
    fn session_reset_notice_before_envelope_is_tolerated() {
        // The reset notice is pushed at the head of the turn buffer
        // (adapter.rs), so the envelope never starts at byte 0 in that case.
        let raw = format!(
            "⚠️ _Session expired, starting fresh..._\n\n{}",
            envelope(
                json!([{"id": "b1", "text": "back"}]),
                json!({"type": "stop"})
            )
        );
        assert_eq!(parse(&raw).unwrap().bubbles, vec!["back"]);
    }

    #[test]
    fn braces_inside_bubble_text_do_not_break_scanning() {
        let plan = parse(&envelope(
            json!([{"id": "b1", "text": "use {\"a\": 1} like this }"}]),
            json!({"type": "stop"}),
        ))
        .unwrap();
        assert_eq!(plan.bubbles, vec!["use {\"a\": 1} like this }"]);
    }

    #[test]
    fn multibyte_text_survives_byte_scanning() {
        let plan = parse(&envelope(
            json!([{"id": "b1", "text": "今天天氣如何 🌤"}]),
            json!({"type": "stop"}),
        ))
        .unwrap();
        assert_eq!(plan.bubbles, vec!["今天天氣如何 🌤"]);
    }

    // --- rejection ---

    #[test]
    fn plain_prose_is_not_structured() {
        let err = parse("hey what's up").unwrap_err();
        assert_eq!(err, StructuredError::NotStructured);
        assert!(!err.found_envelope(), "prose is safe to deliver verbatim");
    }

    #[test]
    fn unrelated_json_snippet_is_not_an_envelope() {
        // A prose reply that happens to show the user some JSON.
        let err = parse("try this config:\n{\"retries\": 3}").unwrap_err();
        assert_eq!(err, StructuredError::NotStructured);
    }

    #[test]
    fn truncated_envelope_is_flagged_not_delivered() {
        let raw = "{\"schema\":\"openab.turn.v1\",\"messages\":[{\"id\":\"b1\",\"text\":\"half";
        let err = parse(raw).unwrap_err();
        assert_eq!(err, StructuredError::Truncated);
        assert!(
            err.found_envelope(),
            "a truncated envelope must never be sent as text"
        );
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let raw = json!({
            "schema": SCHEMA_V1,
            "messages": [{"id": "b1", "text": "hi"}],
            "next": {"type": "stop"},
            "confidence": 0.9
        })
        .to_string();
        assert!(matches!(
            parse(&raw).unwrap_err(),
            StructuredError::Malformed(_)
        ));
    }

    #[test]
    fn unknown_bubble_field_is_rejected() {
        let raw = envelope(
            json!([{"id": "b1", "text": "hi", "delay_ms": 200}]),
            json!({"type": "stop"}),
        );
        assert!(matches!(
            parse(&raw).unwrap_err(),
            StructuredError::Malformed(_)
        ));
    }

    #[test]
    fn wrong_schema_is_rejected() {
        let raw = json!({
            "schema": "openab.turn.v2",
            "messages": [{"id": "b1", "text": "hi"}],
            "next": {"type": "stop"}
        })
        .to_string();
        assert_eq!(
            parse(&raw).unwrap_err(),
            StructuredError::SchemaMismatch {
                found: "openab.turn.v2".into()
            }
        );
    }

    #[test]
    fn too_many_bubbles_is_rejected_not_truncated() {
        let raw = envelope(
            json!([
                {"id": "b1", "text": "1"},
                {"id": "b2", "text": "2"},
                {"id": "b3", "text": "3"},
                {"id": "b4", "text": "4"},
                {"id": "b5", "text": "5"}
            ]),
            json!({"type": "stop"}),
        );
        assert_eq!(
            parse(&raw).unwrap_err(),
            StructuredError::TooManyBubbles { found: 5, max: 4 }
        );
    }

    #[test]
    fn empty_bubble_text_is_rejected() {
        let raw = envelope(
            json!([{"id": "b1", "text": "   "}]),
            json!({"type": "stop"}),
        );
        assert_eq!(
            parse(&raw).unwrap_err(),
            StructuredError::EmptyBubble { index: 0 }
        );
    }

    #[test]
    fn blank_bubble_id_is_rejected() {
        let raw = envelope(json!([{"id": " ", "text": "hi"}]), json!({"type": "stop"}));
        assert_eq!(
            parse(&raw).unwrap_err(),
            StructuredError::BlankBubbleId { index: 0 }
        );
    }

    #[test]
    fn duplicate_bubble_id_is_rejected() {
        let raw = envelope(
            json!([
                {"id": "b1", "text": "one"},
                {"id": "b1", "text": "two"}
            ]),
            json!({"type": "stop"}),
        );
        assert_eq!(
            parse(&raw).unwrap_err(),
            StructuredError::DuplicateBubbleId { index: 1 }
        );
    }

    #[test]
    fn over_long_bubble_is_rejected() {
        let long = "x".repeat(DEFAULT_MAX_BUBBLE_CHARS + 1);
        let raw = envelope(json!([{"id": "b1", "text": long}]), json!({"type": "stop"}));
        assert_eq!(
            parse(&raw).unwrap_err(),
            StructuredError::BubbleTooLong {
                index: 0,
                chars: DEFAULT_MAX_BUBBLE_CHARS + 1,
                max: DEFAULT_MAX_BUBBLE_CHARS,
            }
        );
    }

    #[test]
    fn zero_max_bubble_chars_disables_the_length_check() {
        let long = "x".repeat(10_000);
        let raw = envelope(json!([{"id": "b1", "text": long}]), json!({"type": "stop"}));
        assert!(parse_structured(&raw, SCHEMA_V1, DEFAULT_MAX_BUBBLES, 0).is_ok());
    }

    #[test]
    fn empty_turn_without_silent_is_rejected() {
        let raw = envelope(json!([]), json!({"type": "stop"}));
        assert_eq!(parse(&raw).unwrap_err(), StructuredError::EmptyTurn);
    }

    #[test]
    fn unknown_next_type_is_rejected() {
        let raw = envelope(
            json!([{"id": "b1", "text": "hi"}]),
            json!({"type": "escalate"}),
        );
        assert!(matches!(
            parse(&raw).unwrap_err(),
            StructuredError::Malformed(_)
        ));
    }

    #[test]
    fn error_display_never_carries_bubble_text() {
        let raw = envelope(
            json!([{"id": "b1", "text": "SECRET-CANARY"}, {"id": "b1", "text": "x"}]),
            json!({"type": "stop"}),
        );
        let rendered = parse(&raw).unwrap_err().to_string();
        assert!(!rendered.contains("SECRET-CANARY"), "got: {rendered}");
    }

    // --- strip_envelope (the leak guard) ---

    #[test]
    fn strip_removes_a_whole_envelope() {
        let raw = format!(
            "here you go:\n{}\nhope that helps",
            envelope(
                json!([{"id": "b1", "text": "CANARY"}]),
                json!({"type": "stop"})
            )
        );
        let stripped = strip_envelope(&raw);
        assert!(!stripped.contains("CANARY"));
        assert!(!stripped.contains("schema"));
        assert_eq!(stripped, "here you go:\n\nhope that helps");
    }

    #[test]
    fn strip_removes_a_truncated_envelope() {
        let raw =
            "sorry, one sec\n{\"schema\":\"openab.turn.v1\",\"messages\":[{\"id\":\"b1\",\"te";
        let stripped = strip_envelope(raw);
        assert_eq!(stripped, "sorry, one sec");
        assert!(!stripped.contains('{'));
    }

    #[test]
    fn strip_leaves_ordinary_prose_untouched() {
        let raw = "try this config:\n{\"retries\": 3}";
        assert_eq!(strip_envelope(raw), raw);
    }

    #[test]
    fn strip_can_return_empty_when_the_turn_was_only_an_envelope() {
        let raw = envelope(json!([{"id": "b1", "text": "hi"}]), json!({"type": "stop"}));
        assert!(strip_envelope(&raw).is_empty());
    }

    // --- cross-crate contract ---

    /// The shared contract fixture, byte-identical to the one `openab-agent`'s
    /// `turn_envelope::render` is tested against. That crate is its own
    /// workspace and cannot be called from here, so the fixture is the seam.
    /// If this test fails, the producer and the parser have drifted apart.
    const CONTRACT_FIXTURE: &str = include_str!("../../../docs/fixtures/turn-envelope-v1.json");

    #[test]
    fn the_agents_envelope_parses_into_the_expected_bubbles() {
        let plan = parse(CONTRACT_FIXTURE).unwrap();
        assert_eq!(
            plan.bubbles,
            vec!["on it", "your flight moved to 8pm\ngate B12"]
        );
        assert_eq!(plan.next, NextAction::Stop);
        // The second bubble's newline stays inside it — one bubble, two lines.
        assert_eq!(plan.bubbles[1].lines().count(), 2);
    }

    // --- config enums ---

    #[test]
    fn delivery_mode_defaults_to_text() {
        assert_eq!(DeliveryMode::default(), DeliveryMode::Text);
    }

    #[test]
    fn delivery_mode_deserializes_known_values() {
        assert_eq!(
            serde_json::from_str::<DeliveryMode>("\"structured\"").unwrap(),
            DeliveryMode::Structured
        );
        assert_eq!(
            serde_json::from_str::<DeliveryMode>("\"sequential\"").unwrap(),
            DeliveryMode::Sequential
        );
        assert_eq!(
            serde_json::from_str::<DeliveryMode>("\"TEXT\"").unwrap(),
            DeliveryMode::Text
        );
        assert!(serde_json::from_str::<DeliveryMode>("\"bubblez\"").is_err());
    }

    #[test]
    fn only_text_mode_keeps_streaming() {
        assert!(!DeliveryMode::Text.is_bubbles());
        assert!(DeliveryMode::Structured.is_bubbles());
        assert!(DeliveryMode::Sequential.is_bubbles());
    }

    #[test]
    fn parse_error_policy_defaults_to_fallback_text() {
        assert_eq!(ParseErrorPolicy::default(), ParseErrorPolicy::FallbackText);
        assert_eq!(
            serde_json::from_str::<ParseErrorPolicy>("\"silent\"").unwrap(),
            ParseErrorPolicy::Silent
        );
        assert!(serde_json::from_str::<ParseErrorPolicy>("\"retry\"").is_err());
    }
}
