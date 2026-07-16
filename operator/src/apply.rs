use crate::bootstrap::BootstrapState;
use crate::manifest::{OABFleetManifest, OABServiceManifest, RawManifest, Runtime};
use anyhow::{Context, Result};
use aws_sdk_ecs::types::{
    AssignPublicIp, AwsVpcConfiguration, CapacityProviderStrategyItem, ContainerDefinition,
    KeyValuePair, NetworkConfiguration, RuntimePlatform, Secret,
};
use aws_sdk_s3::primitives::ByteStream;
use std::fmt;
use std::path::Path;

// Progress rendering is scoped to the current async task. Library calls set it
// to false, while CLI/delete callers retain the existing rendering behavior.
tokio::task_local! {
    static PROGRESS_ENABLED: bool;
}

pub(crate) fn progress_enabled() -> bool {
    PROGRESS_ENABLED
        .try_with(|enabled| *enabled)
        .unwrap_or(true)
}

/// Run `future` with human-readable progress output suppressed (library
/// callers). Shared by the programmatic apply and delete entry points.
pub(crate) async fn with_progress_suppressed<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    PROGRESS_ENABLED.scope(false, future).await
}

macro_rules! println {
    ($($arg:tt)*) => {{ if progress_enabled() { std::println!($($arg)*); } }};
}
macro_rules! eprintln {
    ($($arg:tt)*) => {{ if progress_enabled() { std::eprintln!($($arg)*); } }};
}
macro_rules! eprint {
    ($($arg:tt)*) => {{ if progress_enabled() { std::eprint!($($arg)*); } }};
}

/// Whether a service was created or updated by reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyAction {
    Created,
    Updated,
}

/// Stable identity for a service targeted by apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceTarget {
    pub namespace: String,
    pub name: String,
    pub ecs_service_name: String,
}

impl From<&OABServiceManifest> for ServiceTarget {
    fn from(manifest: &OABServiceManifest) -> Self {
        Self {
            namespace: manifest.metadata.namespace.clone(),
            name: manifest.metadata.name.clone(),
            ecs_service_name: manifest.ecs_service_name(),
        }
    }
}

/// Reconciliation outcome for one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedService {
    pub namespace: String,
    pub name: String,
    pub ecs_service_name: String,
    pub action: ApplyAction,
    pub webhook_urls: Vec<String>,
    pub warnings: Vec<String>,
}

/// Structured result of a successful (or partially completed) apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub services: Vec<AppliedService>,
}

/// High-level phase in which apply failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyErrorKind {
    Validation,
    Target,
    Reconciliation,
}

/// Structured apply failure. Reconciliation failures identify the failed
/// service and retain the report for all services completed before it.
#[derive(Debug)]
pub struct ApplyError {
    pub kind: ApplyErrorKind,
    pub failed_service: Option<ServiceTarget>,
    pub completed: ApplyReport,
    source: anyhow::Error,
}

impl ApplyError {
    fn validation(source: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: ApplyErrorKind::Validation,
            failed_service: None,
            completed: ApplyReport::default(),
            source: source.into(),
        }
    }

    fn target(source: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: ApplyErrorKind::Target,
            failed_service: None,
            completed: ApplyReport::default(),
            source: source.into(),
        }
    }

    fn reconciliation(
        failed_service: ServiceTarget,
        completed: ApplyReport,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            kind: ApplyErrorKind::Reconciliation,
            failed_service: Some(failed_service),
            completed,
            source: source.into(),
        }
    }

    pub fn source_error(&self) -> &anyhow::Error {
        &self.source
    }
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.failed_service {
            Some(service) => write!(
                f,
                "apply {:?} error for {}/{}: {}",
                self.kind, service.namespace, service.name, self.source
            ),
            None => write!(f, "apply {:?} error: {}", self.kind, self.source),
        }
    }
}

impl std::error::Error for ApplyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Target options for [`apply_manifests`]. The cluster is deliberately
/// required; the library never guesses a default deployment target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOptions {
    /// ECS cluster name or ARN. The cluster must already exist and be active.
    pub cluster: String,
    /// Optional control-plane bucket override. When absent, the library uses
    /// `OAB_CONTROL_PLANE_BUCKET`, then derives `oab-control-plane-{account}`
    /// from the caller's AWS identity. It never reads CLI home configuration.
    pub control_plane_bucket: Option<String>,
    /// Wait for every reconciled ECS service to stabilize before returning.
    pub wait: bool,
}

