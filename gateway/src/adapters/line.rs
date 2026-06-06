use crate::schema::*;
use axum::extract::State;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info, warn};

// --- LINE types ---

#[derive(Debug, Deserialize)]
pub struct LineWebhookBody {
    events: Vec<LineEvent>,
}

#[derive(Debug, Deserialize)]
struct LineEvent {
    #[serde(rename = "type")]
    event_type: String,
    source: Option<LineSource>,
    message: Option<LineMessage>,
    #[serde(rename = "replyToken")]
    reply_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LineSource {
    #[serde(rename = "type")]
    source_type: String,
    #[serde(rename = "userId")]
    user_id: Option<String>,
    #[serde(rename = "groupId")]
    group_id: Option<String>,
    #[serde(rename = "roomId")]
    room_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LineMessage {
    id: String,
    #[serde(rename = "type")]
    message_type: String,
    text: Option<String>,
}

/// Base URL for LINE Messaging API. Overridden in tests via the `api_base` parameter.
pub const LINE_API_BASE: &str = "https://api.line.me";

/// Decide whether to silently drop a LINE message based on group-mention gating.
///
/// LINE webhooks don't expose mention entities, so the gateway gates on a configurable
/// substring keyword. 1:1 ("user") messages are never gated. When no keyword is configured,
/// nothing is gated (legacy behavior).
fn should_drop_group_message(channel_type: &str, text: &str, keyword: Option<&str>) -> bool {
    if channel_type == "user" {
        return false;
    }
    match keyword {
        Some(kw) if !kw.is_empty() => !text.contains(kw),
        _ => false,
    }
}

// --- Webhook handler ---

pub async fn webhook(
    State(state): State<Arc<crate::AppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::http::StatusCode {
    // Validate X-Line-Signature
    if let Some(ref channel_secret) = state.line_channel_secret {
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let signature = headers
            .get("x-line-signature")
            .and_then(|v| v.to_str().ok());
        let Some(signature) = signature else {
            warn!("LINE webhook rejected: missing X-Line-Signature");
            return axum::http::StatusCode::UNAUTHORIZED;
        };

        let mut mac = Hmac::<Sha256>::new_from_slice(channel_secret.as_bytes()).expect("HMAC key");
        mac.update(&body);
        let expected =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        if signature != expected {
            warn!("LINE webhook rejected: invalid signature");
            return axum::http::StatusCode::UNAUTHORIZED;
        }
    }

    let webhook_body: LineWebhookBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            warn!("LINE webhook parse error: {e}");
            return axum::http::StatusCode::BAD_REQUEST;
        }
    };

    for event in webhook_body.events {
        if event.event_type != "message" {
            continue;
        }
        let Some(ref msg) = event.message else {
            continue;
        };
        if msg.message_type != "text" {
            continue;
        }
        let Some(ref text) = msg.text else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }

        let source = event.source.as_ref();
        let (channel_id, channel_type) = match source {
            Some(s) if s.source_type == "group" => {
                match s.group_id.as_deref() {
                    Some(id) if !id.is_empty() => (id.to_string(), "group".to_string()),
                    _ => {
                        warn!("LINE group event missing groupId, skipping");
                        continue;
                    }
                }
            }
            Some(s) if s.source_type == "room" => {
                match s.room_id.as_deref() {
                    Some(id) if !id.is_empty() => (id.to_string(), "room".to_string()),
                    _ => {
                        warn!("LINE room event missing roomId, skipping");
                        continue;
                    }
                }
            }
            Some(s) => {
                match s.user_id.as_deref() {
                    Some(id) if !id.is_empty() => (id.to_string(), "user".to_string()),
                    _ => {
                        warn!("LINE user event missing userId, skipping");
                        continue;
                    }
                }
            }
            None => {
                warn!("LINE event missing source, skipping");
                continue;
            }
        };
        if should_drop_group_message(
            &channel_type,
            text,
            state.line_group_mention_keyword.as_deref(),
        ) {
            info!(
                channel = %channel_id,
                keyword = ?state.line_group_mention_keyword.as_deref(),
                "line group message dropped (mention keyword not found in text)"
            );
            continue;
        }

        let user_id = source
            .and_then(|s| s.user_id.as_deref())
            .unwrap_or("unknown");

        let gateway_event = GatewayEvent::new(
            "line",
            ChannelInfo {
                id: channel_id.clone(),
                channel_type,
                thread_id: None,
            },
            SenderInfo {
                id: user_id.into(),
                name: user_id.into(),
                display_name: user_id.into(),
                is_bot: false,
            },
            text,
            &msg.id,
            vec![],
        );

        // Cache the reply token for hybrid Reply/Push dispatch
        if let Some(ref reply_token) = event.reply_token {
            let mut cache = state
                .reply_token_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cache.len() >= crate::REPLY_TOKEN_CACHE_MAX {
                warn!(
                    size = cache.len(),
                    "reply token cache full, skipping insert"
                );
            } else {
                cache.insert(
                    gateway_event.event_id.clone(),
                    (reply_token.clone(), std::time::Instant::now()),
                );
                info!(event_id = %gateway_event.event_id, "cached LINE replyToken");
            }
        }

        let json = serde_json::to_string(&gateway_event).unwrap();
        info!(channel = %channel_id, sender = %user_id, "line → gateway");
        let _ = state.event_tx.send(json);
    }

