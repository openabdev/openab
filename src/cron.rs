use crate::adapter::{AdapterRouter, ChatAdapter, ChannelRef, SenderContext};
use crate::config::CronJobConfig;
use chrono::{Timelike, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Parse a 5-field POSIX cron expression into a `Schedule`.
/// The `cron` crate expects a 6-field expression (with seconds), so we prepend "0".
pub fn parse_cron_expr(expr: &str) -> Result<Schedule, cron::error::Error> {
    let six_field = format!("0 {}", expr);
    Schedule::from_str(&six_field)
}

/// Check whether a cron schedule should fire right now.
/// Truncates the current time to the minute boundary and checks if the
/// schedule has an event at exactly that minute.
pub fn should_fire(schedule: &Schedule, tz: Tz) -> bool {
    let now = Utc::now().with_timezone(&tz);
    // Truncate to start of current minute
    let minute_start = now
        .with_second(0).unwrap()
        .with_nanosecond(0).unwrap();
    // Query upcoming events from 1 second before the minute boundary.
    // `upcoming()` returns events strictly > the query time, so querying
    // from (minute_start - 1s) will include minute_start itself.
    let query_from = minute_start - chrono::Duration::seconds(1);
    schedule
        .after(&query_from)
        .next()
        .map(|next| next == minute_start)
        .unwrap_or(false)
}

/// Known platforms that have adapter support.
const VALID_PLATFORMS: &[&str] = &["discord", "slack"];

/// Validate all cronjob configs at startup (fail-fast on bad cron expressions or timezones).
/// `configured_platforms` is the set of platforms that have adapters configured (e.g. "discord", "slack").
pub fn validate_cronjobs(cronjobs: &[CronJobConfig], configured_platforms: &[&str]) -> anyhow::Result<()> {
    for (i, job) in cronjobs.iter().enumerate() {
        parse_cron_expr(&job.schedule).map_err(|e| {
            anyhow::anyhow!("cronjobs[{i}]: invalid cron expression {:?}: {e}", job.schedule)
        })?;
        job.timezone.parse::<Tz>().map_err(|e| {
            anyhow::anyhow!("cronjobs[{i}]: invalid timezone {:?}: {e}", job.timezone)
        })?;
        if !VALID_PLATFORMS.contains(&job.platform.as_str()) {
            anyhow::bail!("cronjobs[{i}]: unknown platform {:?} (expected one of: {VALID_PLATFORMS:?})", job.platform);
        }
        if !configured_platforms.contains(&job.platform.as_str()) {
            anyhow::bail!("cronjobs[{i}]: platform {:?} is not configured — add [{}] to config.toml", job.platform, job.platform);
        }
    }
    Ok(())
}

/// Run the internal cron scheduler. Evaluates cron expressions once per minute.
pub async fn run_scheduler(
    cronjobs: Vec<CronJobConfig>,
    router: Arc<AdapterRouter>,
    adapters: HashMap<String, Arc<dyn ChatAdapter>>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    if cronjobs.is_empty() {
        debug!("no cronjobs configured, scheduler not started");
        return;
    }

    // Parse cron expressions into Schedule objects. Already validated at
    // startup by validate_cronjobs(), so errors here are purely defensive.
    let jobs: Vec<(Schedule, Tz, CronJobConfig)> = cronjobs
        .into_iter()
        .filter(|job| {
            if !job.enabled {
                info!(schedule = %job.schedule, channel = %job.channel, "cronjob disabled, skipping");
            }
            job.enabled
        })
        .filter_map(|job| {
            let schedule = match parse_cron_expr(&job.schedule) {
                Ok(s) => s,
                Err(e) => {
                    error!(schedule = %job.schedule, error = %e, "invalid cron expression, skipping");
                    return None;
                }
            };
            let tz: Tz = match job.timezone.parse() {
                Ok(t) => t,
                Err(e) => {
                    error!(timezone = %job.timezone, error = %e, "invalid timezone, skipping");
                    return None;
                }
            };
            info!(
                schedule = %job.schedule,
                timezone = %job.timezone,
                channel = %job.channel,
                platform = %job.platform,
                message = %job.message,
                "cronjob registered"
            );
            Some((schedule, tz, job))
        })
        .collect();

    if jobs.is_empty() {
        warn!("all cronjob expressions invalid, scheduler not started");
        return;
    }

    info!(count = jobs.len(), "cron scheduler started");

    // Track in-flight jobs to prevent overlapping executions
    let in_flight: Arc<Mutex<HashSet<usize>>> = Arc::new(Mutex::new(HashSet::new()));

    // Use interval instead of sleep to compensate for drift.
    // Delay (not Burst) so we skip missed ticks instead of rapid-firing.
    // First, align to the next minute boundary so cron fires at :00, not at
    // whatever second the process happened to start.
    let now = Utc::now();
    let secs_into_minute = now.timestamp() % 60;
    let align_delay = if secs_into_minute == 0 { 0 } else { 60 - secs_into_minute as u64 };
    if align_delay > 0 {
        debug!(align_secs = align_delay, "aligning to next minute boundary");
        tokio::time::sleep(std::time::Duration::from_secs(align_delay)).await;
    }
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Track spawned tasks so we can wait for them on shutdown
    let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                for (idx, (schedule, tz, job)) in jobs.iter().enumerate() {
                    if !should_fire(schedule, *tz) {
                        continue;
                    }
                    // Skip if previous execution still running
                    {
                        let running = in_flight.lock().await;
                        if running.contains(&idx) {
                            warn!(schedule = %job.schedule, channel = %job.channel, "skipping cronjob, previous execution still running");
                            continue;
                        }
                    }
                    info!(
                        schedule = %job.schedule,
                        channel = %job.channel,
                        platform = %job.platform,
                        message = %job.message,
                        sender = %job.sender_name,
                        "🔔 cronjob fired"
                    );
                    in_flight.lock().await.insert(idx);

                    // Spawn the entire fire_cronjob so send_message doesn't block the loop
                    let job = job.clone();
                    let router = router.clone();
                    let adapters = adapters.clone();
                    let in_flight = in_flight.clone();
                    tasks.spawn(async move {
                        fire_cronjob(idx, &job, &router, &adapters, in_flight).await;
                    });
                }
                // Reap completed tasks to avoid unbounded growth
                while tasks.try_join_next().is_some() {}
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("cron scheduler shutting down, waiting for in-flight tasks");
                    // Wait for all in-flight tasks with a timeout
                    let drain = async { while tasks.join_next().await.is_some() {} };
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), drain).await;
                    return;
                }
            }
        }
    }
}

