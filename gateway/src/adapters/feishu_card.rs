//! Feishu CardKit v2 streaming card client.
//!
//! Thin reqwest wrapper around the Feishu CardKit v1 REST API
//! (`/open-apis/cardkit/v1/*`), used to drive streaming replies that are free
//! of the `PATCH /im/v1/messages` 20-edit cap (errcode 230072) and render
//! markdown / tables natively (Issue #1124).
//!
//! Endpoint shapes are taken from the official docs:
//! - Create card entity:  POST  `/open-apis/cardkit/v1/cards`
//! - Stream text update:  PUT   `/open-apis/cardkit/v1/cards/:card_id/elements/:element_id/content`
//! - Update card config:  PATCH `/open-apis/cardkit/v1/cards/:card_id/settings`
//! - Send card message:   POST  `/open-apis/im/v1/messages[/:id/reply]` (msg_type=interactive)
//!
//! Design notes / pitfalls baked in:
//! - The stream-text API takes the FULL accumulated text, NOT a delta. Feishu
//!   renders a typewriter effect only when the new text has the old text as a
//!   prefix; otherwise it replaces wholesale. Passing deltas is the
//!   openclaw-lark #565 bug — guarded by `update_card_stream_*` tests.
//! - `sequence` must STRICTLY increase across every operation on one card
//!   (errcode 300317 otherwise). The counter is owned by the session (S4);
//!   this module just forwards whatever sequence it is given.
//! - A card entity can be SENT only once and expires after 14 days; the
//!   session (S4/S5) creates a fresh entity per streaming reply.
//! - `streaming_mode` must be on (set at create time) for the content API to
//!   work (errcode 300309 / 200850 otherwise); finalize turns it back off.
//!
// Most of this module is wired in as of S5 (REST client + session registry +
// idle reaper). A few convenience APIs remain exercised only by tests or
// reserved for phase two (card splitting): `FeishuStreamRegistry::{get,
// contains, len, is_empty}`.

use serde_json::Value;
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

/// Element ID of the single streaming markdown element in our MVP card.
/// Must match between `markdown_to_card_v2` (create) and `update_card_stream`.
pub const STREAM_ELEMENT_ID: &str = "md_stream";

/// Outcome of a CardKit streaming operation, classified for the S5 caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardOutcome {
    /// Operation applied (HTTP 2xx + code 0).
    Updated,
    /// Rate limited (HTTP 429). Caller may skip this frame and retry later.
    RateLimited,
    /// Any other failure: HTTP error status, non-zero errcode, or a transport
    /// / body-parse error. Caller should fall back to the post path. Carries
    /// the Feishu errcode (0 when not a structured API error) plus a short
    /// message for logging.
    Failed { code: i64, message: String },
}

// ---------------------------------------------------------------------------
// Redundant table-fence stripping (E2E fix, 2026-06-19)
//
// Agents often wrap a GFM table in a bare ``` fence to get monospace alignment
// in environments that DON'T render tables (Discord, terminals). Feishu cards
// DO render tables, so on the card path we unwrap a fence whose body is exactly
// one complete GFM table — platform-correct adaptation, the adapter's job.
// Strict guards keep false positives/negatives near zero; never drops bytes.
// ---------------------------------------------------------------------------

/// A bare code fence line: trimmed content is exactly three backticks (no
/// language tag). Tagged fences (e.g. ```rust) are never treated as bare.
fn is_bare_fence(line: &str) -> bool {
    line.trim() == "```"
}

/// Index of the next bare fence at or after `start` (the closing fence).
fn find_closing_fence(lines: &[&str], start: usize) -> Option<usize> {
    (start..lines.len()).find(|&j| is_bare_fence(lines[j]))
}

/// Trim leading/trailing blank lines; return the `[start, end)` bounds.
fn trim_blank_bounds(lines: &[&str]) -> (usize, usize) {
    let mut s = 0;
    let mut e = lines.len();
    while s < e && lines[s].trim().is_empty() {
        s += 1;
    }
    while e > s && lines[e - 1].trim().is_empty() {
        e -= 1;
    }
    (s, e)
}

/// Count GFM table columns in a row (outer pipes stripped before splitting).
fn count_table_cols(line: &str) -> usize {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').count()
}

/// A GFM delimiter cell: optional leading `:`, one-or-more `-`, optional
/// trailing `:` (matches `^:?-+:?$` on the trimmed cell).
fn is_delim_cell(cell: &str) -> bool {
    let c = cell.trim().as_bytes();
    if c.is_empty() {
        return false;
    }
    let mut i = 0;
    if c[i] == b':' {
        i += 1;
    }
    let dash_start = i;
    while i < c.len() && c[i] == b'-' {
        i += 1;
    }
    if i == dash_start {
        return false; // need at least one dash
    }
    if i < c.len() && c[i] == b':' {
        i += 1;
    }
    i == c.len()
}