impl ApplyOptions {
    /// Create apply options for an explicit ECS cluster.
    pub fn new(cluster: impl Into<String>) -> Self {
        Self {
            cluster: cluster.into(),
            control_plane_bucket: None,
            wait: false,
        }
    }

    /// Override the S3 bucket used for bootstrap state and desired manifests.
    pub fn with_control_plane_bucket(mut self, bucket: impl Into<String>) -> Self {
        self.control_plane_bucket = Some(bucket.into());
        self
    }

    /// Configure whether apply waits for ECS deployment stabilization.
    pub fn with_wait(mut self, wait: bool) -> Self {
        self.wait = wait;
        self
    }
}

struct BootstrapResolution {
    state: Option<BootstrapState>,
    warning: Option<String>,
}

struct PreparedApply {
    bucket: String,
    bootstrap: BootstrapResolution,
}

async fn load_bootstrap_state(s3: &aws_sdk_s3::Client, bucket: &str) -> BootstrapResolution {
    match crate::bootstrap::load_state_pub(s3, bucket).await {
        Ok(Some(state)) => BootstrapResolution {
            state: Some(state),
            warning: None,
        },
        Ok(None) => BootstrapResolution {
            state: None,
            warning: Some(format!(
                "no bootstrap state found in s3://{bucket}/bootstrap-state.json (run `oabctl bootstrap` first)"
            )),
        },
        Err(error) => BootstrapResolution {
            state: None,
            warning: Some(format!(
                "failed to read bootstrap state from s3://{bucket}: {error}"
            )),
        },
    }
}

fn validate_apply_request(
    manifests: &[OABServiceManifest],
    cluster: &str,
) -> std::result::Result<(), ApplyError> {
    if manifests.is_empty() {
        return Err(ApplyError::validation(anyhow::anyhow!(
            "no manifests to apply (empty manifest set)"
        )));
    }
    if cluster.trim().is_empty() {
        return Err(ApplyError::validation(anyhow::anyhow!(
            "ApplyOptions.cluster must not be empty or whitespace"
        )));
    }
    for manifest in manifests {
        manifest.validate().map_err(|error| {
            ApplyError::validation(error.context(format!(
                "invalid manifest {}/{}",
                manifest.metadata.namespace, manifest.metadata.name
            )))
        })?;
        if matches!(&manifest.spec.runtime, Runtime::Kubernetes(_)) {
            return Err(ApplyError::validation(anyhow::anyhow!(
                "Kubernetes runtime not yet implemented (manifest: {})",
                manifest.metadata.name
            )));
        }
    }
    Ok(())
}

fn classify_cluster_response(
    requested: &str,
    clusters: &[(&str, &str, &str)],
    failures: &[String],
) -> std::result::Result<(), String> {
    if !failures.is_empty() {
        return Err(format!(
            "ECS rejected cluster '{requested}': {}",
            failures.join("; ")
        ));
    }
    let Some((_, _, status)) = clusters
        .iter()
        .find(|(name, arn, _)| *name == requested || *arn == requested)
    else {
        return Err(format!(
            "ECS cluster '{requested}' was not returned by DescribeClusters"
        ));
    };
    if *status != "ACTIVE" {
        return Err(format!(
            "ECS cluster '{requested}' is not reachable for apply (status: {})",
            if status.is_empty() { "unknown" } else { status }
        ));
    }
    Ok(())
}

async fn validate_cluster(
    ecs: &aws_sdk_ecs::Client,
    cluster: &str,
) -> std::result::Result<(), ApplyError> {
    let response = ecs
        .describe_clusters()
        .clusters(cluster)
        .send()
        .await
        .map_err(|error| {
            ApplyError::target(
                anyhow::Error::new(error)
                    .context(format!("failed to describe ECS cluster '{cluster}'")),
            )
        })?;
    let clusters: Vec<(&str, &str, &str)> = response
        .clusters()
        .iter()
        .map(|item| {
            (
                item.cluster_name().unwrap_or_default(),
                item.cluster_arn().unwrap_or_default(),
                item.status().unwrap_or_default(),
            )
        })
        .collect();
    let failures: Vec<String> = response
        .failures()
        .iter()
        .map(|failure| {
            let target = failure.arn().unwrap_or(cluster);
            let reason = failure.reason().unwrap_or("unknown failure");
            match failure.detail() {
                Some(detail) if !detail.is_empty() => format!("{target}: {reason} ({detail})"),
                _ => format!("{target}: {reason}"),
            }
        })
        .collect();
    classify_cluster_response(cluster, &clusters, &failures)
        .map_err(|message| ApplyError::target(anyhow::anyhow!(message)))
}

