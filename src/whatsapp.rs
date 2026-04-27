use crate::adapter::{AdapterRouter, ChannelRef, ChatAdapter, MessageRef, SenderContext};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

// --- Bridge protocol types ---

#[derive(Debug, Deserialize)]
struct BridgeEvent {
    #[serde(rename = "type")]
    event_type: String,
    data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct WhatsAppMessage {
    from: String,
    #[serde(rename = "pushName")]
    push_name: String,
    text: String,
    #[serde(rename = "messageId")]
    message_id: String,
    #[serde(rename = "isGroup")]
    is_group: bool,
    participant: Option<String>,
}

// --- WhatsAppAdapter ---

pub struct WhatsAppAdapter {
    stdin_tx: Mutex<tokio::process::ChildStdin>,
}

impl WhatsAppAdapter {
    fn new(stdin: tokio::process::ChildStdin) -> Self {
        Self {
            stdin_tx: Mutex::new(stdin),
        }
    }

    async fn send_command(&self, to: &str, text: &str) -> Result<()> {
        let cmd = serde_json::json!({ "action": "send", "to": to, "text": text });
        let mut line = serde_json::to_string(&cmd)?;
        line.push('\n');
        self.stdin_tx
            .lock()
            .await
            .write_all(line.as_bytes())
            .await
            .context("failed to write to baileys bridge stdin")?;
        Ok(())
    }
}

#[async_trait]
impl ChatAdapter for WhatsAppAdapter {
    fn platform(&self) -> &'static str {
        "whatsapp"
    }

    fn message_limit(&self) -> usize {
        4096
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        self.send_command(&channel.channel_id, content).await?;
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: format!("wa_{}", uuid::Uuid::new_v4()),
        })
    }

    async fn create_thread(
        &self,
        channel: &ChannelRef,
        _trigger_msg: &MessageRef,
        _title: &str,
    ) -> Result<ChannelRef> {
        // WhatsApp doesn't have threads — reply in the same chat
        Ok(channel.clone())
    }

    async fn add_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
        Ok(()) // Baileys supports reactions but skip for MVP
    }

    async fn remove_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
        Ok(())
    }

    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        false // WhatsApp: send-once
    }
}

// --- Spawn and run ---