/// True if `body` (the lines between fences) is EXACTLY one well-formed GFM
/// table and nothing else: a header row with pipes, a delimiter row whose
/// column count matches and whose cells are all valid delimiters, and ≥1 data
/// row. Blank lines are tolerated only at the leading/trailing edges; any
/// interior blank line or non-table line disqualifies it.
fn is_single_gfm_table(body: &[&str]) -> bool {
    let (s, e) = trim_blank_bounds(body);
    let rows = &body[s..e];
    if rows.len() < 3 {
        return false; // header + delimiter + at least one data row
    }
    if rows.iter().any(|l| l.trim().is_empty()) {
        return false; // no interior blank lines
    }
    if !rows[0].contains('|') {
        return false; // header must have a pipe
    }
    let cols = count_table_cols(rows[0]);
    if cols == 0 {
        return false;
    }
    let delim = rows[1].trim();
    if !delim.contains('|') || !delim.contains('-') || count_table_cols(rows[1]) != cols {
        return false;
    }
    let dinner = delim.strip_prefix('|').unwrap_or(delim);
    let dinner = dinner.strip_suffix('|').unwrap_or(dinner);
    if !dinner.split('|').all(is_delim_cell) {
        return false;
    }
    // Every data row must contain a pipe.
    rows[2..].iter().all(|l| l.contains('|'))
}

/// Unwrap any bare code fence whose body is exactly one GFM table, so Feishu
/// renders it as a native table instead of a code block. Only CLOSED, untagged
/// fences qualify — an unclosed fence (mid-stream) and any non-table body are
/// left untouched, so streaming snapshots are stable and never flicker.
/// Returns `Cow::Borrowed` unchanged when nothing matches (never drops bytes).
fn strip_redundant_table_fence(text: &str) -> Cow<'_, str> {
    if !text.contains("```") {
        return Cow::Borrowed(text);
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        if is_bare_fence(lines[i]) {
            if let Some(close) = find_closing_fence(&lines, i + 1) {
                let body = &lines[i + 1..close];
                if is_single_gfm_table(body) {
                    let (s, e) = trim_blank_bounds(body);
                    out.extend_from_slice(&body[s..e]);
                    changed = true;
                } else {
                    // Not a lone table: keep the whole fenced block verbatim
                    // (including both fence lines) so nothing is altered.
                    out.extend_from_slice(&lines[i..=close]);
                }
                i = close + 1;
                continue;
            }
            // Unclosed fence (mid-stream): leave the rest as-is.
        }
        out.push(lines[i]);
        i += 1;
    }
    if changed {
        Cow::Owned(out.join("\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// Build a CardKit JSON 2.0 card holding a single markdown element.
///
/// - `schema` is pinned to `"2.0"` (the only structure the API accepts).
/// - `streaming` toggles `streaming_mode`: `true` for the live typewriter card,
///   `false` for the finalized static card.
/// - The text is first passed through `strip_redundant_table_fence` so a
///   table wrapped in a bare ``` fence renders as a native table.
/// - `update_multi` is left at its default (`true`); setting it `false` is
///   rejected in streaming mode (errcode 300302), so we never emit it.
pub fn markdown_to_card_v2(text: &str, streaming: bool) -> Value {
    let text = strip_redundant_table_fence(text);
    let text = text.as_ref();
    let mut config = serde_json::json!({ "streaming_mode": streaming });
    if streaming {
        config["streaming_config"] = serde_json::json!({ "print_strategy": "fast" });
    }
    serde_json::json!({
        "schema": "2.0",
        "config": config,
        "body": {
            "elements": [
                {
                    "tag": "markdown",
                    "content": text,
                    "element_id": STREAM_ELEMENT_ID
                }
            ]
        }
    })
}

/// Classify a CardKit (PUT/PATCH) response into a `CardOutcome`.
async fn classify(resp: reqwest::Response, op: &'static str) -> CardOutcome {
    let status = resp.status();
    if status.as_u16() == 429 {
        tracing::warn!(op, "feishu cardkit rate limited (429)");
        return CardOutcome::RateLimited;
    }
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return CardOutcome::Failed {
                code: 0,
                message: format!("{op}: bad response body: {e}"),
            };
        }
    };
    let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if status.is_success() && code == 0 {
        CardOutcome::Updated
    } else {
        let msg = body
            .get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        tracing::warn!(op, %status, code, msg = %msg, "feishu cardkit op failed");
        CardOutcome::Failed { code, message: msg }
    }
}

/// Create a streaming card entity seeded with the initial (full) text.
/// Returns the new `card_id` on success, or a `CardOutcome` describing the
/// failure (so the caller can decide between fallback and skip).
pub async fn create_streaming_card(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    text: &str,
) -> Result<String, CardOutcome> {
    let card = markdown_to_card_v2(text, true);
    let body = serde_json::json!({
        "type": "card_json",
        "data": card.to_string(),
    });
    let url = format!("{api_base}/open-apis/cardkit/v1/cards");
    let resp = match client
        .post(&url)
        .bearer_auth(token)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return Err(CardOutcome::Failed {
                code: 0,
                message: format!("create: request error: {e}"),
            })
        }
    };
    let status = resp.status();
    if status.as_u16() == 429 {
        return Err(CardOutcome::RateLimited);
    }
    let rb: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return Err(CardOutcome::Failed {
                code: 0,
                message: format!("create: bad response body: {e}"),
            })
        }
    };
    let code = rb.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if status.is_success() && code == 0 {
        if let Some(id) = rb.pointer("/data/card_id").and_then(|v| v.as_str()) {
            tracing::info!(card_id = %id, "feishu streaming card created");
            return Ok(id.to_string());
        }
        return Err(CardOutcome::Failed {
            code: 0,
            message: "create: response missing data.card_id".into(),
        });
    }
    let msg = rb
        .get("msg")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    tracing::warn!(%status, code, msg = %msg, "feishu create card failed");
    Err(CardOutcome::Failed { code, message: msg })
}

