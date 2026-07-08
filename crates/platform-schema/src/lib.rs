//! Platform capability schema — authoritative types for `docs/platforms/schema/*.toml`.
//!
//! Each `schema/<platform>.toml` deserializes into [`Platform`]. The conformance
//! tests in `tests/conformance.rs` validate structure, enums, the closed feature
//! set, and that every code-ref `source` still exists in the tree.
//!
//! The blank template + human-readable field docs live in
//! `docs/platforms/_template.toml`; keep the two in sync (a conformance test
//! checks the template still enumerates every section + feature key).

use serde::Deserialize;

/// Current schema version. Bump when the schema changes; stale files are flagged.
pub const SCHEMA_VERSION: &str = "2026-07-08";

/// The complete, closed OpenAB feature set (Schema 2). Every platform file must
/// contain exactly these keys, once each.
pub const EXPECTED_FEATURES: &[&str] = &[
    "send_message",
    "message_split",
    "streaming",
    "reply_quote",
    "edit_message",
    "delete_message",
    "emoji_reactions",
    "threads_topics",
    "media_inbound",
    "voice_stt",
    "trust_gate",
    "deny_echo",
    "mention_gating",
    "slash_commands",
    "multibot",
    "group_routing",
    "cron_dispatch",
];

/// Every `[capability.*]` sub-section name, in template order.
pub const CAPABILITY_SECTIONS: &[&str] = &[
    "transport",
    "inbound_auth",
    "threads",
    "slash_commands",
    "mentions",
    "emoji_reactions",
    "edit_message",
    "delete_message",
    "rich_content",
    "attachments",
    "message_length_limit",
    "dm_support",
    "group_model",
    "group_sender_identity",
    "send_model",
    "proactive_push",
    "bot_to_bot",
    "typing_indicator",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    pub schema_version: String,
    pub platform: Identity,
    pub capability: Capability,
    #[serde(default)]
    pub openab_features: Vec<Feature>,
    #[serde(default)]
    pub quirks: Vec<Quirk>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub name: String,
    pub official_docs: String,
    pub description: String,
}

