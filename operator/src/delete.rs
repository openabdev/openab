use anyhow::{Context, Result};
use std::path::Path;

/// Delete every OABService defined in a manifest file or directory (mirrors
/// `apply -f`). Each manifest's `metadata.name`/`metadata.namespace` are used
/// directly — cluster is resolved from bootstrap state (supports imported
/// clusters with custom names), falling back to "oab" if unavailable.
/// An `OABFleet` manifest expands to multiple services, all deleted in turn.
///
/// Continues past a failed delete instead of stopping at the first one, so
/// one broken/already-gone service in a fleet doesn't block cleanup of the
/// rest — but still returns an error at the end if anything failed.
pub async fn run_from_file(aws_config: &aws_config::SdkConfig, file_path: &str) -> Result<()> {
    let path = Path::new(file_path);
    let manifests = crate::apply::load_manifests(path)
        .with_context(|| format!("failed to load manifest(s) from {file_path}"))?;

    if manifests.is_empty() {
        anyhow::bail!("no manifests found at {}", file_path);
    }

    // Resolve cluster from config (same source as `get` and CLI `delete` commands).
    // ~/.oabctl/config.toml defaults.cluster; falls back to "oab".
    // Propagate config load errors — silently defaulting could target the wrong cluster.
    let oab_cfg = crate::config::OabConfig::load()
        .context("failed to load ~/.oabctl/config.toml (run `oabctl bootstrap` first)")?;
    let cluster = &oab_cfg.defaults.cluster;

    let mut failures = Vec::new();
    for m in &manifests {
        println!("Deleting {} (from {})...", m.metadata.name, file_path);
        if let Err(e) = run(aws_config, "oabservice", &m.metadata.name, cluster, &m.metadata.namespace).await {
            eprintln!("  ⚠ failed to delete {}: {e}", m.metadata.name);
            failures.push(m.metadata.name.clone());
        }
    }

    if !failures.is_empty() {
        anyhow::bail!("failed to delete {} of {} service(s): {}", failures.len(), manifests.len(), failures.join(", "));
    }
    Ok(())
}


pub async fn run(
    aws_config: &aws_config::SdkConfig,
    resource: &str,
    name: &str,
    cluster: &str,
    namespace: &str,
) -> Result<()> {
    if resource != "oabservice" {
        anyhow::bail!("unknown resource type: {}. Use 'oabservice'", resource);
    }

    let service_name = format!("oab-{}-{}", namespace, name);
    let ecs = aws_sdk_ecs::Client::new(aws_config);
    let s3 = aws_sdk_s3::Client::new(aws_config);
    let bucket = "oab-control-plane";

    println!("Deleting {}...", name);

    // Capture the service's Cloud Map registry ARN (if any) BEFORE deleting it,
    // so teardown can resolve the exact Cloud Map service by ARN instead of a
    // name-only account-wide scan (which could otherwise match a
    // same-named bot in a different VPC/environment).
    let registry_arn: Option<String> = ecs
        .describe_services()
        .cluster(cluster)
        .services(&service_name)
        .send()
        .await
        .ok()
        .and_then(|r| r.services().first().cloned())
        .and_then(|s| s.service_registries().first().and_then(|r| r.registry_arn()).map(|a| a.to_string()));

    // 1. Scale to 0
    let _ = ecs
        .update_service()
        .cluster(cluster)
        .service(&service_name)
        .desired_count(0)
        .send()
        .await;
    println!("  ✓ Scaled to 0");

    // 2. Delete ECS service
    ecs.delete_service()
        .cluster(cluster)
        .service(&service_name)
        .force(true)
        .send()
        .await
        .context("failed to delete ECS service")?;
    println!("  ✓ ECS service deleted");

    // 2a. Wait for the service to fully drain (INACTIVE status) so that
    // a subsequent `apply` doesn't hit "Unable to Start a service that
    // is still Draining".
    const DRAIN_POLL_ATTEMPTS: u32 = 12;
    const DRAIN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

    eprint!("  ⏳ Waiting for drain to complete...");
    for i in 0..DRAIN_POLL_ATTEMPTS {
        // Check status first, then sleep — avoids an unnecessary initial delay
        // when the service transitions quickly.
        let resp = ecs
            .describe_services()
            .cluster(cluster)
            .services(&service_name)
            .send()
            .await;
        let is_gone = match resp {
            Ok(r) => r
                .services()
                .first()
                .map(|s| s.status() == Some("INACTIVE"))
                .unwrap_or(true),
            Err(e) => {
                eprintln!("\n  ⚠ describe_services error (retrying): {e}");
                false
            }
        };
        if is_gone {
            if i == 0 {
                eprintln!(" done (immediate)");
            } else {
                let elapsed = u64::from(i) * DRAIN_POLL_INTERVAL.as_secs();
                eprintln!(" done ({elapsed}s)");
            }
            break;
        }
        if i == DRAIN_POLL_ATTEMPTS - 1 {
            eprintln!(" timed out (service may still be draining)");
        } else {
            eprint!(".");
            tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
        }
    }

    // 2b. Best-effort ingress teardown: Cloud Map service + this API's
    // routes/integration/stage. No-op for bots that never had ingress. Never
    // blocks deletion — failures are logged only.
    if let Err(e) =
        crate::ingress::teardown(aws_config, namespace, name, registry_arn.as_deref()).await
    {
        eprintln!("  ⚠ ingress teardown skipped: {e}");
    }

    // 2c. The bot is being permanently removed here (unlike `apply`'s
    // ingress-removed recreate path), so it's safe to delete the per-bot HTTP
    // API resource itself too — there's no webhook URL to keep stable for a
    // bot that no longer exists.
    if let Err(e) = crate::ingress::delete_api(aws_config, namespace, name).await {
        eprintln!("  ⚠ HTTP API cleanup skipped: {e}");
    }

    // 3. Clean up S3 manifest
    let manifest_key = format!("manifests/{}/{}.yaml", namespace, name);
    let _ = s3
        .delete_object()
        .bucket(bucket)
        .key(&manifest_key)
        .send()
        .await;
    println!("  ✓ Manifest removed from S3");

    // 4. Clean up S3 config (list and delete all generations)
    let config_prefix = format!("config/{}/{}/", namespace, name);
    let list = s3
        .list_objects_v2()
        .bucket(bucket)
        .prefix(&config_prefix)
        .send()
        .await;
    if let Ok(resp) = list {
        for obj in resp.contents() {
            if let Some(key) = obj.key() {
                let _ = s3.delete_object().bucket(bucket).key(key).send().await;
            }
        }
    }
    println!("  ✓ Config artifacts removed from S3");

    println!("\n✓ {} deleted", name);
    Ok(())
}