/// Send an interactive card message referencing `card_id`.
///
/// Mirrors `feishu::send_post_message`: uses the reply API when `reply_to` is
/// `Some(root_id)` (stays in-thread), otherwise creates a new message in the
/// chat. Returns the sent `message_id` on success.
pub async fn send_card_message(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: &str,
    reply_to: Option<&str>,
    card_id: &str,
) -> Option<String> {
    let content = serde_json::json!({
        "type": "card",
        "data": { "card_id": card_id }
    })
    .to_string();

    let (url, body) = if let Some(root_id) = reply_to {
        (
            format!("{api_base}/open-apis/im/v1/messages/{root_id}/reply"),
            serde_json::json!({
                "msg_type": "interactive",
                "content": content,
            }),
        )
    } else {
        (
            format!("{api_base}/open-apis/im/v1/messages?receive_id_type=chat_id"),
            serde_json::json!({
                "receive_id": chat_id,
                "msg_type": "interactive",
                "content": content,
            }),
        )
    };

    match client
        .post(&url)
        .bearer_auth(token)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let rb: Value = resp.json().await.unwrap_or_default();
            let mid = rb
                .pointer("/data/message_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            tracing::info!(chat_id = %chat_id, reply_to = ?reply_to, message_id = ?mid, "feishu card message sent");
            mid
        }
        Ok(resp) => {
            let status = resp.status();
            let t = resp.text().await.unwrap_or_default();
            tracing::error!(status = %status, body = %t, "feishu send card message failed");
            None
        }
        Err(e) => {
            tracing::error!(err = %e, "feishu send card message request failed");
            None
        }
    }
}

/// Stream a FULL text snapshot to the card's markdown element.
///
/// `text` MUST be the cumulative content, never a delta (#565). `sequence`
/// must strictly increase per card across all operations (#317). This
/// function forwards both verbatim and does not accumulate, truncate, or
/// diff — accumulation is the session's job (S5).
pub async fn update_card_stream(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    card_id: &str,
    element_id: &str,
    text: &str,
    sequence: i64,
) -> CardOutcome {
    // Three-exit consistency: unwrap a redundant table fence here too, so a
    // closed table fence renders natively mid-stream (no flicker) and matches
    // the create/finalize paths.
    let text = strip_redundant_table_fence(text);
    let url =
        format!("{api_base}/open-apis/cardkit/v1/cards/{card_id}/elements/{element_id}/content");
    let body = serde_json::json!({
        "content": text.as_ref(),
        "sequence": sequence,
    });
    match client
        .put(&url)
        .bearer_auth(token)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => classify(resp, "update_content").await,
        Err(e) => CardOutcome::Failed {
            code: 0,
            message: format!("update: request error: {e}"),
        },
    }
}