async fn prepare_apply(
    aws_config: &aws_config::SdkConfig,
    ecs: &aws_sdk_ecs::Client,
    s3: &aws_sdk_s3::Client,
    manifests: &[OABServiceManifest],
    cluster: &str,
    configured_bucket: Option<&str>,
) -> std::result::Result<PreparedApply, ApplyError> {
    validate_apply_request(manifests, cluster)?;
    validate_cluster(ecs, cluster).await?;
    let bucket = crate::control_plane::resolve_bucket(aws_config, configured_bucket)
        .await
        .map_err(ApplyError::target)?;
    let bootstrap = load_bootstrap_state(s3, &bucket).await;
    Ok(PreparedApply { bucket, bootstrap })
}

pub(crate) async fn run(
    aws_config: &aws_config::SdkConfig,
    file_path: &str,
    sync_config: bool,
    wait: bool,
) -> Result<()> {
    let path = Path::new(file_path);
    let manifests = load_manifests(path)?;
    let oab_cfg = crate::config::OabConfig::load()
        .context("failed to load ~/.oabctl/config.toml (run `oabctl bootstrap` first)")?;
    let cluster = &oab_cfg.defaults.cluster;
    let ecs = aws_sdk_ecs::Client::new(aws_config);
    let s3 = aws_sdk_s3::Client::new(aws_config);

    // Local validation and DescribeClusters happen before config sync or any
    // other mutating request.
    let prepared = prepare_apply(
        aws_config,
        &ecs,
        &s3,
        &manifests,
        cluster,
        oab_cfg.bootstrap.bucket.as_deref(),
    )
    .await?;

    if sync_config {
        for manifest in &manifests {
            let config_path = path.parent().unwrap_or(Path::new(".")).join("config.toml");
            if config_path.exists() && !manifest.spec.config_from.is_empty() {
                let body = ByteStream::from_path(&config_path)
                    .await
                    .context("failed to read local config.toml")?;
                if let Some(s3_path) = manifest.spec.config_from.strip_prefix("s3://") {
                    let (bucket, key) = s3_path
                        .split_once('/')
                        .context("invalid configFrom S3 URI")?;
                    s3.put_object()
                        .bucket(bucket)
                        .key(key)
                        .body(body)
                        .send()
                        .await
                        .context("failed to sync config.toml to S3")?;
                    eprintln!("  ⬆ Synced config.toml → {}", manifest.spec.config_from);
                }
            }
        }
    }

    apply_manifests_prepared(aws_config, &ecs, &s3, &manifests, cluster, wait, &prepared).await?;
    Ok(())
}

/// Validate and reconcile in-memory manifests without writing progress to
/// process-global stdout or stderr.
pub async fn apply_manifests(
    aws_config: &aws_config::SdkConfig,
    manifests: &[OABServiceManifest],
    opts: &ApplyOptions,
) -> std::result::Result<ApplyReport, ApplyError> {
    PROGRESS_ENABLED
        .scope(false, async {
            validate_apply_request(manifests, &opts.cluster)?;
            let ecs = aws_sdk_ecs::Client::new(aws_config);
            let s3 = aws_sdk_s3::Client::new(aws_config);
            validate_cluster(&ecs, &opts.cluster).await?;
            let bucket = crate::control_plane::resolve_bucket(
                aws_config,
                opts.control_plane_bucket.as_deref(),
            )
            .await
            .map_err(ApplyError::target)?;
            let prepared = PreparedApply {
                bootstrap: load_bootstrap_state(&s3, &bucket).await,
                bucket,
            };
            apply_manifests_prepared(
                aws_config,
                &ecs,
                &s3,
                manifests,
                &opts.cluster,
                opts.wait,
                &prepared,
            )
            .await
        })
        .await
}

