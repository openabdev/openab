use crate::bootstrap::BootstrapState;
use crate::manifest::{OABFleetManifest, OABServiceManifest, RawManifest, Runtime};
use anyhow::{Context, Result};
use aws_sdk_ecs::types::{
    AssignPublicIp, AwsVpcConfiguration, CapacityProviderStrategyItem, ContainerDefinition,
    KeyValuePair, NetworkConfiguration, RuntimePlatform, Secret,
};
use aws_sdk_s3::primitives::ByteStream;
use std::path::Path;

/// Load bootstrap state to resolve the task execution role ARN (and other
/// networking defaults). Required on `register_task_definition` whenever the
/// task uses ECS-injected secrets (or pulls from a private registry) — ECS
/// rejects the request with "you must also specify a value for
/// 'executionRoleArn'" otherwise.
pub(crate) async fn load_bootstrap_state(config: &aws_config::SdkConfig) -> Option<BootstrapState> {
    let bucket = if let Some(b) = crate::config::OabConfig::load().ok().and_then(|c| c.bucket()) {
        b
    } else {
        let sts = aws_sdk_sts::Client::new(config);
        let identity = match sts.get_caller_identity().send().await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("  ⚠ load_bootstrap_state: STS get_caller_identity failed: {e}");
                return None;
            }
        };
        let account = match identity.account() {
            Some(a) => a.to_string(),
            None => {
                eprintln!("  ⚠ load_bootstrap_state: STS response missing account field");
                return None;
            }
        };
        format!("oab-control-plane-{account}")
    };
    let s3 = aws_sdk_s3::Client::new(config);
    match crate::bootstrap::load_state_pub(&s3, &bucket).await {
        Ok(Some(state)) => Some(state),
        Ok(None) => {
            eprintln!("  ⚠ load_bootstrap_state: no bootstrap state found in s3://{bucket}/bootstrap-state.json (run `oabctl bootstrap` first)");
            None
        }
        Err(e) => {
            eprintln!("  ⚠ load_bootstrap_state: failed to read bootstrap state from s3://{bucket}: {e}");
            None
        }
    }
}

pub async fn run(aws_config: &aws_config::SdkConfig, file_path: &str, sync_config: bool, wait: bool) -> Result<()> {
    let path = Path::new(file_path);
    let manifests = load_manifests(path)?;

    if manifests.is_empty() {
        anyhow::bail!("no manifests found at {}", file_path);
    }

    // --sync: upload local config.toml to S3 configFrom path
    if sync_config {
        let s3 = aws_sdk_s3::Client::new(aws_config);
        for m in &manifests {
            let config_path = path.parent().unwrap_or(Path::new(".")).join("config.toml");
            if config_path.exists() && !m.spec.config_from.is_empty() {
                let body = aws_sdk_s3::primitives::ByteStream::from_path(&config_path).await
                    .context("failed to read local config.toml")?;
                // Parse s3://bucket/key from configFrom
                if let Some(s3_path) = m.spec.config_from.strip_prefix("s3://") {
                    let (bucket, key) = s3_path.split_once('/').context("invalid configFrom S3 URI")?;
                    s3.put_object().bucket(bucket).key(key).body(body).send().await
                        .context("failed to sync config.toml to S3")?;
                    eprintln!("  ⬆ Synced config.toml → {}", m.spec.config_from);
                }
            }
        }
    }

    let ecs = aws_sdk_ecs::Client::new(aws_config);
    let s3 = aws_sdk_s3::Client::new(aws_config);

    // Validate ALL manifests before applying any (prevent partial apply)
    for m in &manifests {
        m.validate()?;
        if matches!(&m.spec.runtime, Runtime::Kubernetes(_)) {
            anyhow::bail!(
                "Kubernetes runtime not yet implemented (manifest: {})",
                m.metadata.name
            );
        }
    }

    for m in &manifests {
        println!("  Applying {} (ECS)...", m.metadata.name);
        apply_ecs(&ecs, &s3, aws_config, m, wait).await?;
    }

    println!("\n{} service(s) applied.", manifests.len());
    Ok(())
}

