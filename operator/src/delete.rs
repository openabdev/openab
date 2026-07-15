use anyhow::{Context, Result};
use std::path::Path;

/// Delete every OABService defined in a manifest file or directory.
pub(crate) async fn run_from_file(
    aws_config: &aws_config::SdkConfig,
    file_path: &str,
) -> Result<()> {
    let path = Path::new(file_path);
    let manifests = crate::apply::load_manifests(path)
        .with_context(|| format!("failed to load manifest(s) from {file_path}"))?;
    if manifests.is_empty() {
        anyhow::bail!("no manifests found at {file_path}");
    }

    let oab_cfg = crate::config::OabConfig::load()
        .context("failed to load ~/.oabctl/config.toml (run `oabctl bootstrap` first)")?;
    let cluster = &oab_cfg.defaults.cluster;
    let bucket =
        crate::control_plane::resolve_bucket(aws_config, oab_cfg.bootstrap.bucket.as_deref())
            .await?;

    let mut failures = Vec::new();
    for manifest in &manifests {
        println!(
            "Deleting {} (from {})...",
            manifest.metadata.name, file_path
        );
        if let Err(error) = run_with_bucket(
            aws_config,
            "oabservice",
            &manifest.metadata.name,
            cluster,
            &manifest.metadata.namespace,
            &bucket,
        )
        .await
        {
            eprintln!("  ⚠ failed to delete {}: {error}", manifest.metadata.name);
            failures.push(manifest.metadata.name.clone());
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "failed to delete {} of {} service(s): {}",
            failures.len(),
            manifests.len(),
            failures.join(", ")
        );
    }
    Ok(())
}

pub(crate) async fn run(
    aws_config: &aws_config::SdkConfig,
    resource: &str,
    name: &str,
    cluster: &str,
    namespace: &str,
) -> Result<()> {
    let oab_cfg =
        crate::config::OabConfig::load().context("failed to load ~/.oabctl/config.toml")?;
    let bucket =
        crate::control_plane::resolve_bucket(aws_config, oab_cfg.bootstrap.bucket.as_deref())
            .await?;
    run_with_bucket(aws_config, resource, name, cluster, namespace, &bucket).await
}

async fn run_with_bucket(
    aws_config: &aws_config::SdkConfig,
    resource: &str,
    name: &str,
    cluster: &str,
    namespace: &str,
    bucket: &str,
) -> Result<()> {
    if resource != "oabservice" {
        anyhow::bail!("unknown resource type: {resource}. Use 'oabservice'");
    }

    let service_name = format!("oab-{namespace}-{name}");
    let ecs = aws_sdk_ecs::Client::new(aws_config);
    let s3 = aws_sdk_s3::Client::new(aws_config);

    println!("Deleting {name}...");

    let registry_arn: Option<String> = ecs
        .describe_services()
        .cluster(cluster)
        .services(&service_name)
        .send()
        .await
        .ok()
        .and_then(|response| response.services().first().cloned())
        .and_then(|service| {
            service
                .service_registries()
                .first()
                .and_then(|registry| registry.registry_arn())
                .map(str::to_owned)
        });

    let _ = ecs
        .update_service()
        .cluster(cluster)
        .service(&service_name)
        .desired_count(0)
        .send()
        .await;
    println!("  ✓ Scaled to 0");

    ecs.delete_service()
        .cluster(cluster)
        .service(&service_name)
        .force(true)
        .send()
        .await
        .context("failed to delete ECS service")?;
    println!("  ✓ ECS service deleted");

    const DRAIN_POLL_ATTEMPTS: u32 = 12;
    const DRAIN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
    eprint!("  ⏳ Waiting for drain to complete...");
    for attempt in 0..DRAIN_POLL_ATTEMPTS {
        let response = ecs
            .describe_services()
            .cluster(cluster)
            .services(&service_name)
            .send()
            .await;
        let is_gone = match response {
            Ok(response) => response
                .services()
                .first()
                .map(|service| service.status() == Some("INACTIVE"))
                .unwrap_or(true),
            Err(error) => {
                eprintln!("\n  ⚠ describe_services error (retrying): {error}");
                false
            }
        };
        if is_gone {
            if attempt == 0 {
                eprintln!(" done (immediate)");
            } else {
                let elapsed = u64::from(attempt) * DRAIN_POLL_INTERVAL.as_secs();
                eprintln!(" done ({elapsed}s)");
            }
            break;
        }
        if attempt == DRAIN_POLL_ATTEMPTS - 1 {
            eprintln!(" timed out (service may still be draining)");
        } else {
            eprint!(".");
            tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
        }
    }

    if let Err(error) =
        crate::ingress::teardown(aws_config, namespace, name, registry_arn.as_deref()).await
    {
        eprintln!("  ⚠ ingress teardown skipped: {error}");
    }
    if let Err(error) = crate::ingress::delete_api(aws_config, namespace, name).await {
        eprintln!("  ⚠ HTTP API cleanup skipped: {error}");
    }

    let manifest_key = format!("manifests/{namespace}/{name}.yaml");
    s3.delete_object()
        .bucket(bucket)
        .key(&manifest_key)
        .send()
        .await
        .with_context(|| format!("failed to delete s3://{bucket}/{manifest_key}"))?;
    println!("  ✓ Manifest removed from S3");

    let artifact_prefix = format!("artifacts/{namespace}/{name}/");
    let mut continuation_token = None;
    loop {
        let response = s3
            .list_objects_v2()
            .bucket(bucket)
            .prefix(&artifact_prefix)
            .set_continuation_token(continuation_token)
            .send()
            .await
            .with_context(|| {
                format!("failed to list config artifacts under s3://{bucket}/{artifact_prefix}")
            })?;
        for object in response.contents() {
            if let Some(key) = object.key() {
                s3.delete_object()
                    .bucket(bucket)
                    .key(key)
                    .send()
                    .await
                    .with_context(|| format!("failed to delete s3://{bucket}/{key}"))?;
            }
        }
        continuation_token = response.next_continuation_token().map(str::to_owned);
        if continuation_token.is_none() {
            break;
        }
    }
    println!("  ✓ Config artifacts removed from S3");

    println!("\n✓ {name} deleted");
    Ok(())
}
