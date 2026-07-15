use anyhow::{Context, Result};
use aws_sdk_ecs::error::ProvideErrorMetadata;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EcsDeletePhase {
    Delete,
    Drain,
    Cleanup,
}

fn ecs_delete_phase(status: Option<&str>) -> Result<EcsDeletePhase> {
    match status {
        Some("ACTIVE") => Ok(EcsDeletePhase::Delete),
        Some("DRAINING") => Ok(EcsDeletePhase::Drain),
        Some("INACTIVE") | None => Ok(EcsDeletePhase::Cleanup),
        Some(other) => anyhow::bail!("unexpected ECS service status during delete: {other}"),
    }
}

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

    let describe_response = ecs
        .describe_services()
        .cluster(cluster)
        .services(&service_name)
        .send()
        .await
        .context("failed to describe ECS service before delete")?;
    let service = describe_response.services().first();
    let registry_arn: Option<String> = service.and_then(|service| {
        service
            .service_registries()
            .first()
            .and_then(|registry| registry.registry_arn())
            .map(str::to_owned)
    });
    let service_status = service.and_then(|service| service.status());
    let delete_phase = ecs_delete_phase(service_status)?;
    let service_needs_delete = delete_phase == EcsDeletePhase::Delete;
    let service_is_draining = delete_phase == EcsDeletePhase::Drain;

    if service_needs_delete {
        let _ = ecs
            .update_service()
            .cluster(cluster)
            .service(&service_name)
            .desired_count(0)
            .send()
            .await;
        println!("  ✓ Scaled to 0");

        match ecs
            .delete_service()
            .cluster(cluster)
            .service(&service_name)
            .force(true)
            .send()
            .await
        {
            Ok(_) => println!("  ✓ ECS service deleted"),
            Err(error) if error.code() == Some("ServiceNotFoundException") => {
                println!("  ✓ ECS service already absent")
            }
            Err(error) => return Err(error).context("failed to delete ECS service"),
        }
    } else if service_is_draining {
        println!("  ✓ ECS service is already draining; resuming delete cleanup");
    } else {
        println!("  ✓ ECS service already absent; resuming dependent cleanup");
    }

    if service_needs_delete || service_is_draining {
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
    }

    if let Err(error) =
        crate::ingress::teardown(aws_config, namespace, name, registry_arn.as_deref()).await
    {
        eprintln!("  ⚠ ingress teardown skipped: {error}");
    }
    if let Err(error) = crate::ingress::delete_api(aws_config, namespace, name).await {
        eprintln!("  ⚠ HTTP API cleanup skipped: {error}");
    }

    let mut cleanup_failures = Vec::new();
    let manifest_key = format!("manifests/{namespace}/{name}.yaml");
    match s3
        .delete_object()
        .bucket(bucket)
        .key(&manifest_key)
        .send()
        .await
    {
        Ok(_) => println!("  ✓ Manifest removed from S3"),
        Err(error) => cleanup_failures.push(format!(
            "failed to delete s3://{bucket}/{manifest_key}: {error}"
        )),
    }

    let artifact_prefix = format!("artifacts/{namespace}/{name}/");
    let mut continuation_token = None;
    loop {
        let response = match s3
            .list_objects_v2()
            .bucket(bucket)
            .prefix(&artifact_prefix)
            .set_continuation_token(continuation_token)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                cleanup_failures.push(format!(
                    "failed to list config artifacts under s3://{bucket}/{artifact_prefix}: {error}"
                ));
                break;
            }
        };
        for object in response.contents() {
            if let Some(key) = object.key() {
                if let Err(error) = s3
                    .delete_object()
                    .bucket(bucket)
                    .key(key)
                    .send()
                    .await
                {
                    cleanup_failures
                        .push(format!("failed to delete s3://{bucket}/{key}: {error}"));
                }
            }
        }
        continuation_token = response.next_continuation_token().map(str::to_owned);
        if continuation_token.is_none() {
            break;
        }
    }
    if cleanup_failures.is_empty() {
        println!("  ✓ Config artifacts removed from S3");
    } else {
        anyhow::bail!(
            "post-delete cleanup incomplete (safe to retry): {}",
            cleanup_failures.join("; ")
        );
    }

    println!("\n✓ {name} deleted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_phase_requests_delete_only_for_active_service() {
        assert_eq!(
            ecs_delete_phase(Some("ACTIVE")).unwrap(),
            EcsDeletePhase::Delete
        );
        assert_eq!(
            ecs_delete_phase(Some("DRAINING")).unwrap(),
            EcsDeletePhase::Drain
        );
        assert_eq!(
            ecs_delete_phase(Some("INACTIVE")).unwrap(),
            EcsDeletePhase::Cleanup
        );
        assert_eq!(ecs_delete_phase(None).unwrap(), EcsDeletePhase::Cleanup);
    }

    #[test]
    fn delete_phase_rejects_unknown_status() {
        assert!(ecs_delete_phase(Some("UNKNOWN")).is_err());
    }
}