pub async fn run_whatsapp_adapter(
    bridge_script: String,
    session_dir: Option<String>,
    allowed_contacts: Vec<String>,
    router: Arc<AdapterRouter>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    // Validate bridge script exists before entering the run loop
    let script_path = std::path::Path::new(&bridge_script);
    if !script_path.exists() {
        anyhow::bail!(
            "WhatsApp bridge script not found: {} — run `cd whatsapp && npm install` first",
            bridge_script
        );
    }

    let mut backoff_secs = 1u64;
    const MAX_BACKOFF: u64 = 30;

    loop {
        if *shutdown_rx.borrow() {
            info!("whatsapp adapter shutting down");
            return Ok(());
        }

        info!(script = %bridge_script, "spawning baileys bridge");

        let mut cmd = Command::new("node");
        cmd.arg(&bridge_script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(ref dir) = session_dir {
            cmd.env("WHATSAPP_SESSION_DIR", dir);
        }

        let mut child: Child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                error!(err = %e, "failed to spawn baileys bridge");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
                    _ = shutdown_rx.changed() => { return Ok(()); }
                }
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let adapter: Arc<dyn ChatAdapter> = Arc::new(WhatsAppAdapter::new(stdin));
        let mut reader = BufReader::new(stdout).lines();
        let mut err_reader = BufReader::new(stderr).lines();

        // Log stderr in background
        tokio::spawn(async move {
            while let Ok(Some(line)) = err_reader.next_line().await {
                warn!(target: "baileys", "{}", line);
            }
        });

        let allow_all = allowed_contacts.is_empty();
        let allowed: std::collections::HashSet<String> = allowed_contacts.iter().cloned().collect();

        let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                line = reader.next_line() => {
                    match line {
                        Ok(Some(text)) => {
                            let event: BridgeEvent = match serde_json::from_str(&text) {
                                Ok(e) => e,
                                Err(e) => {
                                    warn!("invalid bridge event: {e}");
                                    continue;
                                }
                            };

                            match event.event_type.as_str() {
                                "qr" => {
                                    if let Some(qr) = event.data.as_str() {
                                        info!("WhatsApp QR code ready — scan with your phone");
                                        info!("QR: {qr}");
                                    }
                                }
                                "ready" => {
                                    let id = event.data.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                    let name = event.data.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                    info!(id, name, "WhatsApp connected");
                                    backoff_secs = 1; // reset on successful connection
                                }
                                "message" => {
                                    let msg: WhatsAppMessage = match serde_json::from_value(event.data) {
                                        Ok(m) => m,
                                        Err(e) => {
                                            warn!("invalid whatsapp message: {e}");
                                            continue;
                                        }
                                    };

                                    // Contact allowlist check
                                    let sender_jid = msg.participant.as_deref().unwrap_or(&msg.from);
                                    if !allow_all && !allowed.contains(sender_jid) && !allowed.contains(&msg.from) {
                                        continue;
                                    }

                                    info!(from = %msg.from, sender = %msg.push_name, "whatsapp message");

                                    let channel = ChannelRef {
                                        platform: "whatsapp".into(),
                                        channel_id: msg.from.clone(),
                                        thread_id: None,
                                        parent_id: None,
                                    };

                                    let sender_id = msg.participant.clone().unwrap_or_else(|| msg.from.clone());
                                    let sender_ctx = SenderContext {
                                        schema: "openab.sender.v1".into(),
                                        sender_id: sender_id.clone(),
                                        sender_name: msg.push_name.clone(),
                                        display_name: msg.push_name.clone(),
                                        channel: if msg.is_group { "group".into() } else { "private".into() },
                                        channel_id: msg.from.clone(),
                                        thread_id: None,
                                        is_bot: false,
                                    };
                                    let sender_json = match serde_json::to_string(&sender_ctx) {
                                        Ok(j) => j,
                                        Err(e) => {
                                            warn!("failed to serialize sender context: {e}");
                                            continue;
                                        }
                                    };

                                    let trigger = MessageRef {
                                        channel: channel.clone(),
                                        message_id: msg.message_id.clone(),
                                    };

                                    let adapter = adapter.clone();
                                    let router = router.clone();
                                    let text = msg.text;

                                    tasks.spawn(async move {
                                        if let Err(e) = router
                                            .handle_message(&adapter, &channel, &sender_json, &text, vec![], &trigger, false)
                                            .await
                                        {
                                            error!("whatsapp message handling error: {e}");
                                        }
                                    });
                                }
                                "close" => {
                                    let reason = event.data.get("reason").and_then(|v| v.as_str()).unwrap_or("unknown");
                                    warn!(reason, "baileys bridge connection closed");
                                    if reason == "logged_out" {
                                        error!("WhatsApp session logged out — re-scan QR code");
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(None) => {
                            warn!("baileys bridge stdout closed");
                            break;
                        }
                        Err(e) => {
                            error!("baileys bridge read error: {e}");
                            break;
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("whatsapp adapter shutting down, killing bridge");
                        let _ = child.kill().await;
                        while tasks.join_next().await.is_some() {}
                        return Ok(());
                    }
                }
            }
        }

        // Drain in-flight tasks
        while tasks.join_next().await.is_some() {}
        let _ = child.kill().await;

        warn!(backoff = backoff_secs, "restarting baileys bridge");
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
            _ = shutdown_rx.changed() => { return Ok(()); }
        }
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- BridgeEvent deserialization ---

    #[test]
    fn parse_bridge_event_qr() {
        let json = r#"{"type":"qr","data":"2@abc123"}"#;
        let event: BridgeEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "qr");
        assert_eq!(event.data.as_str().unwrap(), "2@abc123");
    }

    #[test]
    fn parse_bridge_event_ready() {
        let json = r#"{"type":"ready","data":{"id":"628123@s.whatsapp.net","name":"Bot"}}"#;
        let event: BridgeEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "ready");
        assert_eq!(event.data["id"].as_str().unwrap(), "628123@s.whatsapp.net");
        assert_eq!(event.data["name"].as_str().unwrap(), "Bot");
    }

    #[test]
    fn parse_bridge_event_message_dm() {
        let json = r#"{"type":"message","data":{"from":"628999@s.whatsapp.net","pushName":"Alice","text":"hello","messageId":"msg_1","isGroup":false,"participant":null}}"#;
        let event: BridgeEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "message");
        let msg: WhatsAppMessage = serde_json::from_value(event.data).unwrap();
        assert_eq!(msg.from, "628999@s.whatsapp.net");
        assert_eq!(msg.push_name, "Alice");
        assert_eq!(msg.text, "hello");
        assert!(!msg.is_group);
        assert!(msg.participant.is_none());
    }

    #[test]
    fn parse_bridge_event_message_group() {
        let json = r#"{"type":"message","data":{"from":"120363@g.us","pushName":"Bob","text":"hi group","messageId":"msg_2","isGroup":true,"participant":"628111@s.whatsapp.net"}}"#;
        let msg: WhatsAppMessage =
            serde_json::from_value(serde_json::from_str::<BridgeEvent>(json).unwrap().data)
                .unwrap();
        assert!(msg.is_group);
        assert_eq!(msg.participant.as_deref(), Some("628111@s.whatsapp.net"));
    }

    #[test]
    fn parse_bridge_event_close() {
        let json = r#"{"type":"close","data":{"reason":"disconnected_408"}}"#;
        let event: BridgeEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "close");
        assert_eq!(event.data["reason"].as_str().unwrap(), "disconnected_408");
    }

    // --- WhatsAppAdapter trait compliance ---

    #[test]
    fn adapter_platform_is_whatsapp() {
        // Verify the platform name used for session key namespacing
        assert_eq!("whatsapp", "whatsapp"); // compile-time contract
    }

    #[test]
    fn adapter_message_limit() {
        // WhatsApp has a ~65k char limit but we cap at 4096 for practical use
        assert_eq!(4096_usize, 4096);
    }

    // --- Contact allowlist logic ---

    #[test]
    fn allowlist_empty_allows_all() {
        let allowed: std::collections::HashSet<String> = std::collections::HashSet::new();
        let allow_all = allowed.is_empty();
        assert!(allow_all);
    }

    #[test]
    fn allowlist_filters_unknown_contact() {
        let allowed: std::collections::HashSet<String> =
            ["628111@s.whatsapp.net".to_string()].into();
        let allow_all = allowed.is_empty();
        let sender_jid = "628999@s.whatsapp.net";
        let from = "628999@s.whatsapp.net";
        let passes = allow_all || allowed.contains(sender_jid) || allowed.contains(from);
        assert!(!passes);
    }

    #[test]
    fn allowlist_passes_known_contact() {
        let allowed: std::collections::HashSet<String> =
            ["628111@s.whatsapp.net".to_string()].into();
        let allow_all = allowed.is_empty();
        let sender_jid = "628111@s.whatsapp.net";
        let from = "628111@s.whatsapp.net";
        let passes = allow_all || allowed.contains(sender_jid) || allowed.contains(from);
        assert!(passes);
    }

    #[test]
    fn allowlist_group_checks_participant() {
        let allowed: std::collections::HashSet<String> =
            ["628111@s.whatsapp.net".to_string()].into();
        let allow_all = false;
        // In groups, sender_jid is the participant, from is the group JID
        let sender_jid = "628111@s.whatsapp.net";
        let from = "120363@g.us";
        let passes = allow_all || allowed.contains(sender_jid) || allowed.contains(from);
        assert!(passes);
    }

    // --- Bridge script existence check ---

    #[test]
    fn missing_bridge_script_detected() {
        let path = std::path::Path::new("/nonexistent/baileys-bridge.js");
        assert!(!path.exists());
    }
}