pub(crate) fn load_manifests(path: &Path) -> Result<Vec<OABServiceManifest>> {
    let mut manifests = Vec::new();
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "yaml" || e == "yml") {
                manifests.extend(parse_manifest_file(&p)?);
            }
        }
    } else {
        manifests.extend(parse_manifest_file(path)?);
    }
    Ok(manifests)
}

/// Parse a YAML file — returns one or more OABServiceManifests (fleet expands to many)
fn parse_manifest_file(path: &Path) -> Result<Vec<OABServiceManifest>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    // Detect kind first
    let raw: RawManifest = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    match raw.kind.as_str() {
        "OABService" => {
            let m: OABServiceManifest = serde_yaml::from_str(&content)
                .with_context(|| format!("failed to parse OABService {}", path.display()))?;
            Ok(vec![m])
        }
        "OABFleet" => {
            let fleet: OABFleetManifest = serde_yaml::from_str(&content)
                .with_context(|| format!("failed to parse OABFleet {}", path.display()))?;
            fleet.validate()?;
            println!("  Fleet '{}': expanding {} agents...", fleet.metadata.name, fleet.spec.agents.len());
            Ok(fleet.expand())
        }
        other => anyhow::bail!("unsupported kind '{}' in {}", other, path.display()),
    }
}