    axum::http::StatusCode::OK
}

// --- Reply handler (hybrid Reply/Push dispatch) ---

/// Dispatch a reply to LINE using the hybrid Reply/Push strategy.
///
/// Returns `true` if Reply API was used (or assumed used), `false` if Push API was used.
pub async fn dispatch_line_reply(
    client: &reqwest::Client,
    access_token: &str,
    reply_cache: &crate::ReplyTokenCache,
    reply: &GatewayReply,
    api_base: &str,
) -> bool {
    if matches!(
        reply.command.as_deref(),
        Some("add_reaction") | Some("remove_reaction") | Some("create_topic")
    ) {
        info!(command = ?reply.command.as_deref(), "line: ignoring unsupported command");
        return false;
    }

    // Extract token from cache (drop lock before HTTP call)
    let cached_token = {
        let mut cache = reply_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .remove(&reply.reply_to)
            .and_then(|(token, cached_at)| {
                if cached_at.elapsed().as_secs() < crate::REPLY_TOKEN_TTL_SECS {
                    Some(token)
                } else {
                    info!("LINE replyToken expired, using Push API");
                    None
                }
            })
    };

    // Try Reply API first (free, no quota consumed)
    let mut used_reply = false;
    if let Some(reply_token) = cached_token {
        info!(to = %reply.channel.id, "gateway → line (reply API)");
        let resp = client
            .post(format!("{}/v2/bot/message/reply", api_base))
            .bearer_auth(access_token)
            .json(&serde_json::json!({
                "replyToken": reply_token,
                "messages": [{"type": "text", "text": reply.content.text}]
            }))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                used_reply = true;
            }
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                let body_lower = body.to_lowercase();
                let token_unusable = status.as_u16() == 400
                    && ((body_lower.contains("invalid") && body_lower.contains("reply token"))
                        || body_lower.contains("expired"));
                if token_unusable {
                    warn!(status = %status, body = %body, "LINE reply token unusable, falling back to Push");
                } else {
                    error!(status = %status, body = %body, "LINE Reply API error, NOT falling back to Push (possible duplicate risk)");
                    used_reply = true;
                }
            }
            Err(e) => {
                error!(err = %e, "LINE Reply API network error, NOT falling back to Push (possible duplicate risk)");
                used_reply = true;
            }
        }
    }

    // Fallback to Push API
    if !used_reply {
        info!(to = %reply.channel.id, "gateway → line (push API)");
        let _ = client
            .post(format!("{}/v2/bot/message/push", api_base))
            .bearer_auth(access_token)
            .json(&serde_json::json!({
                "to": reply.channel.id,
                "messages": [{"type": "text", "text": reply.content.text}]
            }))
            .send()
            .await
            .map_err(|e| error!("line push error: {e}"));
    }

    used_reply
}

#[cfg(test)]
mod tests {
    use super::should_drop_group_message;

    #[test]
    fn user_1on1_never_dropped_even_without_keyword() {
        assert!(!should_drop_group_message("user", "hello", Some("@Go")));
        assert!(!should_drop_group_message("user", "hello", None));
    }

    #[test]
    fn group_without_keyword_config_is_not_dropped() {
        assert!(!should_drop_group_message("group", "anything", None));
        assert!(!should_drop_group_message("room", "anything", None));
    }

    #[test]
    fn group_with_keyword_drops_when_missing() {
        assert!(should_drop_group_message("group", "weather is great", Some("@Go醬")));
        assert!(should_drop_group_message("room",  "lunch?",            Some("@Go醬")));
    }

    #[test]
    fn group_with_keyword_passes_when_present() {
        assert!(!should_drop_group_message("group", "@Go醬 hi",         Some("@Go醬")));
        assert!(!should_drop_group_message("group", "hey @Go醬, plan?", Some("@Go醬")));
        assert!(!should_drop_group_message("room",  "@Go醬",            Some("@Go醬")));
    }

    #[test]
    fn empty_keyword_treated_as_unset() {
        assert!(!should_drop_group_message("group", "anything", Some("")));
    }

    #[test]
    fn keyword_match_is_case_and_substring_sensitive() {
        // Substring match — partial keyword inside a longer token still counts.
        assert!(!should_drop_group_message("group", "x@Go醬y", Some("@Go醬")));
        // Case-sensitive — different casing is treated as no match.
        assert!(should_drop_group_message("group", "@go醬 hi", Some("@Go醬")));
    }
}
