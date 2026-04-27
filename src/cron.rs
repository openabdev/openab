use crate::adapter::{AdapterRouter, ChatAdapter, ChannelRef, MessageRef, SenderContext};
use crate::config::CronJobConfig;
use chrono::Utc;
use chrono_tz::Tz;
use cron::Schedule;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

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

    // Parse and validate all cron expressions upfront
    let jobs: Vec<(Schedule, Tz, &CronJobConfig)> = cronjobs
        .iter()
        .filter_map(|job| {
            let expr = format!("0 {}", job.schedule);
            let schedule = match Schedule::from_str(&expr) {
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

    loop {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                let now = Utc::now();
                for (idx, (schedule, tz, job)) in jobs.iter().enumerate() {
                    let now_tz = now.with_timezone(tz);
                    if let Some(next) = schedule.upcoming(*tz).next() {
                        let diff = (next - now_tz).num_seconds();
                        debug!(schedule = %job.schedule, timezone = %job.timezone, next = %next, diff_secs = diff, "checking");
                        if diff <= 60 {
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
                            let in_flight = in_flight.clone();
                            in_flight.lock().await.insert(idx);
                            fire_cronjob(idx, job, &router, &adapters, in_flight).await;
                        }
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("cron scheduler shutting down");
                    return;
                }
            }
        }
    }
}

async fn fire_cronjob(
    idx: usize,
    job: &CronJobConfig,
    router: &Arc<AdapterRouter>,
    adapters: &HashMap<String, Arc<dyn ChatAdapter>>,
    in_flight: Arc<Mutex<HashSet<usize>>>,
) {
    let adapter = match adapters.get(&job.platform) {
        Some(a) => a.clone(),
        None => {
            error!(platform = %job.platform, "no adapter for platform, skipping cronjob");
            in_flight.lock().await.remove(&idx);
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
    let sender_json = serde_json::to_string(&sender).unwrap_or_default();

    // Send visible message first so users see what triggered
    let trigger_msg = match adapter.send_message(&thread_channel, &format!("🕐 [{}]: {}", job.sender_name, job.message)).await {
        Ok(msg) => msg,
        Err(e) => {
            error!(channel = %job.channel, error = %e, "failed to send cron message");
            in_flight.lock().await.remove(&idx);
            return;
        }
    };

    // Trigger agent processing
    let router = router.clone();
    let adapter = adapter.clone();
    let prompt = job.message.clone();
    let thread_channel = thread_channel.clone();
    tokio::spawn(async move {
        if let Err(e) = router
            .handle_message(&adapter, &thread_channel, &sender_json, &prompt, vec![], &trigger_msg, false)
            .await
        {
            error!("cron handle_message error: {e}");
        }
        in_flight.lock().await.remove(&idx);
    });
}
