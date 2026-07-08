use anyhow::{Context, Result};
use aws_sdk_scheduler::error::ProvideErrorMetadata;

/// Resolve a service name to (cluster, service_name).
/// Only resolves against oabctl-managed services (oab cluster, oab-* prefix).
/// Does NOT use ecsctl aliases — scheduled scaling is for oabctl services only.
async fn resolve_service(
    aws_config: &aws_config::SdkConfig,
    name: &str,
) -> Result<(String, String)> {
    let oab_cfg =
        crate::config::OabConfig::load().context("failed to load ~/.oabctl/config.toml")?;
    let cluster = oab_cfg.defaults.cluster;
    let namespace = oab_cfg.defaults.namespace;
    let service_name = format!("oab-{}-{}", namespace, name);

    // Verify the service exists in the oab cluster
    let ecs = aws_sdk_ecs::Client::new(aws_config);
    let resp = ecs
        .describe_services()
        .cluster(&cluster)
        .services(&service_name)
        .send()
        .await
        .context("failed to describe ECS service")?;

    let svc = resp.services().first();
    match svc {
        Some(s) if s.status() == Some("ACTIVE") => {}
        Some(s) => {
            let status = s.status().unwrap_or("UNKNOWN");
            anyhow::bail!(
                "service '{}' is {} — cannot scale. Use 'oabctl get oabservice' to list available services.",
                name, status
            );
        }
        None => {
            anyhow::bail!(
                "service '{}' not found. Use 'oabctl get oabservice' to list available services.\n\
                 Note: oabctl scale only works with oabctl-managed services, not ecsctl aliases.",
                name
            );
        }
    }

    Ok((cluster, service_name))
}

/// Immediate scale: delegates to ecsctl's scale_service for the core ECS call.
pub async fn run(aws_config: &aws_config::SdkConfig, alias: &str, size: i32) -> Result<()> {
    validate_size(size)?;
    let (cluster, service_name) = resolve_service(aws_config, alias).await?;
    let ecs = aws_sdk_ecs::Client::new(aws_config);

    ecsctl::scale::scale_service(&ecs, &cluster, &service_name, size, false).await?;

    Ok(())
}

/// Build the scheduler Target for ECS UpdateService.
fn build_schedule_target(
    role_arn: &str,
    target_input: &str,
) -> Result<aws_sdk_scheduler::types::Target> {
    aws_sdk_scheduler::types::Target::builder()
        .arn("arn:aws:scheduler:::aws-sdk:ecs:updateService")
        .role_arn(role_arn)
        .input(target_input)
        .build()
        .context("failed to build scheduler target")
}

/// Build the FlexibleTimeWindow (OFF mode).
fn build_flexible_time_window() -> Result<aws_sdk_scheduler::types::FlexibleTimeWindow> {
    aws_sdk_scheduler::types::FlexibleTimeWindow::builder()
        .mode(aws_sdk_scheduler::types::FlexibleTimeWindowMode::Off)
        .build()
        .context("failed to build flexible time window")
}

/// Validate scale size — OAB services are single-instance (one bot token per service).
/// Only 0 (off) or 1 (on) is valid.
fn validate_size(size: i32) -> Result<()> {
    if size != 0 && size != 1 {
        anyhow::bail!(
            "invalid size: {}. OAB services can only scale to 0 (off) or 1 (on) — \
             each service runs a single bot token and scaling above 1 would cause duplicate responses.",
            size
        );
    }
    Ok(())
}

/// Validate schedule expression format.
/// EventBridge Scheduler accepts: cron(...), rate(...), at(...)
fn validate_schedule_expression(expr: &str) -> Result<()> {
    let trimmed = expr.trim();
    if trimmed.starts_with("cron(") && trimmed.ends_with(')') {
        // cron expressions should have 6 fields: min hour dom month dow year
        let inner = &trimmed[5..trimmed.len() - 1];
        let fields: Vec<&str> = inner.split_whitespace().collect();
        if fields.len() != 6 {
            anyhow::bail!(
                "invalid cron expression: expected 6 fields (min hour dom month dow year), got {}.\n\
                 Example: cron(0 8 * * ? *)",
                fields.len()
            );
        }
        Ok(())
    } else if trimmed.starts_with("rate(") && trimmed.ends_with(')') {
        let inner = &trimmed[5..trimmed.len() - 1].trim();
        if inner.is_empty() {
            anyhow::bail!(
                "invalid rate expression: empty value.\n\
                 Example: rate(1 hour) or rate(5 minutes)"
            );
        }
        Ok(())
    } else if trimmed.starts_with("at(") && trimmed.ends_with(')') {
        Ok(())
    } else {
        anyhow::bail!(
            "invalid schedule expression: '{}'\n\
             Must start with cron(...), rate(...), or at(...).\n\
             Examples:\n\
             - cron(0 8 * * ? *)      — daily at 8:00 AM\n\
             - rate(1 hour)           — every hour\n\
             - at(2024-01-01T00:00:00) — one-time",
            trimmed
        );
    }
}