/// Finalize the stream by REPLACING the card with a STATIC card carrying the
/// final full text (`PUT /cardkit/v1/cards/:id`, full card/update).
///
/// A full replace forces Feishu to re-parse the markdown statically, which is
/// what fixes GFM tables: during streaming they render as a code block, and a
/// PATCH-settings (streaming_mode=false) alone only stops the cursor without
/// re-rendering — the broken table would persist. Replacing with a static card
/// re-renders the table correctly (E2E finding, 2026-06-19). `sequence` must be
/// greater than the last content update's sequence.
pub async fn finish_card_stream(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    card_id: &str,
    text: &str,
    sequence: i64,
) -> CardOutcome {
    let card = markdown_to_card_v2(text, false);
    let body = serde_json::json!({
        "card": { "type": "card_json", "data": card.to_string() },
        "sequence": sequence,
    });
    let url = format!("{api_base}/open-apis/cardkit/v1/cards/{card_id}");
    match client
        .put(&url)
        .bearer_auth(token)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => classify(resp, "finish_card_update").await,
        Err(e) => CardOutcome::Failed {
            code: 0,
            message: format!("finish: request error: {e}"),
        },
    }
}

// ---------------------------------------------------------------------------
// Streaming session state (S4): registry + state machine + FIFO eviction
// ---------------------------------------------------------------------------

/// Max concurrent card-streaming sessions before FIFO eviction kicks in.
const STREAM_SESSIONS_MAX: usize = 1024;

/// State of one promoted card-streaming reply.
///
/// A session exists only AFTER a reply has been promoted from the post path to
/// a CardKit card. In the registry it is keyed by the placeholder *post*
/// message_id that core still believes it is editing (`om_post`), so core stays
/// oblivious to the post→card swap — no core or schema change.
pub struct FeishuStreamSession {
    /// CardKit card entity id — the streaming target for content updates.
    pub card_id: String,
    /// message_id of the sent interactive card message (for cleanup / delete).
    pub card_message_id: String,
    /// Monotonic op counter. Every CardKit op (content update, finalize) must
    /// use a STRICTLY increasing sequence, else Feishu rejects it (errcode
    /// 300317). The session owns this counter.
    pub sequence: i64,
    /// Last activity time, for the idle-finalize reaper (S5).
    pub last_activity: Instant,
    /// True once finalize (streaming_mode off) has been sent, so the reaper
    /// does not double-finalize.
    pub finalized: bool,
    /// Latest full text snapshot pushed to the card. The idle reaper uses it to
    /// rebuild a STATIC card at finalize (which fixes streaming-mode tables).
    pub last_text: String,
}

impl FeishuStreamSession {
    fn new(card_id: String, card_message_id: String, initial_text: String) -> Self {
        Self {
            card_id,
            card_message_id,
            sequence: 0,
            last_activity: Instant::now(),
            finalized: false,
            last_text: initial_text,
        }
    }

    /// Advance to the next strictly-increasing sequence and refresh activity.
    /// Used for every content update and for finalize.
    pub fn next_sequence(&mut self) -> i64 {
        self.sequence += 1;
        self.last_activity = Instant::now();
        self.sequence
    }

    /// Mark finalized so the idle reaper won't finalize this session again.
    pub fn mark_finalized(&mut self) {
        self.finalized = true;
    }

    /// Whether this session has been idle for at least `idle_ms` and is not
    /// yet finalized — i.e. a finalize candidate for the reaper.
    pub fn is_idle(&self, idle_ms: u64) -> bool {
        !self.finalized && self.last_activity.elapsed().as_millis() as u64 >= idle_ms
    }
}

/// Registry of active card-streaming sessions, keyed by `om_post`.
///
/// Insertion-order FIFO eviction mirrors `EditCountsCache` in feishu.rs: the
/// oldest *insertions* age out first, which strongly favours keeping active
/// (recently promoted) streams over stale ones.
#[derive(Default)]
pub struct FeishuStreamRegistry {
    sessions: HashMap<String, FeishuStreamSession>,
    order: VecDeque<String>,
}

impl FeishuStreamRegistry {
    /// Register a freshly-promoted session under its placeholder `om_post` key.
    /// Promotion is one-way; if the key somehow already exists it is replaced
    /// but keeps its FIFO position (no duplicate `order` entry).
    pub fn promote(
        &mut self,
        om_post: &str,
        card_id: String,
        card_message_id: String,
        initial_text: String,
    ) {
        let was_new = !self.sessions.contains_key(om_post);
        self.sessions.insert(
            om_post.to_string(),
            FeishuStreamSession::new(card_id, card_message_id, initial_text),
        );
        if was_new {
            self.order.push_back(om_post.to_string());
            self.evict_if_overcap();
        }
    }