async fn apply_ecs(
    ecs: &aws_sdk_ecs::Client,
    s3: &aws_sdk_s3::Client,
    config: &aws_config::SdkConfig,
    m: &OABServiceManifest,
    wait: bool,
) -> Result<()> {
    let ecs_rt = match &m.spec.runtime {
        Runtime::Ecs(rt) => rt,
        _ => unreachable!(),
    };

    let service_name = m.ecs_service_name();
    let bucket = if let Some(b) = crate::config::OabConfig::load().ok().and_then(|c| c.bucket()) {
        b
    } else {
        // Fallback: derive from account ID
        let sts = aws_sdk_sts::Client::new(config);
        let account = sts.get_caller_identity().send().await
            .ok().and_then(|r| r.account().map(|a| a.to_string()))
            .unwrap_or_else(|| "unknown".to_string());
        format!("oab-control-plane-{account}")
    };

    // Read current generation from S3 manifest (if exists), increment.
    // Also capture whether the *previous* apply had ingress configured, so we
    // can detect "ingress was removed from the manifest" and tear it down
    // below — apply only ever provisioned ingress resources before this, so a
    // manifest edit that drops `spec.ingress` used to orphan the per-bot HTTP
    // API and Cloud Map service.
    let manifest_key = format!("manifests/{}/{}.yaml", m.metadata.namespace, m.metadata.name);
    let (current_gen, previously_had_ingress) =
        match s3.get_object().bucket(&bucket).key(&manifest_key).send().await {
            Ok(resp) => {
                let bytes = resp.body.collect().await?.into_bytes();
                let existing: OABServiceManifest = serde_yaml::from_slice(&bytes)?;
                (existing.metadata.generation, existing.spec.ingress.is_some())
            }
            Err(_) => (0, false),
        };
    let generation = current_gen + 1;

    // Resolve cluster from config (same source as `get` and `delete` commands).
    // ~/.oabctl/config.toml defaults.cluster; falls back to "oab".
    // Propagate config load errors (same behavior as get/delete) — silently
    // defaulting to "oab" on a malformed config could target the wrong cluster.
    let oab_cfg = crate::config::OabConfig::load()
        .context("failed to load ~/.oabctl/config.toml (run `oabctl bootstrap` first)")?;
    let cluster = &oab_cfg.defaults.cluster;

    // Look up the ECS service's current registry ARN(s) up front so both the
    // ingress-removal teardown below and the update/create logic further down
    // can use the *exact* registry rather than falling back to a name-only
    // Cloud Map scan (which can collide across VPCs/environments that share
    // an account and reuse the same namespace/name).
    let describe_resp = ecs
        .describe_services()
        .cluster(cluster.as_str())
        .services(&service_name)
        .send()
        .await;
    let existing_registry_arns: Vec<String> = describe_resp
        .as_ref()
        .ok()
        .and_then(|r| r.services().first())
        .map(|s| {
            s.service_registries()
                .iter()
                .filter_map(|r| r.registry_arn())
                .map(|a| a.to_string())
                .collect()
        })
        .unwrap_or_default();
    let has_registries = !existing_registry_arns.is_empty();

    // If ingress was configured before but is absent now, tear down the
    // orphaned per-bot ingress resources (best-effort, mirrors `oabctl delete`)
    // and detach the stale registry from the ECS service itself — omitting
    // `serviceRegistries` on `UpdateService` leaves the existing configuration
    // untouched (AWS only clears it when explicitly passed an empty list), so
    // without this the service would keep pointing at a Cloud Map service that
    // teardown() is about to delete.
    if previously_had_ingress && m.spec.ingress.is_none() {
        eprintln!("  🌐 ingress removed from manifest — tearing down orphaned resources...");
        if let Err(e) = crate::ingress::teardown(
            config,
            &m.metadata.namespace,
            &m.metadata.name,
            existing_registry_arns.first().map(|s| s.as_str()),
        )
        .await
        {
            eprintln!("  ⚠ ingress teardown skipped: {e}");
        }
    }

    // 1. Upload manifest to S3 (record of desired state)
    let mut manifest_to_store = serde_yaml::to_value(m)?;
    manifest_to_store["metadata"]["generation"] = serde_yaml::Value::Number(generation.into());
    let manifest_yaml = serde_yaml::to_string(&manifest_to_store)?;
    s3.put_object()
        .bucket(&bucket)
        .key(&manifest_key)
        .body(ByteStream::from(manifest_yaml.into_bytes()))
        .send()
        .await
        .context("failed to upload manifest to S3")?;

    // 2. Build environment variables
    let mut env_vars = vec![
        KeyValuePair::builder().name("NAMESPACE").value(&m.metadata.namespace).build(),
        KeyValuePair::builder().name("NAME").value(&m.metadata.name).build(),
    ];
    // openab's own AWS SDK calls (config-s3 loading, secrets resolution, etc.)
    // resolve region via the standard chain: AWS_REGION env var → profile →
    // IMDS. Fargate tasks have no EC2 instance metadata to fall back to, so
    // without this the SDK can fail to resolve an endpoint at all.
    // Region is injected below after bootstrap_state is loaded (to allow
    // fallback to bootstrap_state.region when config.region() is None).
    if let Some(ref bootstrap) = m.spec.bootstrap_from {
        env_vars.push(KeyValuePair::builder().name("BOOTSTRAP_FROM").value(bootstrap).build());
    }

    // 3. Build secrets from map. Values can be either the ECS-native
    //    `valueFrom` format directly (a Secrets Manager ARN, optionally with
    //    a `:<jsonKey>::` suffix), or the same `aws-sm://<secret-id>#<json-key>`
    //    shorthand openab itself uses for in-app secret refs — resolved here
    //    into the ECS-native form ECS actually requires, since ECS has no
    //    knowledge of that scheme.
    let sm = aws_sdk_secretsmanager::Client::new(config);
    let mut secrets: Vec<Secret> = Vec::with_capacity(m.spec.secrets.len());
    for (name, value) in &m.spec.secrets {
        let value_from = crate::secrets::resolve_value_from(&sm, value).await?;
        secrets.push(Secret::builder().name(name).value_from(value_from).build().unwrap());
    }

    // Load bootstrap state — supplies CloudWatch log group, execution role
    // ARN, task role ARN, and region fallback.
    let bootstrap_state = load_bootstrap_state(config).await;

    // Resolve effective region: prefer SDK config, fall back to bootstrap
    // state's recorded region. Fargate has no IMDS, so without AWS_REGION the
    // container's SDK calls will fail to resolve endpoints entirely.
    let effective_region: Option<String> = config.region()
        .map(|r| r.as_ref().to_string())
        .or_else(|| bootstrap_state.as_ref().map(|s| s.region.clone()));
    if let Some(ref region) = effective_region {
        env_vars.push(KeyValuePair::builder().name("AWS_REGION").value(region).build());
    }

    let mut container = ContainerDefinition::builder()
        .name("openab")
        .image(&m.spec.image)
        .essential(true)
        .set_environment(Some(env_vars))
        .set_secrets(if secrets.is_empty() { None } else { Some(secrets) });

    // The image's default CMD points `openab` at a local
    // /etc/openab/config.toml that nothing populates. openab has native
    // s3:// config-source support (built with the `config-s3` feature,
    // included in the default feature set + `unified`), so override the
    // command to load configFrom directly instead — no download step,
    // sidecar, or entrypoint script needed. Uses the task role's existing
    // s3:GetObject grant on `{bucket}/artifacts/*`.
    if !m.spec.config_from.is_empty() {
        container = container.set_command(Some(vec![
            "openab".to_string(),
            "run".to_string(),
            "-c".to_string(),
            m.spec.config_from.clone(),
        ]));
    }

    // Ship container stdout/stderr to the log group bootstrap created, so a
    // crashing/misbehaving container is actually diagnosable. Without this,
    // ECS uses no log driver and task failures are opaque (no log stream at
    // all, not even an empty one).
    if let Some(log_group) = bootstrap_state.as_ref().map(|s| &s.resources.log_group) {
        if let Some(ref region) = effective_region {
            container = container.log_configuration(
                aws_sdk_ecs::types::LogConfiguration::builder()
                    .log_driver(aws_sdk_ecs::types::LogDriver::Awslogs)
                    .options("awslogs-group", log_group.as_str())
                    .options("awslogs-region", region.as_str())
                    .options("awslogs-stream-prefix", &service_name)
                    .options("awslogs-create-group", "true")
                    .build()?,
            );
        }
    }

    // Ingress needs the container port exposed so ECS can register an SRV record
    // (Cloud Map + API Gateway learn the target port from it).
    if let Some(ingress) = &m.spec.ingress {
        container = container.port_mappings(
            aws_sdk_ecs::types::PortMapping::builder()
                .container_port(ingress.container_port as i32)
                .protocol(aws_sdk_ecs::types::TransportProtocol::Tcp)
                .build(),
        );
    }

    let container = container.build();

    // ECS requires executionRoleArn whenever the task definition uses
    // container secrets (or a private registry) — resolve it from bootstrap
    // state rather than requiring it in the manifest, matching how the
    // task role / cluster / subnets are already sourced from bootstrap.
    //
    // taskRoleArn is separate and equally required: ECS only provisions the
    // AWS_CONTAINER_CREDENTIALS_RELATIVE_URI endpoint (and injects that env
    // var into the container) when a task role is set on the task
    // definition. Without it, the running `openab` process has no AWS
    // credentials at all for its own SDK calls (fetching configFrom from S3,
    // resolving spec.secrets values via aws-sm:// refs, etc.) — it falls
    // through envvar/profile/webidentity/ECS providers and finally tries
    // IMDS, which doesn't exist on Fargate, and fails with a generic
    // "dispatch failure". This was previously never set at all.
    let execution_role_arn = bootstrap_state.as_ref().map(|s| s.resources.execution_role_arn.clone());
    // Manifest task_role_arn takes precedence over bootstrap shared role.
    // Filter empty strings so a blank value falls through to bootstrap.
    let task_role_arn = resolve_task_role_arn(
        &ecs_rt.task_role_arn,
        bootstrap_state.as_ref().map(|s| s.resources.task_role_arn.as_str()),
    );
    match (&task_role_arn, ecs_rt.task_role_arn.as_deref().filter(|s| !s.is_empty())) {
        (Some(arn), Some(_)) => eprintln!("  ℹ taskRoleArn: {arn} (from manifest)"),
        (Some(arn), None) => eprintln!("  ℹ taskRoleArn: {arn} (from bootstrap)"),
        (None, _) => eprintln!("  ⚠ no taskRoleArn resolved — task will have no IAM role"),
    }

    let mut register_req = ecs
        .register_task_definition()
        .family(&service_name)
        .requires_compatibilities(aws_sdk_ecs::types::Compatibility::Fargate)
        .network_mode(aws_sdk_ecs::types::NetworkMode::Awsvpc)
        .cpu(&m.spec.resources.cpu)
        .memory(&m.spec.resources.memory)
        .container_definitions(container);
    if let Some(arn) = &execution_role_arn {
        register_req = register_req.execution_role_arn(arn);
    } else if !m.spec.secrets.is_empty() {
        anyhow::bail!(
            "spec.secrets is set but no bootstrap execution role was found — run `oabctl bootstrap` first, or ECS will reject task registration"
        );
    }
    if let Some(arn) = &task_role_arn {
        register_req = register_req.task_role_arn(arn);
    } else {
        anyhow::bail!(
            "no bootstrap task role was found — run `oabctl bootstrap` first, or the running container will have no AWS credentials"
        );
    }

    // Set runtime platform (OS + CPU architecture) — required for Fargate to
    // schedule on Graviton (ARM64) vs Intel/AMD (X86_64).
    let cpu_arch = match ecs_rt.architecture.as_str() {
        "ARM64" => aws_sdk_ecs::types::CpuArchitecture::Arm64,
        "X86_64" => aws_sdk_ecs::types::CpuArchitecture::X8664,
        other => anyhow::bail!("unsupported architecture '{other}' — should be caught by manifest validation"),
    };
    register_req = register_req.runtime_platform(
        RuntimePlatform::builder()
            .operating_system_family(aws_sdk_ecs::types::OsFamily::Linux)
            .cpu_architecture(cpu_arch)
            .build(),
    );

    let task_def = register_req
        .send()
        .await
        .context("failed to register task definition")?;

    let task_def_arn = task_def
        .task_definition()
        .and_then(|td| td.task_definition_arn())
        .unwrap_or_default()
        .to_string();

    // 5. Create or update ECS service
    let assign_ip = if ecs_rt.networking.assign_public_ip {
        AssignPublicIp::Enabled
    } else {
        AssignPublicIp::Disabled
    };

    let vpc_config = AwsVpcConfiguration::builder()
        .set_subnets(Some(ecs_rt.networking.subnets.clone()))
        .set_security_groups(Some(ecs_rt.networking.security_groups.clone()))
        .assign_public_ip(assign_ip)
        .build()?;

    let network_config = NetworkConfiguration::builder()
        .awsvpc_configuration(vpc_config)
        .build();

    // Ingress: ensure Cloud Map BEFORE the service exists-check, so the
    // registry ARN is ready whether the ECS service needs to be created (via
    // `create_service`) or updated to attach/replace service discovery (via
    // `update_service` — ECS has supported changing `serviceRegistries` on an
    // existing service since March 2022; no delete-and-recreate is needed).
    let cloud_map = if let Some(ingress) = &m.spec.ingress {
        eprintln!("  🌐 Reconciling ingress (Cloud Map)...");
        let cm = crate::ingress::ensure_cloud_map(config, m, ingress).await?;
        Some(cm)
    } else {
        None
    };

    // Check if service exists. Reuses `describe_resp` captured above (before
    // the ingress-removal teardown) — `ensure_cloud_map` above doesn't touch
    // the ECS service, so its ACTIVE status can't have changed since then.
    let service_active = describe_resp
        .as_ref()
        .ok()
        .and_then(|r| r.services().first())
        .is_some_and(|s| s.status() == Some("ACTIVE"));

    if service_active {
        // Recreate is NOT required to attach/fix service discovery: ECS's
        // UpdateService API has supported adding/updating/removing
        // serviceRegistries since March 2022 (rolling replacement — new tasks
        // start with the updated registry, old tasks stop once they're
        // healthy, no downtime gap). It does require the AWSServiceRoleForECS
        // service-linked role, which ECS creates automatically the first time
        // any account uses ECS service discovery — no action needed here.
        let registry_mismatch = cloud_map.as_ref().is_some_and(|cm| {
            has_registries && !existing_registry_arns.contains(&cm.registry_arn)
        });
        // `ingress` was removed from the manifest (cloud_map is None here)
        // but the ECS service still has a registry attached from a previous
        // apply — must explicitly detach it. `UpdateService` treats an
        // *omitted* `serviceRegistries` field as "leave unchanged", not
        // "clear"; only an explicit empty list detaches it. Without this the
        // service keeps pointing at the Cloud Map service that the
        // ingress-removal teardown (above) just deleted.
        let needs_detach = cloud_map.is_none() && has_registries;

        let mut update_req = ecs
            .update_service()
            .cluster(cluster.as_str())
            .service(&service_name)
            .task_definition(&task_def_arn)
            .enable_execute_command(true)
            .network_configuration(network_config);

        if let Some(cm) = &cloud_map {
            if !has_registries || registry_mismatch {
                let mut registry = aws_sdk_ecs::types::ServiceRegistry::builder()
                    .registry_arn(&cm.registry_arn);
                if let Some(ingress) = &m.spec.ingress {
                    registry = registry
                        .container_name("openab")
                        .container_port(ingress.container_port as i32);
                }
                update_req = update_req.service_registries(registry.build());
            }
        } else if needs_detach {
            update_req = update_req.set_service_registries(Some(Vec::new()));
        }

        update_req
            .send()
            .await
            .context("failed to update ECS service")?;

        if cloud_map.is_some() && (!has_registries || registry_mismatch) {
            if registry_mismatch {
                println!(
                    "  ✓ {} updated (service discovery re-pointed to the current Cloud Map service; rolling replacement, no downtime)",
                    m.metadata.name
                );
            } else {
                println!(
                    "  ✓ {} updated (service discovery attached; rolling replacement, no downtime)",
                    m.metadata.name
                );
            }
        } else if needs_detach {
            println!(
                "  ✓ {} updated (service discovery detached; rolling replacement, no downtime)",
                m.metadata.name
            );
        } else {
            println!("  ✓ {} updated", m.metadata.name);
        }
    } else {
        let cap_strategy = CapacityProviderStrategyItem::builder()
            .capacity_provider(&ecs_rt.capacity_provider)
            .weight(1)
            .build()?;

        let mut create_req = ecs
            .create_service()
            .cluster(cluster.as_str())
            .service_name(&service_name)
            .task_definition(&task_def_arn)
            .desired_count(1)
            .enable_execute_command(true)
            .capacity_provider_strategy(cap_strategy)
            .network_configuration(network_config);

        if let Some(cm) = &cloud_map {
            let mut registry = aws_sdk_ecs::types::ServiceRegistry::builder()
                .registry_arn(&cm.registry_arn);
            // SRV records require the container name + port so ECS registers the
            // task's port alongside its IP.
            if let Some(ingress) = &m.spec.ingress {
                registry = registry
                    .container_name("openab")
                    .container_port(ingress.container_port as i32);
            }
            create_req = create_req.service_registries(registry.build());
        }

        // Retry with backoff if ECS reports "still Draining" (race with a
        // recent delete that hasn't fully completed yet).
        // Match on the typed error code (InvalidParameterException) rather than
        // raw message text to be resilient to SDK/API wording changes.
        use aws_sdk_ecs::error::ProvideErrorMetadata;
        const DRAIN_RETRY_ATTEMPTS: u32 = 12;
        const DRAIN_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

        for attempt in 0..DRAIN_RETRY_ATTEMPTS {
            match create_req.clone().send().await {
                Ok(_) => {
                    if attempt > 0 {
                        eprintln!(" ok");
                    }
                    break;
                }
                Err(e) => {
                    let is_draining = e.code() == Some("InvalidParameterException")
                        && e.message()
                            .unwrap_or_default()
                            .to_lowercase()
                            .contains("draining");
                    let is_last = attempt == DRAIN_RETRY_ATTEMPTS - 1;
                    if is_draining && !is_last {
                        if attempt == 0 {
                            eprint!("  ⏳ Service still draining, retrying...");
                        } else {
                            eprint!(".");
                        }
                        tokio::time::sleep(DRAIN_RETRY_INTERVAL).await;
                    } else {
                        if attempt > 0 {
                            eprintln!(" failed");
                        }
                        let ctx = if is_last && is_draining {
                            "failed to create ECS service after retries (service still draining)"
                        } else {
                            "failed to create ECS service"
                        };
                        return Err(e).context(ctx);
                    }
                }
            }
        }
        println!(
            "  ✓ {} created ({}, {}cpu/{}mem{})",
            m.metadata.name,
            ecs_rt.capacity_provider,
            m.spec.resources.cpu,
            m.spec.resources.memory,
            if cloud_map.is_some() {
                ", service discovery"
            } else {
                ""
            }
        );
    }

    // Ingress step 2: VPC Link + API Gateway + routes + SG rule.
    if let (Some(ingress), Some(cm)) = (&m.spec.ingress, &cloud_map) {
        eprintln!("  🌐 Reconciling ingress (VPC Link + API Gateway)...");
        let urls = crate::ingress::ensure_gateway(
            config,
            &m.metadata.namespace,
            &m.metadata.name,
            ingress,
            &ecs_rt.networking.subnets,
            &ecs_rt.networking.security_groups,
            &cm.registry_arn,
        )
        .await?;
        println!("  🔗 Webhook URL(s) for {}:", m.metadata.name);
        for u in &urls {
            println!("     {u}");
        }

        // Best-effort: register the Telegram webhook so the bot starts
        // receiving updates without a manual `curl setWebhook` step. Only
        // fires when `/webhook/telegram` is one of the ingress paths and
        // spec.secrets has a TELEGRAM_BOT_TOKEN entry; a no-op otherwise.
        // Never fails `apply` — the AWS provisioning above already
        // succeeded, and this is a convenience on top of it.
        let path_urls: Vec<(String, String)> =
            ingress.paths.iter().cloned().zip(urls.iter().cloned()).collect();
        match crate::ingress::register_telegram_webhook(config, &m.spec.secrets, &path_urls).await
        {
            Ok(Some(desc)) => eprintln!("  ✓ Telegram webhook registered: {desc}"),
            Ok(None) => {}
            Err(e) => eprintln!("  ⚠ Telegram webhook registration failed (apply still succeeded): {e}"),
        }
    }

    if wait {
        eprintln!("  ⏳ Waiting for {} to stabilize...", m.metadata.name);
        wait_for_stable(ecs, cluster, &service_name).await?;
        eprintln!("  ✓ {} is stable", m.metadata.name);
    }

    Ok(())
}