/// Check if a schedule exists. Returns true only for confirmed existence;
/// returns false for ResourceNotFoundException; propagates other errors.
async fn schedule_exists(
    scheduler: &aws_sdk_scheduler::Client,
    name: &str,
    group_name: &str,
) -> Result<bool> {
    match scheduler
        .get_schedule()
        .name(name)
        .group_name(group_name)
        .send()
        .await
    {
        Ok(_) => Ok(true),
        Err(e) => {
            let service_err = e.as_service_error();
            if service_err
                .map(|se| se.is_resource_not_found_exception())
                .unwrap_or(false)
            {
                Ok(false)
            } else {
                Err(e).context(format!("failed to check if schedule '{}' exists", name))
            }
        }
    }
}

/// Scheduled scale: create an EventBridge Scheduler schedule that calls
/// ECS UpdateService at the given schedule expression.
pub async fn run_with_schedule(
    aws_config: &aws_config::SdkConfig,
    alias: &str,
    size: i32,
    schedule_expression: &str,
    timezone: Option<&str>,
) -> Result<()> {
    validate_size(size)?;
    // Basic input validation for schedule expression
    validate_schedule_expression(schedule_expression)?;

    let (cluster, service_name) = resolve_service(aws_config, alias).await?;
    let scheduler = aws_sdk_scheduler::Client::new(aws_config);
    let sts = aws_sdk_sts::Client::new(aws_config);
    let iam = aws_sdk_iam::Client::new(aws_config);

    // Get account ID for ARN construction
    let identity = sts
        .get_caller_identity()
        .send()
        .await
        .context("failed to get caller identity")?;
    let account_id = identity.account().context("no account ID")?;
    let region = aws_config
        .region()
        .map(|r| r.as_ref().to_string())
        .unwrap_or_else(|| "us-east-1".to_string());

    // Ensure schedule group exists
    let group_name = "oab-schedules";
    ensure_schedule_group(&scheduler, group_name).await?;

    // Ensure scheduler IAM role exists
    let role_arn = ensure_scheduler_role(&iam, account_id, &region).await?;

    // Build schedule name: oab-scale-{alias}-to-{size}
    // AWS schedule names: max 64 chars, pattern [0-9a-zA-Z-_.]+
    // Truncate alias (not suffix) to preserve -to-{size} for uniqueness
    let safe_alias = alias.replace(
        |c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.',
        "-",
    );
    let suffix = format!("-to-{}", size);
    let prefix = "oab-scale-";
    let max_alias_len = 64 - prefix.len() - suffix.len();
    let truncated_alias = if safe_alias.len() > max_alias_len {
        &safe_alias[..max_alias_len]
    } else {
        &safe_alias
    };
    let schedule_name = format!("{}{}{}", prefix, truncated_alias, suffix);

    // Build the ECS UpdateService input for the universal target
    let target_input = serde_json::json!({
        "Cluster": cluster,
        "Service": service_name,
        "DesiredCount": size
    });
    let target_input_str = target_input.to_string();

    let tz = timezone.unwrap_or("UTC");

    // Check if schedule already exists (properly handles transient errors)
    let exists = schedule_exists(&scheduler, &schedule_name, group_name).await?;

    let target = build_schedule_target(&role_arn, &target_input_str)?;
    let flexible_time_window = build_flexible_time_window()?;

    if exists {
        scheduler
            .update_schedule()
            .name(&schedule_name)
            .group_name(group_name)
            .schedule_expression(schedule_expression)
            .schedule_expression_timezone(tz)
            .flexible_time_window(flexible_time_window)
            .target(target)
            .send()
            .await
            .context("failed to update schedule")?;
    } else {
        // Retry with backoff, but ONLY for IAM propagation errors (AccessDeniedException).
        // Other errors (validation, quota, etc.) fail immediately.
        let mut last_err = None;
        for attempt in 0..5 {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs(2u64.pow(attempt));
                eprintln!(
                    "  retrying schedule creation (attempt {}/5, waiting {}s for IAM propagation)...",
                    attempt + 1,
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
            }
            let t = build_schedule_target(&role_arn, &target_input_str)?;
            let ftw = build_flexible_time_window()?;
            match scheduler
                .create_schedule()
                .name(&schedule_name)
                .group_name(group_name)
                .schedule_expression(schedule_expression)
                .schedule_expression_timezone(tz)
                .flexible_time_window(ftw)
                .target(t)
                .send()
                .await
            {
                Ok(_) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    // Retry on IAM propagation delays. These manifest as:
                    // - AccessDeniedException: role not yet assumable
                    // - ValidationException with "execution role": role propagation pending
                    // All other errors (quota, conflict, bad input) fail immediately.
                    let is_iam_propagation = e
                        .as_service_error()
                        .map(|se| {
                            if se.is_validation_exception() {
                                // ValidationException during role propagation mentions "execution role"
                                se.message()
                                    .map(|m| m.contains("execution role"))
                                    .unwrap_or(false)
                            } else {
                                // AccessDeniedException is unmodeled; check via code()
                                se.code()
                                    .map(|c| c == "AccessDeniedException" || c == "AccessDenied")
                                    .unwrap_or(false)
                            }
                        })
                        .unwrap_or(false);
                    if is_iam_propagation {
                        last_err = Some(e);
                    } else {
                        return Err(e).context("failed to create schedule");
                    }
                }
            }
        }
        if let Some(e) = last_err {
            return Err(e).context(
                "failed to create schedule after retries (IAM role may not have propagated)",
            );
        }
    }

    let action = if exists { "Updated" } else { "Created" };
    println!("✓ Schedule {action}: {schedule_name}");
    println!("  Expression: {schedule_expression} ({tz})");
    println!("  Action:     scale {alias} ({service_name}) to {size}");
    println!("  Group:      {group_name}");
    println!("\n  Use 'oabctl schedule list' to view all schedules");
    println!("  Use 'oabctl schedule delete {schedule_name}' to remove");
    Ok(())
}