// ─── Schema 1 — platform-capability ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub transport: Transport,
    pub inbound_auth: InboundAuth,
    pub threads: Threads,
    pub slash_commands: SlashCommands,
    pub mentions: Mentions,
    pub emoji_reactions: EmojiReactions,
    pub edit_message: EditMessage,
    pub delete_message: DeleteMessage,
    pub rich_content: RichContent,
    pub attachments: Attachments,
    pub message_length_limit: MessageLengthLimit,
    pub dm_support: DmSupport,
    pub group_model: GroupModel,
    pub group_sender_identity: GroupSenderIdentity,
    pub send_model: SendModel,
    pub proactive_push: ProactivePush,
    pub bot_to_bot: BotToBot,
    pub typing_indicator: TypingIndicator,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Webhook,
    Websocket,
    SocketMode,
    LongPoll,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transport {
    pub kind: TransportKind,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    HmacSha256,
    JwtRs256,
    Aes,
    SharedSecret,
    Oauth,
    None,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundAuth {
    pub scheme: AuthScheme,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadModel {
    Native,
    ReplyToOnly,
    Emulated,
    None,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Threads {
    pub model: ThreadModel,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlashCommands {
    pub supported: bool,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionMethod {
    AtMention,
    Username,
    SelfFlag,
    None,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mentions {
    pub method: MentionMethod,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmojiReactions {
    pub bot_can_add: bool,
    pub bot_can_remove: bool,
    pub bot_receives_events: bool,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditMessage {
    pub supported: bool,
    /// Cap on edits per message, if the platform imposes one (e.g. Feishu = 20).
    #[serde(default)]
    pub max_edits: Option<u32>,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteScope {
    None,
    Own,
    Others,
    OwnAndOthers,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteMessage {
    pub supported: bool,
    pub scope: DeleteScope,
    /// Deletion time window in seconds, if bounded (e.g. WeCom recall = 86400).
    #[serde(default)]
    pub window_sec: Option<u32>,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichContent {
    pub markdown: bool,
    pub cards: bool,
    pub buttons: bool,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Audio,
    Video,
    File,
}

/// How the bot delivers outbound media: by URL reference, or by uploading bytes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundDelivery {
    Url,
    Upload,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachments {
    pub inbound: Vec<AttachmentKind>,
    pub outbound: Vec<AttachmentKind>,
    #[serde(default)]
    pub max_size_mb: Option<u32>,
    /// Max attachments per message, if the platform caps it (e.g. Discord = 10).
    #[serde(default)]
    pub max_count: Option<u32>,
    #[serde(default)]
    pub outbound_delivery: Option<OutboundDelivery>,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageLengthLimit {
    pub max_chars: u32,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DmSupport {
    pub supported: bool,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupModel {
    pub kinds: Vec<String>,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StableId {
    Yes,
    No,
    ConsentGated,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSenderIdentity {
    pub stable_id: StableId,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendModelKind {
    AnyTime,
    ReplyOnly,
    PushOnly,
    Hybrid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendModel {
    pub model: SendModelKind,
    #[serde(default)]
    pub reply_token_ttl_sec: Option<u32>,
    #[serde(default)]
    pub max_objects_per_send: Option<u32>,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaModel {
    Unlimited,
    Metered,
    None,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProactivePush {
    pub supported: bool,
    pub quota_model: QuotaModel,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotToBot {
    pub delivered: bool,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypingIndicator {
    pub supported: bool,
    pub note: String,
    pub source: String,
}

// ─── Schema 2 — openab-feature-support ──────────────────────────────────────

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Implemented,
    Partial,
    Workaround,
    NotImplemented,
    #[serde(rename = "n_a")]
    Na,
}

impl Status {
    /// A claimed-present feature must cite where it lives.
    pub fn requires_source(self) -> bool {
        matches!(self, Status::Implemented | Status::Partial | Status::Workaround)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    pub feature: String,
    pub status: Status,
    pub note: String,
    #[serde(default)]
    pub source: Vec<String>,
    #[serde(default)]
    pub pr: Option<String>,
}

// ─── Schema 3 — platform-quirks ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuirkKind {
    Intrinsic,
    OpenabDecision,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quirk {
    pub date: String,
    pub title: String,
    pub note: String,
    pub kind: QuirkKind,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub refs: Vec<String>,
}

// ─── source-ref parsing ─────────────────────────────────────────────────────

/// A parsed code-ref source: `"path/to/file.rs"` or `"path/to/file.rs#symbol"`.
#[derive(Debug)]
pub struct CodeRef<'a> {
    pub file: &'a str,
    pub symbol: Option<&'a str>,
}

/// Split a source string into file + optional `#symbol`.
pub fn parse_code_ref(s: &str) -> CodeRef<'_> {
    match s.split_once('#') {
        Some((file, symbol)) => CodeRef { file, symbol: Some(symbol) },
        None => CodeRef { file: s, symbol: None },
    }
}

/// Does this source string look like an in-repo code ref (vs an official-doc URL)?
pub fn is_code_ref(s: &str) -> bool {
    !s.starts_with("http://") && !s.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_ref_parsing() {
        let r = parse_code_ref("crates/a/src/b.rs#foo");
        assert_eq!(r.file, "crates/a/src/b.rs");
        assert_eq!(r.symbol, Some("foo"));
        assert_eq!(parse_code_ref("crates/a/src/b.rs").symbol, None);
        assert!(is_code_ref("crates/a.rs"));
        assert!(!is_code_ref("https://example.com"));
    }

    #[test]
    fn rejects_out_of_set_enum() {
        let toml = "kind = \"carrier_pigeon\"\nnote = \"x\"\nsource = \"y\"";
        assert!(toml::from_str::<Transport>(toml).is_err());
    }

    #[test]
    fn rejects_unknown_field() {
        let toml = "supported = true\nnote = \"x\"\nsource = \"y\"\nbogus = 1";
        assert!(toml::from_str::<SlashCommands>(toml).is_err());
    }

    #[test]
    fn status_source_requirement() {
        assert!(Status::Implemented.requires_source());
        assert!(!Status::Na.requires_source());
    }
}
