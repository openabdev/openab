use crate::adapter::{AdapterRouter, ChatAdapter, ChannelRef, SenderContext};
use crate::config::CronJobConfig;
use crate::format;
use chrono::{Timelike, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::SystemTime;
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
    let minute_start = now
        .with_second(0).unwrap()
        .with_nanosecond(0).unwrap();
    let query_from = minute_start - chrono::Duration::seconds(1);
    schedule
        .after(&query_from)
        .next()
        .map(|next| next == minute_start)
        .unwrap_or(false)
}

/// Known platforms that have adapter support.
const VALID_PLATFORMS: &[&str] = &["discord", "slack"];

/// Validate all cronjob configs (fail-fast on bad cron expressions or timezones).
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

// ---------------------------------------------------------------------------
// Usercron hot-reload
// ---------------------------------------------------------------------------

/// Wrapper for deserializing cronjob.toml which contains `[[cronjobs]]`.
#[derive(serde::Deserialize)]
struct UsercronFile {
    #[serde(default)]
    cronjobs: Vec<CronJobConfig>,
}

/// Load and validate cronjobs from an external TOML file.
/// Returns an empty vec if the file doesn't exist.
/// Logs and skips individual invalid entries rather than failing entirely.
pub fn load_usercron_file(path: &Path, configured_platforms: &[&str]) -> Vec<CronJobConfig> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return vec![],
        Err(e) => {
            warn!(path = %path.display(), error = %e, "failed to read usercron file");
            return vec![];
        }
    };
    let parsed: UsercronFile = match toml::from_str(&content) {
        Ok(f) => f,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "failed to parse usercron file, skipping all entries");
            return vec![];
        }
    };
    // Validate each entry individually — keep valid ones, skip bad ones
    parsed.cronjobs.into_iter().enumerate().filter(|(i, job)| {
        if let Err(e) = parse_cron_expr(&job.schedule) {
            warn!(index = i, schedule = %job.schedule, error = %e, "usercron: invalid cron expression, skipping");
            return false;
        }
        if job.timezone.parse::<Tz>().is_err() {
            warn!(index = i, timezone = %job.timezone, "usercron: invalid timezone, skipping");
            return false;
        }
        if !VALID_PLATFORMS.contains(&job.platform.as_str()) {
            warn!(index = i, platform = %job.platform, "usercron: unknown platform, skipping");
            return false;
        }
        if !configured_platforms.contains(&job.platform.as_str()) {
            warn!(index = i, platform = %job.platform, "usercron: platform not configured, skipping");
            return false;
        }
        true
    }).map(|(_, job)| job).collect()
}

/// Get file mtime, returns None if file doesn't exist or metadata fails.
fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// A parsed, ready-to-evaluate cron job.
struct ParsedJob {
    schedule: Schedule,
    tz: Tz,
    config: CronJobConfig,
}

/// Parse a list of CronJobConfig into ParsedJob, filtering out disabled/invalid entries.
fn parse_job_list(configs: &[CronJobConfig], source: &str) -> Vec<ParsedJob> {
    configs.iter().filter(|job| {
        if !job.enabled {
            info!(schedule = %job.schedule, channel = %job.channel, source, "cronjob disabled, skipping");
        }
        job.enabled
    }).filter_map(|job| {
        let schedule = match parse_cron_expr(&job.schedule) {
            Ok(s) => s,
            Err(e) => {
                error!(schedule = %job.schedule, error = %e, source, "invalid cron expression, skipping");
                return None;
            }
        };
        let tz: Tz = match job.timezone.parse() {
            Ok(t) => t,
            Err(e) => {
                error!(timezone = %job.timezone, error = %e, source, "invalid timezone, skipping");
                return None;
            }
        };
        info!(
            schedule = %job.schedule, timezone = %job.timezone,
            channel = %job.channel, platform = %job.platform,
            message = %job.message, source,
            "cronjob registered"
        );
        Some(ParsedJob { schedule, tz, config: job.clone() })
    }).collect()
}