async fn apply_manifests_prepared(
    aws_config: &aws_config::SdkConfig,
    ecs: &aws_sdk_ecs::Client,
    s3: &aws_sdk_s3::Client,
    manifests: &[OABServiceManifest],
    cluster: &str,
    wait: bool,
    prepared: &PreparedApply,
) -> std::result::Result<ApplyReport, ApplyError> {
    let mut report = ApplyReport::default();
    for manifest in manifests {
        println!("  Applying {} (ECS)...", manifest.metadata.name);
        match apply_ecs(ecs, s3, aws_config, manifest, cluster, wait, prepared).await {
            Ok(service) => report.services.push(service),
            Err(error) => {
                return Err(ApplyError::reconciliation(
                    ServiceTarget::from(manifest),
                    report,
                    error,
                ));
            }
        }
    }
    println!("\n{} service(s) applied.", report.services.len());
    Ok(report)
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
            println!(
                "  Fleet '{}': expanding {} agents...",
                fleet.metadata.name,
                fleet.spec.agents.len()
            );
            Ok(fleet.expand())
        }
        other => anyhow::bail!("unsupported kind '{}' in {}", other, path.display()),
    }
}


const INGRESS_TEARDOWN_CHECKPOINT_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct IngressTeardownCheckpoint {
    version: u8,
    cluster: String,
    service_name: String,
    registry_arns: Vec<String>,
}

fn ingress_teardown_checkpoint_key(namespace: &str, name: &str) -> String {
    format!("ingress-teardown-checkpoints/{namespace}/{name}.json")
}

async fn load_ingress_teardown_checkpoint(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    namespace: &str,
    name: &str,
) -> Result<Option<IngressTeardownCheckpoint>> {
    use aws_sdk_s3::error::ProvideErrorMetadata;

    let key = ingress_teardown_checkpoint_key(namespace, name);
    match s3.get_object().bucket(bucket).key(&key).send().await {
        Ok(response) => {
            let bytes = response
                .body
                .collect()
                .await
                .context("failed to read ingress teardown checkpoint")?
                .into_bytes();
            Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("invalid ingress teardown checkpoint s3://{bucket}/{key}")
            })?))
        }
        Err(error) if matches!(error.code(), Some("NoSuchKey" | "NotFound")) => Ok(None),
        Err(error) => Err(error).with_context(|| {
            format!("failed to read ingress teardown checkpoint s3://{bucket}/{key}")
        }),
    }
}

async fn save_ingress_teardown_checkpoint(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    namespace: &str,
    name: &str,
    checkpoint: &IngressTeardownCheckpoint,
) -> Result<()> {
    let key = ingress_teardown_checkpoint_key(namespace, name);
    let body = serde_json::to_vec_pretty(checkpoint)?;
    s3.put_object()
        .bucket(bucket)
        .key(&key)
        .body(ByteStream::from(body))
        .content_type("application/json")
        .send()
        .await
        .with_context(|| format!("failed to persist s3://{bucket}/{key}"))?;
    Ok(())
}

async fn remove_ingress_teardown_checkpoint(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    namespace: &str,
    name: &str,
) -> Result<()> {
    let key = ingress_teardown_checkpoint_key(namespace, name);
    s3.delete_object()
        .bucket(bucket)
        .key(&key)
        .send()
        .await
        .with_context(|| format!("failed to remove s3://{bucket}/{key}"))?;
    Ok(())
}

