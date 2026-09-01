//! One-shot `/remind` slash command — schedules a delayed mention in a Discord channel.
//!
//! Persistence: reminders are stored in `reminders.json` and reloaded on startup.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serenity::http::Http;
use serenity::model::id::ChannelId;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// A single pending reminder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    pub channel_id: u64,
    pub sender_id: u64,
    /// Raw mention strings (e.g. "<@123>", "<@&456>")
    pub targets: Vec<String>,
    pub message: String,
    pub fire_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// Shared reminder store with file persistence.
#[derive(Clone)]
pub struct ReminderStore {
    reminders: Arc<Mutex<Vec<Reminder>>>,
    path: PathBuf,
}

impl ReminderStore {
    /// Load or create the reminder store from the given path.
    pub fn load(path: PathBuf) -> Self {
        let reminders = match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
                warn!(error = %e, "failed to parse reminders.json, starting empty");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        info!(count = reminders.len(), path = %path.display(), "loaded reminders");
        Self {
            reminders: Arc::new(Mutex::new(reminders)),
            path,
        }
    }

    /// Add a reminder and persist to disk.
    pub async fn add(&self, reminder: Reminder) {
        let snapshot = {
            let mut reminders = self.reminders.lock().await;
            reminders.push(reminder);
            reminders.clone()
        };
        self.persist(&snapshot);
    }

    /// Remove a reminder by ID and persist.
    pub async fn remove(&self, id: &str) {
        let snapshot = {
            let mut reminders = self.reminders.lock().await;
            reminders.retain(|r| r.id != id);
            reminders.clone()
        };
        self.persist(&snapshot);
    }

    /// Get all pending reminders (for startup re-scheduling).
    pub async fn pending(&self) -> Vec<Reminder> {
        self.reminders.lock().await.clone()
    }

    fn persist(&self, reminders: &[Reminder]) {
        match serde_json::to_string_pretty(reminders) {
            Ok(data) => {
                if let Some(parent) = self.path.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        error!(error = %e, "failed to create reminders directory");
                        return;
                    }
                }
                if let Err(e) = std::fs::write(&self.path, data) {
                    error!(error = %e, "failed to persist reminders.json");
                }
            }
            Err(e) => {
                error!(error = %e, "failed to serialize reminders, skipping persist");
            }
        }
    }
}

/// Maximum allowed message length for reminders.
pub const MAX_MESSAGE_LEN: usize = 1800;

/// Maximum number of mention targets per reminder.
pub const MAX_TARGETS: usize = 10;

/// Sanitize reminder message: neutralize @everyone/@here.
pub fn sanitize_message(msg: &str) -> String {
    msg.replace("@everyone", "@\u{200b}everyone")
        .replace("@here", "@\u{200b}here")
}

/// Validate reminder message length.
pub fn validate_message(msg: &str) -> Result<(), String> {
    if msg.len() > MAX_MESSAGE_LEN {
        Err(format!(
            "message too long (max {MAX_MESSAGE_LEN} characters)"
        ))
    } else {
        Ok(())
    }
}

/// Parse a human delay string like "30m", "2h", "7d" into seconds.
/// Supports combinations: "1h30m", "2d12h".
/// Range: 1m (60s) to 30d (2_592_000s).
pub fn parse_delay(input: &str) -> Result<u64, String> {
    let s = input.trim().to_lowercase();
    if s.is_empty() {
        return Err("empty delay".into());
    }

    let mut total_secs: u64 = 0;
    let mut num_buf = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num_buf.push(ch);
        } else {
            let n: u64 = num_buf
                .parse()
                .map_err(|_| format!("invalid number in delay: {input}"))?;
            num_buf.clear();
            let multiplier = match ch {
                'm' => 60,
                'h' => 3600,
                'd' => 86400,
                _ => return Err(format!("unknown unit '{ch}' in delay (use m/h/d)")),
            };
            total_secs += n * multiplier;
        }
    }

    // Handle bare number (default to minutes)
    if !num_buf.is_empty() {
        let n: u64 = num_buf
            .parse()
            .map_err(|_| format!("invalid number in delay: {input}"))?;
        total_secs += n * 60; // default unit = minutes
    }

    if total_secs < 60 {
        return Err("minimum delay is 1m".into());
    }
    if total_secs > 2_592_000 {
        return Err("maximum delay is 30d".into());
    }

    Ok(total_secs)
}

/// Format seconds into a human-readable string like "2h 30m".
pub fn format_delay(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(format!("{d}d"));
    }
    if h > 0 {
        parts.push(format!("{h}h"));
    }
    if m > 0 {
        parts.push(format!("{m}m"));
    }
    if parts.is_empty() {
        "< 1m".into()
    } else {
        parts.join(" ")
    }
}