/// List all schedules in the oab-schedules group.
pub async fn list_schedules(aws_config: &aws_config::SdkConfig) -> Result<()> {
    let scheduler = aws_sdk_scheduler::Client::new(aws_config);
    let group_name = "oab-schedules";

    // Paginate through all schedules
    let mut all_schedules = Vec::new();
    let mut next_token: Option<String> = None;

    loop {
        let mut req = scheduler.list_schedules().group_name(group_name);
        if let Some(token) = &next_token {
            req = req.next_token(token);
        }

        let resp = req.send().await;

        match resp {
            Ok(output) => {
                all_schedules.extend(output.schedules().to_vec());
                next_token = output.next_token().map(|s| s.to_string());
                if next_token.is_none() {
                    break;
                }
            }
            Err(e) => {
                if e.as_service_error()
                    .map(|se| se.is_resource_not_found_exception())
                    .unwrap_or(false)
                {
                    println!("No schedules configured yet.");
                    println!(
                        "  Use 'oabctl scale <alias> <size> --with-schedule <expr>' to create one."
                    );
                    return Ok(());
                } else {
                    anyhow::bail!("failed to list schedules: {e}");
                }
            }
        }
    }

    if all_schedules.is_empty() {
        println!("No schedules found in group '{group_name}'.");
        println!("  Use 'oabctl scale <alias> <size> --with-schedule <expr>' to create one.");
        return Ok(());
    }

    // Warn about N+1 latency for large schedule counts
    if all_schedules.len() > 10 {
        eprintln!(
            "  note: fetching details for {} schedules — this may take a moment...",
            all_schedules.len()
        );
    }

    println!("{:<40} {:<30} {:<16} STATE", "NAME", "SCHEDULE", "TIMEZONE");
    for s in &all_schedules {
        let name = s.name().unwrap_or("-");
        let state = s.state().map(|st| st.as_str()).unwrap_or("?");

        // Fetch full schedule to get expression and timezone
        // Note: N+1 API calls — acceptable for typical oab schedule counts (<20)
        let (expr, tz) = match scheduler
            .get_schedule()
            .name(name)
            .group_name(group_name)
            .send()
            .await
        {
            Ok(detail) => {
                let e = detail.schedule_expression().unwrap_or("-").to_string();
                let t = detail
                    .schedule_expression_timezone()
                    .unwrap_or("UTC")
                    .to_string();
                (e, t)
            }
            Err(_) => ("-".to_string(), "-".to_string()),
        };

        println!("{:<40} {:<30} {:<16} {}", name, expr, tz, state);
    }

    Ok(())
}