async fn wait_for_registry_detach(
    ecs: &aws_sdk_ecs::Client,
    cluster: &str,
    service_name: &str,
) -> Result<()> {
    const ATTEMPTS: u32 = 12;
    const INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

    for attempt in 0..ATTEMPTS {
        let response = ecs
            .describe_services()
            .cluster(cluster)
            .services(service_name)
            .send()
            .await
            .context("failed to verify ECS service registry detach")?;
        if !response.failures().is_empty() {
            anyhow::bail!(
                "ECS returned failure(s) while verifying registry detach: {:?}",
                response.failures()
            );
        }
        let service = match response.services() {
            [service] => service,
            [] => anyhow::bail!(
                "ECS service disappeared while verifying registry detach"
            ),
            services => anyhow::bail!(
                "ECS returned {} services for one registry-detach target",
                services.len()
            ),
        };
        if service.service_registries().is_empty() {
            return Ok(());
        }
        if attempt + 1 < ATTEMPTS {
            tokio::time::sleep(INTERVAL).await;
        }
    }

    anyhow::bail!(
        "ECS service {service_name} still reports service registries after waiting; Cloud Map cleanup was not started"
    )
}
async fn apply_ecs(
    ecs: &aws_sdk_ecs::Client,
    s3: &aws_sdk_s3::Client,
    config: &aws_config::SdkConfig,
    m: &OABServiceManifest,
    cluster: &str,
    wait: bool,
    prepared: &PreparedApply,
) -> Result<AppliedService> {
    let bucket = prepared.bucket.as_str();
    let bootstrap = &prepared.bootstrap;
    let ecs_rt = match &m.spec.runtime {
        Runtime::Ecs(rt) => rt,
        _ => unreachable!(),
    };

    let service_name = m.ecs_service_name();
    let mut warnings = bootstrap.warning.iter().cloned().collect::<Vec<_>>();
    if let Some(warning) = &bootstrap.warning {
        eprintln!("  ⚠ {warning}");
    }
    let bootstrap_state = bootstrap.state.as_ref();

    // Read the current generation from the stored desired-state manifest.
    // Ingress teardown retry state is kept separately in an exact-identity
    // checkpoint, so writing a newer manifest cannot lose pending cleanup.
    let manifest_key = format!(
        "manifests/{}/{}.yaml",
        m.metadata.namespace, m.metadata.name
    );
    let current_gen = match s3
        .get_object()
        .bucket(bucket)
        .key(&manifest_key)
        .send()
        .await
    {
        Ok(resp) => {
            let bytes = resp.body.collect().await?.into_bytes();
            let existing: OABServiceManifest = serde_yaml::from_slice(&bytes)?;
            existing.metadata.generation
        }
        Err(_) => 0,
    };
    let generation = current_gen + 1;

    // Resolve the current ECS registry identities without first-match behavior.
    let describe_resp = ecs
        .describe_services()
        .cluster(cluster)
        .services(&service_name)
        .send()
        .await
        .context("failed to describe ECS service")?;
    if !describe_resp.failures().is_empty() {
        anyhow::bail!(
            "ECS returned failure(s) while resolving apply identity: {:?}",
            describe_resp.failures()
        );
    }
    let existing_service = match describe_resp.services() {
        [] => None,
        [service] => Some(service),
        services => anyhow::bail!(
            "ECS returned {} services for one apply target",
            services.len()
        ),
    };
    let mut existing_registry_arns = Vec::new();
    if let Some(service) = existing_service {
        for registry in service.service_registries() {
            existing_registry_arns.push(
                registry
                    .registry_arn()
                    .filter(|value| !value.trim().is_empty())
                    .context("ECS returned a service registry without an ARN")?
                    .to_string(),
            );
        }
    }
    existing_registry_arns.sort();
    existing_registry_arns.dedup();
    let has_registries = !existing_registry_arns.is_empty();

    let stored_teardown = load_ingress_teardown_checkpoint(
        s3,
        bucket,
        &m.metadata.namespace,
        &m.metadata.name,
    )
    .await?;
    if m.spec.ingress.is_some() && stored_teardown.is_some() {
        anyhow::bail!(
            "a previous ingress teardown is still pending; apply the ingress-free manifest again before re-enabling ingress"
        );
    }
    let teardown_checkpoint = if m.spec.ingress.is_none() {
        match stored_teardown {
            Some(checkpoint) => {
                if checkpoint.version != INGRESS_TEARDOWN_CHECKPOINT_VERSION
                    || checkpoint.cluster != cluster
                    || checkpoint.service_name != service_name
                    || checkpoint.registry_arns.is_empty()
                {
                    anyhow::bail!("ingress teardown checkpoint does not match this apply target");
                }
                Some(checkpoint)
            }
            None if has_registries => {
                let checkpoint = IngressTeardownCheckpoint {
                    version: INGRESS_TEARDOWN_CHECKPOINT_VERSION,
                    cluster: cluster.to_string(),
                    service_name: service_name.clone(),
                    registry_arns: existing_registry_arns.clone(),
                };
                save_ingress_teardown_checkpoint(
                    s3,
                    bucket,
                    &m.metadata.namespace,
                    &m.metadata.name,
                    &checkpoint,
                )
                .await?;
                Some(checkpoint)
            }
            None => None,
        }
    } else {
        None
    };

    if let Some(checkpoint) = &teardown_checkpoint {
        eprintln!("  🌐 ingress removed from manifest — tearing down exact API wiring...");
        for registry_arn in &checkpoint.registry_arns {
            warnings.extend(
                crate::ingress::teardown(
                    config,
                    &m.metadata.namespace,
                    &m.metadata.name,
                    Some(registry_arn),
                )
                .await
                .context("failed to tear down exact ingress API wiring")?,
            );
        }
    }

    // 1. Upload manifest to S3 (record of desired state)
    let mut manifest_to_store = serde_yaml::to_value(m)?;
    manifest_to_store["metadata"]["generation"] = serde_yaml::Value::Number(generation.into());
    let manifest_yaml = serde_yaml::to_string(&manifest_to_store)?;
    s3.put_object()
        .bucket(bucket)
        .key(&manifest_key)
        .body(ByteStream::from(manifest_yaml.into_bytes()))
        .send()
        .await
        .context("failed to upload manifest to S3")?;

    // 2. Build environment variables
    let mut env_vars = vec![
        KeyValuePair::builder()
            .name("NAMESPACE")
            .value(&m.metadata.namespace)
            .build(),
        KeyValuePair::builder()
            .name("NAME")
            .value(&m.metadata.name)
            .build(),
    ];
    // openab's own AWS SDK calls (config-s3 loading, secrets resolution, etc.)
    // resolve region via the standard chain: AWS_REGION env var → profile →
    // IMDS. Fargate tasks have no EC2 instance metadata to fall back to, so
    // without this the SDK can fail to resolve an endpoint at all.
    // Region is injected below after bootstrap_state is loaded (to allow
    // fallback to bootstrap_state.region when config.region() is None).
    if let Some(ref bootstrap) = m.spec.bootstrap_from {
        env_vars.push(
            KeyValuePair::builder()
                .name("BOOTSTRAP_FROM")
                .value(bootstrap)
                .build(),
        );
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
        secrets.push(
            Secret::builder()
                .name(name)
                .value_from(value_from)
                .build()
                .unwrap(),
        );
    }

    // Resolve effective region: prefer SDK config, fall back to bootstrap
    // state's recorded region. Fargate has no IMDS, so without AWS_REGION the
    // container's SDK calls will fail to resolve endpoints entirely.
    let effective_region: Option<String> = config
        .region()
        .map(|r| r.as_ref().to_string())
        .or_else(|| bootstrap_state.as_ref().map(|s| s.region.clone()));
    if let Some(ref region) = effective_region {
        env_vars.push(
            KeyValuePair::builder()
                .name("AWS_REGION")
                .value(region)
                .build(),
        );
    }

    let mut container = ContainerDefinition::builder()
        .name("openab")
        .image(&m.spec.image)
        .essential(true)
        .set_environment(Some(env_vars))
        .set_secrets(if secrets.is_empty() {
            None
        } else {
            Some(secrets)
        });

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
    let execution_role_arn = bootstrap_state
        .as_ref()
        .map(|s| s.resources.execution_role_arn.clone());
    // Manifest task_role_arn takes precedence over bootstrap shared role.
    // Filter empty strings so a blank value falls through to bootstrap.
    let task_role_arn = resolve_task_role_arn(
        &ecs_rt.task_role_arn,
        bootstrap_state
            .as_ref()
            .map(|s| s.resources.task_role_arn.as_str()),
    );
    match (
        &task_role_arn,
        ecs_rt.task_role_arn.as_deref().filter(|s| !s.is_empty()),
    ) {
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
        other => anyhow::bail!(
            "unsupported architecture '{other}' — should be caught by manifest validation"
        ),
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
        .services()
        .first()
        .is_some_and(|service| service.status() == Some("ACTIVE"));
    let action;

    if service_active {
        action = ApplyAction::Updated;
        // Recreate is NOT required to attach/fix service discovery: ECS's
        // UpdateService API has supported adding/updating/removing
        // serviceRegistries since March 2022 (rolling replacement — new tasks
        // start with the updated registry, old tasks stop once they're
        // healthy, no downtime gap). It does require the AWSServiceRoleForECS
        // service-linked role, which ECS creates automatically the first time
        // any account uses ECS service discovery — no action needed here.
        let registry_mismatch = cloud_map
            .as_ref()
            .is_some_and(|cm| has_registries && !existing_registry_arns.contains(&cm.registry_arn));
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
            .cluster(cluster)
            .service(&service_name)
            .task_definition(&task_def_arn)
            .enable_execute_command(true)
            .network_configuration(network_config);

        if let Some(cm) = &cloud_map {
            if !has_registries || registry_mismatch {
                let mut registry =
                    aws_sdk_ecs::types::ServiceRegistry::builder().registry_arn(&cm.registry_arn);
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
        action = ApplyAction::Created;
        let cap_strategy = CapacityProviderStrategyItem::builder()
            .capacity_provider(&ecs_rt.capacity_provider)
            .weight(1)
            .build()?;

        let mut create_req = ecs
            .create_service()
            .cluster(cluster)
            .service_name(&service_name)
            .task_definition(&task_def_arn)
            .desired_count(1)
            .enable_execute_command(true)
            .capacity_provider_strategy(cap_strategy)
            .network_configuration(network_config);

        if let Some(cm) = &cloud_map {
            let mut registry =
                aws_sdk_ecs::types::ServiceRegistry::builder().registry_arn(&cm.registry_arn);
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

    if let Some(checkpoint) = &teardown_checkpoint {
        wait_for_registry_detach(&ecs, cluster, &service_name).await?;
        for registry_arn in &checkpoint.registry_arns {
            crate::ingress::delete_cloud_map_exact(config, registry_arn, &service_name)
                .await
                .with_context(|| {
                    format!("failed exact Cloud Map cleanup for {registry_arn}")
                })?;
        }
    }

    // Ingress step 2: VPC Link + API Gateway + routes + SG rule.
    let mut webhook_urls = Vec::new();
    if let (Some(ingress), Some(cm)) = (&m.spec.ingress, &cloud_map) {
        eprintln!("  🌐 Reconciling ingress (VPC Link + API Gateway)...");
        let gateway = crate::ingress::ensure_gateway(
            config,
            &m.metadata.namespace,
            &m.metadata.name,
            ingress,
            &ecs_rt.networking.subnets,
            &ecs_rt.networking.security_groups,
            &cm.registry_arn,
        )
        .await?;
        webhook_urls = gateway.webhook_urls;
        warnings.extend(gateway.warnings);
        println!("  🔗 Webhook URL(s) for {}:", m.metadata.name);
        for url in &webhook_urls {
            println!("     {url}");
        }

        let path_urls: Vec<(String, String)> = ingress
            .paths
            .iter()
            .cloned()
            .zip(webhook_urls.iter().cloned())
            .collect();
        match crate::ingress::register_telegram_webhook(config, &m.spec.secrets, &path_urls).await {
            Ok(Some(description)) => {
                eprintln!("  ✓ Telegram webhook registered: {description}")
            }
            Ok(None) => {}
            Err(error) => {
                let warning = format!(
                    "Telegram webhook registration failed (apply still succeeded): {error}"
                );
                eprintln!("  ⚠ {warning}");
                warnings.push(warning);
            }
        }
    }

    if wait {
        eprintln!("  ⏳ Waiting for {} to stabilize...", m.metadata.name);
        wait_for_stable(ecs, cluster, &service_name).await?;
        eprintln!("  ✓ {} is stable", m.metadata.name);
    }

    if teardown_checkpoint.is_some() {
        remove_ingress_teardown_checkpoint(
            s3,
            bucket,
            &m.metadata.namespace,
            &m.metadata.name,
        )
        .await?;
    }

    Ok(AppliedService {
        namespace: m.metadata.namespace.clone(),
        name: m.metadata.name.clone(),
        ecs_service_name: service_name,
        action,
        webhook_urls,
        warnings,
    })
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
        let resp = ecs
            .describe_services()
            .cluster(cluster)
            .services(service)
            .send()
            .await?;
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
    fn apply_options_accepts_explicit_control_plane_bucket() {
        let options = ApplyOptions::new("prod-cluster")
            .with_control_plane_bucket("prod-control-plane")
            .with_wait(true);
        assert_eq!(options.cluster, "prod-cluster");
        assert_eq!(
            options.control_plane_bucket.as_deref(),
            Some("prod-control-plane")
        );
        assert!(options.wait);
    }

    fn test_sdk_config() -> aws_config::SdkConfig {
        aws_config::SdkConfig::builder()
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build()
    }

    fn minimal_manifest() -> OABServiceManifest {
        serde_yaml::from_str(
            r#"
apiVersion: oab.dev/v2
kind: OABService
metadata:
  name: test-svc
  namespace: test
spec:
  image: example.com/openab:latest
  resources:
    cpu: "256"
    memory: "512"
  configFrom: s3://bucket/config.toml
  runtime:
    type: ecs
    networking:
      subnets: [subnet-aaa]
      securityGroups: [sg-aaa]
"#,
        )
        .expect("valid manifest")
    }

    #[tokio::test]
    async fn programmatic_apply_progress_scope_is_disabled() {
        PROGRESS_ENABLED
            .scope(false, async { assert!(!progress_enabled()) })
            .await;
        assert!(progress_enabled());
    }

    #[tokio::test]
    async fn apply_manifests_rejects_empty_set() {
        let cfg = test_sdk_config();
        let err = apply_manifests(&cfg, &[], &ApplyOptions::new("test-cluster"))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ApplyErrorKind::Validation);
        assert!(err.to_string().contains("empty manifest set"));
    }

    #[tokio::test]
    async fn apply_manifests_rejects_empty_cluster_name_locally() {
        let cfg = test_sdk_config();
        let manifest = minimal_manifest();
        let err = apply_manifests(&cfg, &[manifest], &ApplyOptions::new(""))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ApplyErrorKind::Validation);
        assert!(err.to_string().contains("empty or whitespace"));
    }

    #[tokio::test]
    async fn apply_manifests_rejects_whitespace_cluster_name_locally() {
        let cfg = test_sdk_config();
        let manifest = minimal_manifest();
        let err = apply_manifests(&cfg, &[manifest], &ApplyOptions::new("  \t\n"))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ApplyErrorKind::Validation);
        assert!(err.to_string().contains("empty or whitespace"));
    }

    #[test]
    fn cluster_response_accepts_requested_active_cluster() {
        assert!(classify_cluster_response(
            "prod",
            &[("prod", "arn:aws:ecs:us-east-1:123:cluster/prod", "ACTIVE")],
            &[],
        )
        .is_ok());
    }

    #[test]
    fn cluster_response_accepts_requested_cluster_arn() {
        let arn = "arn:aws:ecs:us-east-1:123:cluster/prod";
        assert!(classify_cluster_response(arn, &[("prod", arn, "ACTIVE")], &[],).is_ok());
    }

    #[test]
    fn cluster_response_rejects_empty_cluster_list() {
        let error = classify_cluster_response("prod", &[], &[]).unwrap_err();
        assert!(error.contains("not returned"));
    }

    #[test]
    fn cluster_response_rejects_service_failures() {
        let error =
            classify_cluster_response("prod", &[], &["prod: MISSING".to_string()]).unwrap_err();
        assert!(error.contains("MISSING"));
    }

    #[test]
    fn cluster_response_rejects_inactive_cluster() {
        let error = classify_cluster_response(
            "prod",
            &[("prod", "arn:aws:ecs:us-east-1:123:cluster/prod", "INACTIVE")],
            &[],
        )
        .unwrap_err();
        assert!(error.contains("INACTIVE"));
    }

    #[test]
    fn reconciliation_error_exposes_failed_service_and_completed_report() {
        let completed_service = AppliedService {
            namespace: "prod".to_string(),
            name: "done".to_string(),
            ecs_service_name: "oab-prod-done".to_string(),
            action: ApplyAction::Updated,
            webhook_urls: vec!["https://example.test/webhook".to_string()],
            warnings: vec!["degraded".to_string()],
        };
        let completed = ApplyReport {
            services: vec![completed_service.clone()],
        };
        let failed = ServiceTarget {
            namespace: "prod".to_string(),
            name: "failed".to_string(),
            ecs_service_name: "oab-prod-failed".to_string(),
        };
        let error =
            ApplyError::reconciliation(failed.clone(), completed.clone(), anyhow::anyhow!("boom"));
        assert_eq!(error.kind, ApplyErrorKind::Reconciliation);
        assert_eq!(error.failed_service, Some(failed));
        assert_eq!(error.completed, completed);
        assert_eq!(error.completed.services[0], completed_service);
    }

    #[test]
    fn apply_error_source_preserves_immediate_anyhow_context() {
        let error = ApplyError::target(anyhow::anyhow!("root cause").context("target lookup"));
        let source = std::error::Error::source(&error).expect("apply error source");
        assert_eq!(source.to_string(), "target lookup");
        assert_eq!(
            source.source().expect("root source").to_string(),
            "root cause"
        );
    }

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