/// Spawn a tokio task that fires the reminder after the delay.
pub fn schedule_reminder(http: Arc<Http>, store: ReminderStore, reminder: Reminder) {
    let now = Utc::now();
    let delay = if reminder.fire_at > now {
        (reminder.fire_at - now).to_std().unwrap_or_default()
    } else {
        std::time::Duration::ZERO
    };

    let id = reminder.id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;

        let targets_str = reminder.targets.join(" ");
        let content = format!(
            "⏰ **Reminder** from <@{}>:\n\"{}\"\ncc {}",
            reminder.sender_id, reminder.message, targets_str
        );

        let channel = ChannelId::new(reminder.channel_id);
        match channel.say(&http, &content).await {
            Ok(_) => {
                info!(id = %id, channel = reminder.channel_id, "reminder fired");
                store.remove(&id).await;
            }
            Err(e) => {
                error!(error = %e, id = %id, "failed to send reminder — keeping for retry on next restart");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_delay_minutes() {
        assert_eq!(parse_delay("5m").unwrap(), 300);
        assert_eq!(parse_delay("1m").unwrap(), 60);
    }

    #[test]
    fn parse_delay_hours() {
        assert_eq!(parse_delay("2h").unwrap(), 7200);
    }

    #[test]
    fn parse_delay_days() {
        assert_eq!(parse_delay("1d").unwrap(), 86400);
        assert_eq!(parse_delay("30d").unwrap(), 2_592_000);
    }

    #[test]
    fn parse_delay_combined() {
        assert_eq!(parse_delay("1h30m").unwrap(), 5400);
        assert_eq!(parse_delay("1d12h").unwrap(), 129_600);
    }

    #[test]
    fn parse_delay_bare_number_defaults_to_minutes() {
        assert_eq!(parse_delay("10").unwrap(), 600);
    }

    #[test]
    fn parse_delay_too_short() {
        assert!(parse_delay("0m").is_err());
        assert!(parse_delay("0h").is_err());
    }

    #[test]
    fn parse_delay_too_long() {
        assert!(parse_delay("31d").is_err());
    }

    #[test]
    fn format_delay_basic() {
        assert_eq!(format_delay(3600), "1h");
        assert_eq!(format_delay(5400), "1h 30m");
        assert_eq!(format_delay(90000), "1d 1h");
    }

    #[test]
    fn parse_delay_empty() {
        assert!(parse_delay("").is_err());
        assert!(parse_delay("   ").is_err());
    }

    #[test]
    fn parse_delay_invalid_unit() {
        assert!(parse_delay("2x").is_err());
        assert!(parse_delay("abc").is_err());
        assert!(parse_delay("5s").is_err());
    }

    #[test]
    fn parse_delay_case_insensitive() {
        assert_eq!(parse_delay("2H").unwrap(), 7200);
        assert_eq!(parse_delay("1D30M").unwrap(), 88200);
    }

    #[test]
    fn parse_delay_whitespace_trimmed() {
        assert_eq!(parse_delay(" 5m ").unwrap(), 300);
    }

    #[test]
    fn parse_delay_bare_number_boundary() {
        assert_eq!(parse_delay("1").unwrap(), 60); // 1 min
        assert_eq!(parse_delay("30").unwrap(), 1800); // 30 min
    }

    #[test]
    fn parse_delay_exact_boundaries() {
        // Exactly 1m (minimum)
        assert_eq!(parse_delay("1m").unwrap(), 60);
        // Exactly 30d (maximum)
        assert_eq!(parse_delay("30d").unwrap(), 2_592_000);
        // Just over 30d
        assert!(parse_delay("30d1m").is_err());
    }

    #[test]
    fn format_delay_zero() {
        assert_eq!(format_delay(0), "< 1m");
    }

    #[test]
    fn format_delay_pure_units() {
        assert_eq!(format_delay(86400), "1d");
        assert_eq!(format_delay(120), "2m");
        assert_eq!(format_delay(7200), "2h");
    }

    #[tokio::test]
    async fn reminder_store_add_remove() {
        let dir = std::env::temp_dir().join(format!("remind_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reminders.json");

        let store = ReminderStore::load(path.clone());
        assert_eq!(store.pending().await.len(), 0);

        let r = Reminder {
            id: "test-1".into(),
            channel_id: 123,
            sender_id: 456,
            targets: vec!["<@789>".into()],
            message: "hello".into(),
            fire_at: Utc::now() + chrono::Duration::hours(1),
            created_at: Utc::now(),
        };

        store.add(r).await;
        assert_eq!(store.pending().await.len(), 1);

        store.remove("test-1").await;
        assert_eq!(store.pending().await.len(), 0);

        // Verify persistence
        let store2 = ReminderStore::load(path.clone());
        assert_eq!(store2.pending().await.len(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn reminder_store_persists_across_reload() {
        let dir = std::env::temp_dir().join(format!("remind_test2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reminders.json");

        let store = ReminderStore::load(path.clone());
        let r = Reminder {
            id: "persist-1".into(),
            channel_id: 100,
            sender_id: 200,
            targets: vec!["<@300>".into()],
            message: "persist test".into(),
            fire_at: Utc::now() + chrono::Duration::hours(2),
            created_at: Utc::now(),
        };
        store.add(r).await;

        // Reload from disk
        let store2 = ReminderStore::load(path.clone());
        let pending = store2.pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "persist-1");
        assert_eq!(pending[0].message, "persist test");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sanitize_message_strips_everyone_here() {
        assert_eq!(
            sanitize_message("hello @everyone"),
            "hello @\u{200b}everyone"
        );
        assert_eq!(
            sanitize_message("hey @here check"),
            "hey @\u{200b}here check"
        );
        assert_eq!(
            sanitize_message("@everyone @here"),
            "@\u{200b}everyone @\u{200b}here"
        );
    }

    #[test]
    fn sanitize_message_no_change() {
        assert_eq!(sanitize_message("normal message"), "normal message");
        assert_eq!(sanitize_message("<@123> hello"), "<@123> hello");
    }

    #[test]
    fn validate_message_ok() {
        assert!(validate_message("short message").is_ok());
        assert!(validate_message(&"a".repeat(1800)).is_ok());
    }

    #[test]
    fn validate_message_too_long() {
        assert!(validate_message(&"a".repeat(1801)).is_err());
    }

    #[test]
    fn max_targets_constant() {
        assert_eq!(MAX_TARGETS, 10);
    }
}