/// Delete a specific schedule (idempotent — already-deleted is not an error).
pub async fn delete_schedule(aws_config: &aws_config::SdkConfig, name: &str) -> Result<()> {
    let scheduler = aws_sdk_scheduler::Client::new(aws_config);
    let group_name = "oab-schedules";

    match scheduler
        .delete_schedule()
        .name(name)
        .group_name(group_name)
        .send()
        .await
    {
        Ok(_) => {
            println!("✓ Deleted schedule: {name}");
        }
        Err(e) => {
            if e.as_service_error()
                .map(|se| se.is_resource_not_found_exception())
                .unwrap_or(false)
            {
                println!("Schedule '{name}' not found (already deleted or never existed).");
            } else {
                return Err(e).context(format!("failed to delete schedule '{name}'"));
            }
        }
    }

    Ok(())
}

/// Ensure the oab-schedules group exists (idempotent).
async fn ensure_schedule_group(
    scheduler: &aws_sdk_scheduler::Client,
    group_name: &str,
) -> Result<()> {
    let resp = scheduler.get_schedule_group().name(group_name).send().await;

    if resp.is_err() {
        let create_result = scheduler
            .create_schedule_group()
            .name(group_name)
            .send()
            .await;

        // Ignore ConflictException (race condition / already exists)
        if let Err(e) = create_result {
            if !e
                .as_service_error()
                .map(|se| se.is_conflict_exception())
                .unwrap_or(false)
            {
                anyhow::bail!("failed to create schedule group: {e}");
            }
        }
    }

    Ok(())
}