/// Run the internal cron scheduler. Evaluates cron expressions once per minute.
/// `usercron_path` enables hot-reload of an external cronjob.toml file.
pub async fn run_scheduler(
    cronjobs: Vec<CronJobConfig>,
    usercron_path: Option<PathBuf>,
    configured_platforms: Vec<String>,
    router: Arc<AdapterRouter>,
    adapters: HashMap<String, Arc<dyn ChatAdapter>>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let platform_refs: Vec<&str> = configured_platforms.iter().map(|s| s.as_str()).collect();

    // Parse baseline jobs from config.toml
    let baseline_jobs = parse_job_list(&cronjobs, "config.toml");

    // Load initial usercron jobs
    let mut usercron_jobs = if let Some(ref path) = usercron_path {
        let configs = load_usercron_file(path, &platform_refs);
        if !configs.is_empty() {
            info!(count = configs.len(), path = %path.display(), "loaded usercron jobs");
        }
        parse_job_list(&configs, "cronjob.toml")
    } else {
        vec![]
    };
    let mut last_usercron_mtime: Option<SystemTime> = usercron_path.as_deref().and_then(file_mtime);

    if baseline_jobs.is_empty() && usercron_jobs.is_empty() {
        if usercron_path.is_some() {
            info!("no cronjobs yet, but usercron_path is set — scheduler will watch for cronjob.toml");
        } else {
            debug!("no cronjobs configured, scheduler not started");
            return;
        }
    }

    let total = baseline_jobs.len() + usercron_jobs.len();
    info!(baseline = baseline_jobs.len(), usercron = usercron_jobs.len(), total, "cron scheduler started");

    let in_flight: Arc<Mutex<HashSet<usize>>> = Arc::new(Mutex::new(HashSet::new()));

    // Align to next minute boundary
    let now = Utc::now();
    let secs_into_minute = now.timestamp() % 60;
    let align_delay = if secs_into_minute == 0 { 0 } else { 60 - secs_into_minute as u64 };
    if align_delay > 0 {
        debug!(align_secs = align_delay, "aligning to next minute boundary");
        tokio::time::sleep(std::time::Duration::from_secs(align_delay)).await;
    }
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // Hot-reload usercron file if mtime changed
                if let Some(ref path) = usercron_path {
                    let current_mtime = file_mtime(path);
                    if current_mtime != last_usercron_mtime {
                        let configs = load_usercron_file(path, &platform_refs);
                        info!(count = configs.len(), path = %path.display(), "usercron file changed, reloading");
                        // Clear in-flight tracking for usercron jobs (indices shift on reload)
                        {
                            let mut running = in_flight.lock().await;
                            let baseline_len = baseline_jobs.len();
                            running.retain(|idx| *idx < baseline_len);
                        }
                        usercron_jobs = parse_job_list(&configs, "cronjob.toml");
                        last_usercron_mtime = current_mtime;
                    }
                }

                // Evaluate all jobs: baseline first, then usercron
                let all_jobs = baseline_jobs.iter().chain(usercron_jobs.iter());
                for (idx, job) in all_jobs.enumerate() {
                    if !should_fire(&job.schedule, job.tz) {
                        continue;
                    }
                    {
                        let running = in_flight.lock().await;
                        if running.contains(&idx) {
                            warn!(schedule = %job.config.schedule, channel = %job.config.channel, "skipping cronjob, previous execution still running");
                            continue;
                        }
                    }
                    info!(
                        schedule = %job.config.schedule,
                        channel = %job.config.channel,
                        platform = %job.config.platform,
                        message = %job.config.message,
                        sender = %job.config.sender_name,
                        "🔔 cronjob fired"
                    );
                    in_flight.lock().await.insert(idx);

                    let config = job.config.clone();
                    let router = router.clone();
                    let adapters = adapters.clone();
                    let in_flight = in_flight.clone();
                    tasks.spawn(async move {
                        fire_cronjob(idx, &config, &router, &adapters, in_flight).await;
                    });
                }
                while tasks.try_join_next().is_some() {}
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("cron scheduler shutting down, waiting for in-flight tasks");
                    let drain = async { while tasks.join_next().await.is_some() {} };
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), drain).await;
                    return;
                }
            }
        }
    }
}