/// Poll until the ECS service's deployment stabilizes, printing each
/// transition as a composite status string — same vocabulary `ecsctl`
/// itself uses for `get`/`alias ls` (github.com/oablab/ecsctl,
/// src/alias.rs): `RUNNING`, `REPLACING(n→m)` (new deployment's tasks still
/// coming up), `DRAINING(n+m)` (new deployment up, old one's tasks still
/// stopping), `PENDING(n)`, `PARTIAL(n/m)`, or the raw ECS service status as
/// a fallback — reused here for a consistent status vocabulary across both
/// tools instead of raw `running_count`/`rollout_state` fields.
async fn wait_for_stable(ecs: &aws_sdk_ecs::Client, cluster: &str, service: &str) -> Result<()> {
    for i in 0..60 {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let resp = ecs.describe_services()
            .cluster(cluster)
            .services(service)
            .send().await?;
        let elapsed = (i + 1) * 5;

        let Some(svc) = resp.services().first() else {
            eprintln!("    [{elapsed}s] service not found in describe-services response yet");
            continue;
        };

        let running = svc.running_count() as usize;
        let desired = svc.desired_count() as usize;
        let pending = svc.pending_count() as usize;
        let deployments = svc.deployments();
        let num_deployments = deployments.len();
        let primary = deployments
            .iter()
            .find(|d| d.status().unwrap_or_default() == "PRIMARY")
            .or_else(|| deployments.first());

        let status = if desired == 0 {
            "STOPPED".to_string()
        } else if running == desired && pending == 0 && num_deployments <= 1 {
            "RUNNING".to_string()
        } else if num_deployments > 1 {
            if let Some(p) = primary {
                let p_running = p.running_count() as usize;
                let p_desired = p.desired_count() as usize;
                if p_running < p_desired {
                    format!("REPLACING({p_running}→{p_desired})")
                } else {
                    let old_running: usize = deployments
                        .iter()
                        .filter(|d| d.status().unwrap_or_default() != "PRIMARY")
                        .map(|d| d.running_count() as usize)
                        .sum();
                    format!("DRAINING({p_running}+{old_running})")
                }
            } else {
                svc.status().unwrap_or("UNKNOWN").to_string()
            }
        } else if pending > 0 {
            format!("PENDING({pending})")
        } else if running < desired {
            format!("PARTIAL({running}/{desired})")
        } else {
            svc.status().unwrap_or("UNKNOWN").to_string()
        };

        eprintln!("    [{elapsed}s] {status}");

        if status == "RUNNING" {
            return Ok(());
        }
    }
    anyhow::bail!("timed out waiting for service to stabilize (5 min)")
}