/// RAII guard that removes a job index from the in-flight set on drop.
/// Ensures cleanup even if the task panics or returns early.
struct InFlightGuard {
    idx: usize,
    set: Arc<Mutex<HashSet<usize>>>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let idx = self.idx;
        let set = self.set.clone();
        // spawn a tiny task because Drop is sync and we need an async lock.
        // If the runtime is shutting down this spawn may be ignored, which is
        // fine — the in-flight set is irrelevant once the scheduler exits.
        tokio::spawn(async move {
            set.lock().await.remove(&idx);
        });
    }
}

async fn fire_cronjob(
    idx: usize,
    job: &CronJobConfig,
    router: &Arc<AdapterRouter>,
    adapters: &HashMap<String, Arc<dyn ChatAdapter>>,
    in_flight: Arc<Mutex<HashSet<usize>>>,
) {
    // Guard ensures idx is removed from in_flight even on panic or early return
    let _guard = InFlightGuard { idx, set: in_flight };

    let adapter = match adapters.get(&job.platform) {
        Some(a) => a.clone(),
        None => {
            error!(platform = %job.platform, "no adapter for platform, skipping cronjob");
            return;
        }
    };

    let thread_channel = ChannelRef {
        platform: job.platform.clone(),
        channel_id: job.channel.clone(),
        thread_id: job.thread_id.clone(),
        parent_id: None,
    };

    let sender = SenderContext {
        schema: "openab.sender.v1".into(),
        sender_id: "openab-cron".into(),
        sender_name: job.sender_name.clone(),
        display_name: job.sender_name.clone(),
        channel: job.platform.clone(),
        channel_id: job.channel.clone(),
        thread_id: job.thread_id.clone(),
        is_bot: true,
    };
    let sender_json = match serde_json::to_string(&sender) {
        Ok(j) => j,
        Err(e) => {
            warn!(error = %e, "failed to serialize cron sender context, skipping");
            return;
        }
    };

    // Send visible message first so users see what triggered
    let trigger_msg = match adapter.send_message(&thread_channel, &format!("🕐 [{}]: {}", job.sender_name, job.message)).await {
        Ok(msg) => msg,
        Err(e) => {
            error!(channel = %job.channel, error = %e, "failed to send cron message");
            return;
        }
    };

    // Trigger agent processing
    if let Err(e) = router
        .handle_message(&adapter, &thread_channel, &sender_json, &job.message, vec![], &trigger_msg, false)
        .await
    {
        error!("cron handle_message error: {e}");
        let _ = adapter.send_message(&thread_channel, &format!("⚠️ cronjob error: {e}")).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn parse_valid_cron_expression() {
        let schedule = parse_cron_expr("0 9 * * 1-5").unwrap();
        // Should produce upcoming times
        let next = schedule.upcoming(chrono_tz::UTC).next();
        assert!(next.is_some());
    }

    #[test]
    fn parse_every_minute_cron() {
        let schedule = parse_cron_expr("* * * * *").unwrap();
        let next = schedule.upcoming(chrono_tz::UTC).next();
        assert!(next.is_some());
    }

    #[test]
    fn parse_invalid_cron_expression() {
        let result = parse_cron_expr("not a cron");
        assert!(result.is_err());
    }

    #[test]
    fn parse_invalid_cron_too_many_fields() {
        // 6 fields (user provides seconds) — should fail or behave unexpectedly
        let result = parse_cron_expr("0 0 9 * * 1-5");
        // With our "0 " prefix this becomes 7 fields — should error
        assert!(result.is_err());
    }

    #[test]
    fn valid_timezone_parses() {
        let tz: Result<Tz, _> = "Asia/Taipei".parse();
        assert!(tz.is_ok());
    }

    #[test]
    fn invalid_timezone_fails() {
        let tz: Result<Tz, _> = "Mars/Olympus".parse();
        assert!(tz.is_err());
    }

    #[test]
    fn utc_timezone_parses() {
        let tz: Result<Tz, _> = "UTC".parse();
        assert!(tz.is_ok());
    }

    #[test]
    fn should_fire_every_minute_returns_true() {
        // "* * * * *" fires every minute — current minute always matches
        let schedule = parse_cron_expr("* * * * *").unwrap();
        assert!(should_fire(&schedule, chrono_tz::UTC));
    }

    #[test]
    fn should_fire_returns_false_for_distant_schedule() {
        // Schedule for Jan 1 at 00:00 — unless we happen to be on Jan 1, this won't match
        let schedule = parse_cron_expr("0 0 1 1 *").unwrap();
        // Only passes if today is NOT Jan 1 at 00:xx UTC
        let now = chrono::Utc::now();
        if now.month() != 1 || now.day() != 1 || now.hour() != 0 {
            assert!(!should_fire(&schedule, chrono_tz::UTC));
        }
    }

    #[test]
    fn should_fire_respects_timezone() {
        let schedule = parse_cron_expr("* * * * *").unwrap();
        let tz: Tz = "Asia/Taipei".parse().unwrap();
        // Every-minute schedule should fire regardless of timezone
        assert!(should_fire(&schedule, tz));
    }

    #[test]
    fn cronjob_config_defaults() {
        let toml_str = r#"
[[cronjobs]]
schedule = "0 9 * * 1-5"
channel = "123"
message = "hello"
"#;
        let cfg: CronJobsWrapper = toml::from_str(toml_str).unwrap();
        let job = &cfg.cronjobs[0];
        assert_eq!(job.enabled, true);
        assert_eq!(job.platform, "discord");
        assert_eq!(job.sender_name, "openab-cron");
        assert_eq!(job.timezone, "UTC");
        assert!(job.thread_id.is_none());
    }

    #[test]
    fn cronjob_config_disabled() {
        let toml_str = r#"
[[cronjobs]]
enabled = false
schedule = "0 9 * * 1-5"
channel = "123"
message = "hello"
"#;
        let cfg: CronJobsWrapper = toml::from_str(toml_str).unwrap();
        let job = &cfg.cronjobs[0];
        assert_eq!(job.enabled, false);
    }

    #[test]
    fn cronjob_config_custom_values() {
        let toml_str = r#"
[[cronjobs]]
schedule = "0 18 * * 1-5"
channel = "456"
message = "report"
platform = "slack"
sender_name = "DailyOps"
timezone = "Asia/Taipei"
thread_id = "789"
"#;
        let cfg: CronJobsWrapper = toml::from_str(toml_str).unwrap();
        let job = &cfg.cronjobs[0];
        assert_eq!(job.platform, "slack");
        assert_eq!(job.sender_name, "DailyOps");
        assert_eq!(job.timezone, "Asia/Taipei");
        assert_eq!(job.thread_id.as_deref(), Some("789"));
    }

    /// Helper struct for deserializing just the cronjobs array in tests.
    #[derive(serde::Deserialize)]
    struct CronJobsWrapper {
        cronjobs: Vec<CronJobConfig>,
    }
}
