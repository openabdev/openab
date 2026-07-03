use anyhow::{Context, Result};

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