/// Resolve the effective task role ARN.
///
/// Resolution order:
/// 1. Manifest `taskRoleArn` (if present and non-empty) → use it
/// 2. Bootstrap shared task role → fallback
/// 3. Neither → `None`
fn resolve_task_role_arn(
    manifest_role: &Option<String>,
    bootstrap_role: Option<&str>,
) -> Option<String> {
    manifest_role
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| bootstrap_role.map(|s| s.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_role_manifest_wins_over_bootstrap() {
        let manifest = Some("arn:aws:iam::111:role/manifest-role".to_string());
        let bootstrap = Some("arn:aws:iam::111:role/bootstrap-role");
        let result = resolve_task_role_arn(&manifest, bootstrap);
        assert_eq!(
            result.as_deref(),
            Some("arn:aws:iam::111:role/manifest-role")
        );
    }

    #[test]
    fn task_role_falls_back_to_bootstrap() {
        let manifest: Option<String> = None;
        let bootstrap = Some("arn:aws:iam::111:role/bootstrap-role");
        let result = resolve_task_role_arn(&manifest, bootstrap);
        assert_eq!(
            result.as_deref(),
            Some("arn:aws:iam::111:role/bootstrap-role")
        );
    }

    #[test]
    fn task_role_manifest_only_no_bootstrap() {
        let manifest = Some("arn:aws:iam::111:role/manifest-role".to_string());
        let bootstrap: Option<&str> = None;
        let result = resolve_task_role_arn(&manifest, bootstrap);
        assert_eq!(
            result.as_deref(),
            Some("arn:aws:iam::111:role/manifest-role")
        );
    }

    #[test]
    fn task_role_none_when_both_absent() {
        let manifest: Option<String> = None;
        let bootstrap: Option<&str> = None;
        let result = resolve_task_role_arn(&manifest, bootstrap);
        assert_eq!(result, None);
    }

    #[test]
    fn task_role_empty_string_falls_through_to_bootstrap() {
        let manifest = Some("".to_string());
        let bootstrap = Some("arn:aws:iam::111:role/bootstrap-role");
        let result = resolve_task_role_arn(&manifest, bootstrap);
        assert_eq!(
            result.as_deref(),
            Some("arn:aws:iam::111:role/bootstrap-role"),
            "empty string in manifest should not override bootstrap"
        );
    }

    #[test]
    fn task_role_empty_string_no_bootstrap_returns_none() {
        let manifest = Some("".to_string());
        let bootstrap: Option<&str> = None;
        let result = resolve_task_role_arn(&manifest, bootstrap);
        assert_eq!(result, None);
    }
}