    /// Returns the session for `om_post`, if any.
    ///
    /// Reserved for phase-two card splitting; production callers use `get_mut`.
    #[allow(dead_code)]
    pub fn get(&self, om_post: &str) -> Option<&FeishuStreamSession> {
        self.sessions.get(om_post)
    }

    pub fn get_mut(&mut self, om_post: &str) -> Option<&mut FeishuStreamSession> {
        self.sessions.get_mut(om_post)
    }

    /// Returns `true` if a session for `om_post` exists.
    ///
    /// Reserved for phase-two card splitting.
    #[allow(dead_code)]
    pub fn contains(&self, om_post: &str) -> bool {
        self.sessions.contains_key(om_post)
    }

    /// Remove a session (after finalize / cleanup). `order` keeps the key; the
    /// FIFO evictor tolerates and skips entries already gone from `sessions`.
    pub fn remove(&mut self, om_post: &str) -> Option<FeishuStreamSession> {
        self.sessions.remove(om_post)
    }

    /// Number of active sessions.
    ///
    /// Reserved for phase-two card splitting.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Returns `true` if no sessions are active.
    ///
    /// Reserved for phase-two card splitting.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Keys of sessions that are finalize candidates for the idle reaper: they
    /// have actually streamed at least once (`sequence > 0`), are idle ≥
    /// `idle_ms`, and are not yet finalized.
    ///
    /// The `sequence > 0` guard is essential. A freshly created session (first
    /// reply sent as card, or a just-promoted card) sits at sequence 0 while
    /// core is still thinking before the first content edit. Finalizing it then
    /// would freeze the placeholder before any content arrives — exactly the
    /// bug that the post path avoids by not creating a session until the first
    /// edit. So the reaper waits until at least one real stream update lands.
    pub fn idle_keys(&self, idle_ms: u64) -> Vec<String> {
        self.sessions
            .iter()
            .filter(|(_, s)| s.sequence > 0 && s.is_idle(idle_ms))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// FIFO eviction: when over `STREAM_SESSIONS_MAX`, drop the oldest half by
    /// insertion order. Tolerant of order/sessions drift — keys already gone
    /// from `sessions` are skipped without counting toward the eviction quota.
    fn evict_if_overcap(&mut self) {
        if self.sessions.len() > STREAM_SESSIONS_MAX {
            let target = self.sessions.len() / 2;
            let mut evicted = 0;
            while evicted < target {
                match self.order.pop_front() {
                    Some(oldest) => {
                        if self.sessions.remove(&oldest).is_some() {
                            evicted += 1;
                        }
                    }
                    None => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // --- markdown_to_card_v2 (pure) ---

    #[test]
    fn card_v2_streaming_has_schema_and_element() {
        let card = markdown_to_card_v2("hello", true);
        assert_eq!(card["schema"], "2.0");
        assert_eq!(card["config"]["streaming_mode"], true);
        assert_eq!(card["config"]["streaming_config"]["print_strategy"], "fast");
        // update_multi must NOT be emitted as false (300302).
        assert!(card["config"].get("update_multi").is_none());
        let el = &card["body"]["elements"][0];
        assert_eq!(el["tag"], "markdown");
        assert_eq!(el["element_id"], STREAM_ELEMENT_ID);
        assert_eq!(el["content"], "hello");
    }

    #[test]
    fn card_v2_static_disables_streaming() {
        // The finalize rebuild uses a static card: streaming_mode=false and no
        // streaming_config. This is what re-renders GFM tables correctly.
        let card = markdown_to_card_v2("| a | b |\n| - | - |\n| 1 | 2 |", false);
        assert_eq!(card["config"]["streaming_mode"], false);
        assert!(card["config"].get("streaming_config").is_none());
        assert_eq!(card["body"]["elements"][0]["element_id"], STREAM_ELEMENT_ID);
    }

    // --- strip_redundant_table_fence (E2E table fix) ---

    const TBL: &str = "| 特性 | Box | Rc |\n| --- | --- | --- |\n| 所有權 | 獨佔 | 共享 |";

    #[test]
    fn strip_unwraps_bare_fenced_table() {
        let wrapped = format!("## 標題\n\n```\n{TBL}\n```\n\n結尾");
        let got = strip_redundant_table_fence(&wrapped);
        assert_eq!(got.as_ref(), format!("## 標題\n\n{TBL}\n\n結尾"));
    }

    #[test]
    fn strip_unwraps_multiple_independent_fences() {
        let input = format!("```\n{TBL}\n```\n中間\n```\n{TBL}\n```");
        let got = strip_redundant_table_fence(&input);
        assert_eq!(got.as_ref(), format!("{TBL}\n中間\n{TBL}"));
    }

    #[test]
    fn strip_tolerates_edge_blank_lines_in_fence() {
        let input = format!("```\n\n{TBL}\n\n```");
        assert_eq!(strip_redundant_table_fence(&input).as_ref(), TBL);
    }

    #[test]
    fn strip_handles_aligned_delimiters() {
        let tbl = "| a | b | c |\n| :-- | :-: | --: |\n| 1 | 2 | 3 |";
        let input = format!("```\n{tbl}\n```");
        assert_eq!(strip_redundant_table_fence(&input).as_ref(), tbl);
    }

    #[test]
    fn strip_is_idempotent() {
        let wrapped = format!("```\n{TBL}\n```");
        let once = strip_redundant_table_fence(&wrapped).into_owned();
        let twice = strip_redundant_table_fence(&once);
        assert_eq!(twice.as_ref(), once);
    }

    // negatives: must NOT unwrap (Cow::Borrowed, content unchanged)

    #[test]
    fn strip_keeps_language_tagged_fence() {
        let input = "```rust\nfn main() {}\n```";
        assert!(matches!(
            strip_redundant_table_fence(input),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strip_keeps_fence_with_table_plus_prose() {
        let input = format!("```\n{TBL}\n這是正文\n```");
        assert!(matches!(
            strip_redundant_table_fence(&input),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strip_keeps_non_table_fence() {
        let input = "```\njust some text\nmore text\n```";
        assert!(matches!(
            strip_redundant_table_fence(input),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strip_keeps_unclosed_fence_midstream() {
        let input = format!("```\n{TBL}");
        assert!(matches!(
            strip_redundant_table_fence(&input),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strip_keeps_table_with_interior_blank_line() {
        let input = "```\n| a | b |\n| - | - |\n\n| 1 | 2 |\n```";
        assert!(matches!(
            strip_redundant_table_fence(input),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strip_keeps_header_delimiter_only_no_data() {
        let input = "```\n| a | b |\n| - | - |\n```";
        assert!(matches!(
            strip_redundant_table_fence(input),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strip_keeps_mismatched_delimiter_cols() {
        let input = "```\n| a | b | c |\n| - | - |\n| 1 | 2 | 3 |\n```";
        assert!(matches!(
            strip_redundant_table_fence(input),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strip_no_fence_is_borrowed() {
        let input = "just a plain reply with no fence";
        assert!(matches!(
            strip_redundant_table_fence(input),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strip_keeps_nested_language_fence_in_body() {
        // body contains a ```rust line → non-table line → keep verbatim.
        let input = "```\n| a | b |\n| - | - |\n```rust\nfn x() {}\n```";
        assert!(matches!(
            strip_redundant_table_fence(input),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    // streaming monotonicity + invariants

    #[test]
    fn strip_streaming_unclosed_then_closed() {
        // While unclosed → no-op (stable); once closed → unwrap.
        let open = "```\n| a | b |\n| - | - |\n| 1 | 2 |";
        assert!(matches!(
            strip_redundant_table_fence(open),
            std::borrow::Cow::Borrowed(_)
        ));
        let closed = format!("{open}\n```");
        assert_eq!(
            strip_redundant_table_fence(&closed).as_ref(),
            "| a | b |\n| - | - |\n| 1 | 2 |"
        );
    }

    #[test]
    fn strip_edge_cases_do_not_panic() {
        for s in [
            "",
            "```",
            "```\n",
            "\n\n",
            "```\n```",
            "| a |\r\n| - |\r\n| 1 |",
        ] {
            let _ = strip_redundant_table_fence(s);
        }
    }

    #[test]
    fn strip_three_exit_consistency() {
        // create (markdown_to_card_v2 true) and finalize (false) unwrap
        // identically; update_card_stream shares the same helper.
        let wrapped = format!("```\n{TBL}\n```");
        let create = markdown_to_card_v2(&wrapped, true);
        let finalize = markdown_to_card_v2(&wrapped, false);
        assert_eq!(create["body"]["elements"][0]["content"], TBL);
        assert_eq!(finalize["body"]["elements"][0]["content"], TBL);
    }

    // --- create_streaming_card ---

    #[tokio::test]
    async fn create_card_success_returns_card_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/cardkit/v1/cards"))
            // Only assert the envelope; the escaped `data` string is covered by
            // the pure card_v2 test above.
            .and(body_partial_json(serde_json::json!({ "type": "card_json" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "msg": "success",
                "data": { "card_id": "7355372766134157313" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let id = create_streaming_card(&client, &server.uri(), "tok", "initial")
            .await
            .expect("should return card_id");
        assert_eq!(id, "7355372766134157313");
    }

    #[tokio::test]
    async fn create_card_errcode_is_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/cardkit/v1/cards"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "code": 300305,
                "msg": "The number of card components exceeds 200"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let out = create_streaming_card(&client, &server.uri(), "tok", "x")
            .await
            .expect_err("should fail");
        assert_eq!(
            out,
            CardOutcome::Failed {
                code: 300305,
                message: "The number of card components exceeds 200".into()
            }
        );
    }

    // --- update_card_stream (#565 guard: FULL content, not delta) ---

    #[tokio::test]
    async fn update_card_stream_sends_full_content_and_sequence() {
        let server = MockServer::start().await;
        let card_id = "7355439197428236291";
        let full = "Hello, world!\nThis is the full cumulative snapshot.";
        // body_json is an EXACT match: if the client ever sent a delta, an
        // extra field, or dropped the sequence, this would fail. This is the
        // openclaw-lark #565 guard at the wire level.
        Mock::given(method("PUT"))
            .and(path(format!(
                "/open-apis/cardkit/v1/cards/{card_id}/elements/md_stream/content"
            )))
            .and(body_json(serde_json::json!({
                "content": full,
                "sequence": 3
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0, "msg": "success", "data": {}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let out = update_card_stream(
            &client,
            &server.uri(),
            "tok",
            card_id,
            STREAM_ELEMENT_ID,
            full,
            3,
        )
        .await;
        assert_eq!(out, CardOutcome::Updated);
    }

    #[tokio::test]
    async fn update_card_stream_rate_limited_429() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(429))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let out = update_card_stream(
            &client,
            &server.uri(),
            "tok",
            "cid",
            STREAM_ELEMENT_ID,
            "text",
            1,
        )
        .await;
        assert_eq!(out, CardOutcome::RateLimited);
    }

    #[tokio::test]
    async fn update_card_stream_sequence_errcode_is_failed() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "code": 300317,
                "msg": "The sequence number for operating on the card did not increment consecutively"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let out = update_card_stream(
            &client,
            &server.uri(),
            "tok",
            "cid",
            STREAM_ELEMENT_ID,
            "text",
            1,
        )
        .await;
        assert!(matches!(out, CardOutcome::Failed { code: 300317, .. }));
    }

    // --- finish_card_stream (full card/update → static card, re-renders tables) ---

    #[tokio::test]
    async fn finish_card_stream_rebuilds_static_card() {
        let server = MockServer::start().await;
        let card_id = "7355439197428236291";
        // Finalize = full card/update (PUT /cards/:id) replacing with a STATIC
        // card. Build the expected escaped data the same way the client does.
        let expected_data = markdown_to_card_v2("final text", false).to_string();
        Mock::given(method("PUT"))
            .and(path(format!("/open-apis/cardkit/v1/cards/{card_id}")))
            .and(body_json(serde_json::json!({
                "card": { "type": "card_json", "data": expected_data },
                "sequence": 9
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0, "msg": "success", "data": {}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let out =
            finish_card_stream(&client, &server.uri(), "tok", card_id, "final text", 9).await;
        assert_eq!(out, CardOutcome::Updated);
    }

    // --- send_card_message (interactive, reply + create) ---

    #[tokio::test]
    async fn send_card_message_reply_in_thread() {
        let server = MockServer::start().await;
        let root = "om_root123";
        // Build the expected content the same way the client does, so the
        // exact-string match is robust to serde_json key ordering.
        let expected_content = serde_json::json!({
            "type": "card",
            "data": { "card_id": "cardabc" }
        })
        .to_string();
        Mock::given(method("POST"))
            .and(path(format!("/open-apis/im/v1/messages/{root}/reply")))
            .and(body_json(serde_json::json!({
                "msg_type": "interactive",
                "content": expected_content
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0, "msg": "success",
                "data": { "message_id": "om_sent789" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let mid =
            send_card_message(&client, &server.uri(), "tok", "oc_chat", Some(root), "cardabc").await;
        assert_eq!(mid.as_deref(), Some("om_sent789"));
    }

    #[tokio::test]
    async fn send_card_message_new_message_to_chat() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/im/v1/messages"))
            .and(body_partial_json(serde_json::json!({
                "receive_id": "oc_chat",
                "msg_type": "interactive"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0, "msg": "success",
                "data": { "message_id": "om_new456" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let mid = send_card_message(&client, &server.uri(), "tok", "oc_chat", None, "cardabc").await;
        assert_eq!(mid.as_deref(), Some("om_new456"));
    }

    // --- streaming session registry (S4) ---

    #[test]
    fn session_next_sequence_strictly_increases() {
        let mut s = FeishuStreamSession::new("card1".into(), "om_msg1".into(), "txt".into());
        assert_eq!(s.sequence, 0);
        assert_eq!(s.next_sequence(), 1);
        assert_eq!(s.next_sequence(), 2);
        assert_eq!(s.next_sequence(), 3);
        assert_eq!(s.sequence, 3);
    }

    #[test]
    fn session_finalize_blocks_idle_candidacy() {
        let mut s = FeishuStreamSession::new("c".into(), "m".into(), "t".into());
        // idle_ms=0 → any non-finalized session is an idle candidate.
        assert!(s.is_idle(0));
        s.mark_finalized();
        assert!(!s.is_idle(0));
    }

    #[test]
    fn session_not_idle_before_window() {
        let s = FeishuStreamSession::new("c".into(), "m".into(), "t".into());
        // Just created → not idle under a 10s window.
        assert!(!s.is_idle(10_000));
    }

    #[test]
    fn registry_promote_get_remove() {
        let mut reg = FeishuStreamRegistry::default();
        assert!(!reg.contains("om_post1"));
        reg.promote("om_post1", "card_a".into(), "om_card_msg".into(), "seed".into());
        assert!(reg.contains("om_post1"));
        let s = reg.get("om_post1").expect("session present");
        assert_eq!(s.card_id, "card_a");
        assert_eq!(s.card_message_id, "om_card_msg");
        assert_eq!(s.sequence, 0);
        assert_eq!(s.last_text, "seed");
        // Mutate the sequence through the registry.
        let seq = reg.get_mut("om_post1").unwrap().next_sequence();
        assert_eq!(seq, 1);
        // Remove.
        let removed = reg.remove("om_post1").expect("removed");
        assert_eq!(removed.card_id, "card_a");
        assert!(!reg.contains("om_post1"));
    }

    #[test]
    fn registry_promote_is_one_way_no_duplicate_order() {
        let mut reg = FeishuStreamRegistry::default();
        reg.promote("om_post1", "card_a".into(), "m1".into(), "t1".into());
        reg.get_mut("om_post1").unwrap().next_sequence(); // seq = 1
        // Re-promoting the same key replaces the session and keeps a single
        // order entry (a fresh session, sequence reset).
        reg.promote("om_post1", "card_b".into(), "m2".into(), "t2".into());
        assert_eq!(reg.len(), 1);
        let s = reg.get("om_post1").unwrap();
        assert_eq!(s.card_id, "card_b");
        assert_eq!(s.sequence, 0);
    }

    #[test]
    fn registry_idle_keys_excludes_finalized() {
        let mut reg = FeishuStreamRegistry::default();
        reg.promote("om_a", "card_a".into(), "m_a".into(), "ta".into());
        reg.promote("om_b", "card_b".into(), "m_b".into(), "tb".into());
        // Both have streamed at least once (sequence > 0).
        reg.get_mut("om_a").unwrap().next_sequence();
        reg.get_mut("om_b").unwrap().next_sequence();
        reg.get_mut("om_b").unwrap().mark_finalized();
        let mut idle = reg.idle_keys(0);
        idle.sort();
        assert_eq!(idle, vec!["om_a".to_string()]);
    }

    #[test]
    fn registry_idle_keys_excludes_unstreamed_session() {
        // A freshly created session at sequence 0 (core still thinking before
        // the first content edit) must NOT be a finalize candidate — otherwise
        // the reaper freezes the placeholder before any content streams in.
        let mut reg = FeishuStreamRegistry::default();
        reg.promote("om_fresh", "c".into(), "m".into(), "...".into());
        assert!(reg.idle_keys(0).is_empty());
        // After the first stream update it becomes a candidate.
        reg.get_mut("om_fresh").unwrap().next_sequence();
        assert_eq!(reg.idle_keys(0), vec!["om_fresh".to_string()]);
    }

    #[test]
    fn registry_fifo_eviction_keeps_recent() {
        let mut reg = FeishuStreamRegistry::default();
        // Insert past the cap; oldest insertions evicted, newest kept.
        let total = STREAM_SESSIONS_MAX + 5;
        for i in 0..total {
            reg.promote(
                &format!("om_{i}"),
                format!("card_{i}"),
                format!("m_{i}"),
                format!("t_{i}"),
            );
        }
        assert!(
            reg.len() <= STREAM_SESSIONS_MAX,
            "len {} should be <= cap {STREAM_SESSIONS_MAX}",
            reg.len()
        );
        // The most recently inserted session must survive.
        assert!(reg.contains(&format!("om_{}", total - 1)));
        // The very first inserted session should have been evicted.
        assert!(!reg.contains("om_0"));
    }
}