/// Ensure the oab-scheduler-role exists (for EventBridge Scheduler to call ECS).
/// Also verifies the inline policy is attached (recovers from partial-state).
async fn ensure_scheduler_role(
    iam: &aws_sdk_iam::Client,
    account_id: &str,
    region: &str,
) -> Result<String> {
    let role_name = "oab-scheduler-role";
    let role_arn = format!("arn:aws:iam::{}:role/{}", account_id, role_name);
    let policy_name = "oab-ecs-scale";

    // Check if role exists
    let role_exists = iam.get_role().role_name(role_name).send().await.is_ok();

    if !role_exists {
        // Create the role with confused-deputy protection
        let trust_policy = serde_json::json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {
                    "Service": "scheduler.amazonaws.com"
                },
                "Action": "sts:AssumeRole",
                "Condition": {
                    "StringEquals": {
                        "aws:SourceAccount": account_id,
                        "aws:SourceArn": format!("arn:aws:scheduler:{region}:{account_id}:schedule-group/oab-schedules")
                    }
                }
            }]
        });

        let create_result = iam
            .create_role()
            .role_name(role_name)
            .assume_role_policy_document(trust_policy.to_string())
            .description(
                "Allows EventBridge Scheduler to call ECS UpdateService for oabctl scale schedules",
            )
            .send()
            .await;

        // Ignore EntityAlreadyExists (race condition with concurrent oabctl runs)
        if let Err(e) = create_result {
            if !e
                .as_service_error()
                .map(|se| se.is_entity_already_exists_exception())
                .unwrap_or(false)
            {
                return Err(e).context("failed to create scheduler IAM role");
            }
        }
    }

    // Always ensure inline policy is current (put_role_policy is idempotent —
    // overwrites existing policy with same name, handles stale/outdated scope).
    // Use wildcard for cluster to support multi-cluster deployments — the oab-*
    // prefix on service name provides sufficient scope restriction.
    let ecs_policy = serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "ecs:UpdateService",
            "Resource": format!("arn:aws:ecs:{region}:{account_id}:service/*/oab-*")
        }]
    });

    iam.put_role_policy()
        .role_name(role_name)
        .policy_name(policy_name)
        .policy_document(ecs_policy.to_string())
        .send()
        .await
        .context("failed to attach policy to scheduler role")?;

    // Wait for IAM propagation if role was just created.
    // IAM is eventually consistent; rather than a fixed sleep, we rely on
    // retry logic at schedule creation time if the first attempt fails with
    // AccessDenied due to propagation delay.
    if !role_exists {
        eprintln!("  ✓ Created IAM role: {role_name} (may take a few seconds to propagate)");
    }

    Ok(role_arn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_schedule_expression_valid_cron() {
        assert!(validate_schedule_expression("cron(0 8 * * ? *)").is_ok());
        assert!(validate_schedule_expression("cron(30 12 1 * ? 2024)").is_ok());
    }

    #[test]
    fn test_validate_schedule_expression_invalid_cron_fields() {
        // 5 fields instead of 6
        let result = validate_schedule_expression("cron(0 8 * * ?)");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("expected 6 fields"));
    }

    #[test]
    fn test_validate_schedule_expression_valid_rate() {
        assert!(validate_schedule_expression("rate(1 hour)").is_ok());
        assert!(validate_schedule_expression("rate(5 minutes)").is_ok());
    }

    #[test]
    fn test_validate_schedule_expression_empty_rate() {
        let result = validate_schedule_expression("rate()");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_schedule_expression_valid_at() {
        assert!(validate_schedule_expression("at(2024-01-01T00:00:00)").is_ok());
    }

    #[test]
    fn test_validate_schedule_expression_invalid_prefix() {
        let result = validate_schedule_expression("every 5 minutes");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Must start with cron("), "got: {}", err);
    }

    #[test]
    fn test_validate_schedule_expression_missing_parens() {
        let result = validate_schedule_expression("cron 0 8 * * ? *");
        assert!(result.is_err());
    }

    #[test]
    fn test_schedule_name_sanitization() {
        // Test the same logic used in run_with_schedule for schedule naming
        let alias = "my-bot/special";
        let safe_alias = alias.replace(
            |c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.',
            "-",
        );
        let schedule_name = format!("oab-scale-{}-to-{}", safe_alias, 0);
        assert_eq!(schedule_name, "oab-scale-my-bot-special-to-0");
    }

    #[test]
    fn test_schedule_name_sanitization_unicode() {
        // Unicode chars are NOT valid in AWS schedule names — replaced with '-'
        let alias = "bot名前";
        let safe_alias = alias.replace(
            |c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.',
            "-",
        );
        let schedule_name = format!("oab-scale-{}-to-{}", safe_alias, 1);
        assert_eq!(schedule_name, "oab-scale-bot---to-1");
    }

    #[test]
    fn test_schedule_name_sanitization_special_chars() {
        let alias = "my.bot@prod";
        let safe_alias = alias.replace(
            |c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.',
            "-",
        );
        assert_eq!(safe_alias, "my.bot-prod");
    }

    #[test]
    fn test_schedule_name_length_cap() {
        let alias = "a-very-long-service-name-that-exceeds-the-sixty-four-character-limit";
        let safe_alias = alias.replace(
            |c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.',
            "-",
        );
        let size = 0;
        let suffix = format!("-to-{}", size);
        let prefix = "oab-scale-";
        let max_alias_len = 64 - prefix.len() - suffix.len();
        let truncated_alias = if safe_alias.len() > max_alias_len {
            &safe_alias[..max_alias_len]
        } else {
            &safe_alias
        };
        let schedule_name = format!("{}{}{}", prefix, truncated_alias, suffix);
        assert!(schedule_name.len() <= 64);
        // Verify suffix is preserved (different sizes produce different names)
        assert!(schedule_name.ends_with("-to-0"));
    }

    #[test]
    fn test_build_schedule_target() {
        let target = build_schedule_target(
            "arn:aws:iam::123456789012:role/test-role",
            r#"{"Cluster":"test","Service":"svc","DesiredCount":1}"#,
        );
        assert!(target.is_ok());
        let t = target.unwrap();
        assert_eq!(t.arn(), "arn:aws:scheduler:::aws-sdk:ecs:updateService");
        assert_eq!(t.role_arn(), "arn:aws:iam::123456789012:role/test-role");
    }

    #[test]
    fn test_build_flexible_time_window() {
        let ftw = build_flexible_time_window();
        assert!(ftw.is_ok());
    }

    #[test]
    fn test_validate_size_zero() {
        assert!(validate_size(0).is_ok());
    }

    #[test]
    fn test_validate_size_one() {
        assert!(validate_size(1).is_ok());
    }

    #[test]
    fn test_validate_size_rejects_two() {
        let result = validate_size(2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid size: 2"));
        assert!(err.contains("0 (off) or 1 (on)"));
    }

    #[test]
    fn test_validate_size_rejects_negative() {
        let result = validate_size(-1);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid size: -1"));
    }

    #[test]
    fn test_validate_size_rejects_large() {
        let result = validate_size(100);
        assert!(result.is_err());
    }
}