/// RAII guard that removes a job index from the in-flight set on drop.
struct InFlightGuard {
    idx: usize,
    set: Arc<Mutex<HashSet<usize>>>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let idx = self.idx;
        let set = self.set.clone();
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
        origin_event_id: None,
    };

    let trigger_msg = match adapter.send_message(&thread_channel, &format!("🕐 [{}]: {}", job.sender_name, job.message)).await {
        Ok(msg) => msg,
        Err(e) => {
            error!(channel = %job.channel, error = %e, "failed to send cron message");
            return;
        }
    };

    let reply_channel = if job.thread_id.is_some() {
        thread_channel.clone()
    } else {
        let thread_name = format::shorten_thread_name(&job.message);
        match adapter.create_thread(&thread_channel, &trigger_msg, &thread_name).await {
            Ok(ch) => ch,
            Err(e) => {
                error!(channel = %job.channel, error = %e, "failed to create cron thread");
                let _ = adapter.send_message(&thread_channel, &format!("⚠️ cronjob: failed to create thread: {e}")).await;
                return;
            }
        }
    };

    let sender = SenderContext {
        schema: "openab.sender.v1".into(),
        sender_id: "openab-cron".into(),
        sender_name: job.sender_name.clone(),
        display_name: job.sender_name.clone(),
        channel: job.platform.clone(),
        channel_id: reply_channel.parent_id.as_deref().unwrap_or(&reply_channel.channel_id).to_string(),
        thread_id: reply_channel.thread_id.clone().or(Some(reply_channel.channel_id.clone())),
        is_bot: true,
    };
    let sender_json = match serde_json::to_string(&sender) {
        Ok(j) => j,
        Err(e) => {
            warn!(error = %e, "failed to serialize cron sender context, skipping");
            return;
        }
    };

    if let Err(e) = router
        .handle_message(&adapter, &reply_channel, &sender_json, &job.message, vec![], &trigger_msg, false)
        .await
    {
        error!("cron handle_message error: {e}");
        let _ = adapter.send_message(&reply_channel, &format!("⚠️ cronjob error: {e}")).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn parse_valid_cron_expression() {
        let schedule = parse_cron_expr("0 9 * * 1-5").unwrap();
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
        assert!(parse_cron_expr("not a cron").is_err());
    }

    #[test]
    fn parse_invalid_cron_too_many_fields() {
        assert!(parse_cron_expr("0 0 9 * * 1-5").is_err());
    }

    #[test]
    fn valid_timezone_parses() {
        assert!("Asia/Taipei".parse::<Tz>().is_ok());
    }

    #[test]
    fn invalid_timezone_fails() {
        assert!("Mars/Olympus".parse::<Tz>().is_err());
    }

    #[test]
    fn utc_timezone_parses() {
        assert!("UTC".parse::<Tz>().is_ok());
    }

    #[test]
    fn should_fire_every_minute_returns_true() {
        let schedule = parse_cron_expr("* * * * *").unwrap();
        assert!(should_fire(&schedule, chrono_tz::UTC));
    }

    #[test]
    fn should_fire_returns_false_for_distant_schedule() {
        let schedule = parse_cron_expr("0 0 1 1 *").unwrap();
        let now = chrono::Utc::now();
        if now.month() != 1 || now.day() != 1 || now.hour() != 0 {
            assert!(!should_fire(&schedule, chrono_tz::UTC));
        }
    }

    #[test]
    fn should_fire_respects_timezone() {
        let schedule = parse_cron_expr("* * * * *").unwrap();
        let tz: Tz = "Asia/Taipei".parse().unwrap();
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
        let cfg: UsercronFile = toml::from_str(toml_str).unwrap();
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
        let cfg: UsercronFile = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.cronjobs[0].enabled, false);
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
        let cfg: UsercronFile = toml::from_str(toml_str).unwrap();
        let job = &cfg.cronjobs[0];
        assert_eq!(job.platform, "slack");
        assert_eq!(job.sender_name, "DailyOps");
        assert_eq!(job.timezone, "Asia/Taipei");
        assert_eq!(job.thread_id.as_deref(), Some("789"));
    }

    #[test]
    fn load_usercron_nonexistent_returns_empty() {
        let jobs = load_usercron_file(Path::new("/tmp/nonexistent-usercron.toml"), &["discord"]);
        assert!(jobs.is_empty());
    }

    #[test]
    fn load_usercron_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cronjob.toml");
        std::fs::write(&path, r#"
[[cronjobs]]
schedule = "* * * * *"
channel = "123"
message = "ping"
"#).unwrap();
        let jobs = load_usercron_file(&path, &["discord"]);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].message, "ping");
    }

    #[test]
    fn load_usercron_invalid_toml_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cronjob.toml");
        std::fs::write(&path, "not valid toml {{{").unwrap();
        let jobs = load_usercron_file(&path, &["discord"]);
        assert!(jobs.is_empty());
    }

    #[test]
    fn load_usercron_skips_invalid_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cronjob.toml");
        std::fs::write(&path, r#"
[[cronjobs]]
schedule = "* * * * *"
channel = "123"
message = "good"

[[cronjobs]]
schedule = "bad cron"
channel = "456"
message = "bad"
"#).unwrap();
        let jobs = load_usercron_file(&path, &["discord"]);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].message, "good");
    }

    #[test]
    fn load_usercron_skips_unconfigured_platform() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cronjob.toml");
        std::fs::write(&path, r#"
[[cronjobs]]
schedule = "* * * * *"
channel = "123"
message = "discord job"

[[cronjobs]]
schedule = "* * * * *"
channel = "456"
message = "slack job"
platform = "slack"
"#).unwrap();
        // Only discord configured
        let jobs = load_usercron_file(&path, &["discord"]);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].message, "discord job");
    }
}
