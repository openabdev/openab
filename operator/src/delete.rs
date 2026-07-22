use anyhow::{Context, Result};
use aws_sdk_ecs::error::ProvideErrorMetadata;
use aws_sdk_s3::primitives::ByteStream;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

// Route all human-readable progress through the same task-local gate as
// apply: CLI callers keep today's output, programmatic callers are silent.
macro_rules! println {
    ($($arg:tt)*) => {{ if crate::apply::progress_enabled() { std::println!($($arg)*); } }};
}
macro_rules! eprintln {
    ($($arg:tt)*) => {{ if crate::apply::progress_enabled() { std::eprintln!($($arg)*); } }};
}
macro_rules! eprint {
    ($($arg:tt)*) => {{ if crate::apply::progress_enabled() { std::eprint!($($arg)*); } }};
}

/// Identity of a service targeted for deletion.
///
/// Deletion does not require the original manifest: the control-plane copy
/// under `manifests/{namespace}/{name}.yaml` (and every other resource) is
/// addressed by `namespace` + `name` alone. Targets are subject to the shared
/// injective identity rule (see [`crate::identity`]): namespaces must not
/// contain `-`, names may. Before any destructive mutation, delete also
/// verifies in the control plane that no other recorded logical pair claims
/// the same physical `oab-{namespace}-{name}` identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteTarget {
    pub namespace: String,
    pub name: String,
}

impl DeleteTarget {
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
        }
    }

    /// ECS service name derived from the target (`oab-{namespace}-{name}`).
    pub fn ecs_service_name(&self) -> String {
        crate::identity::physical_service_name(&self.namespace, &self.name)
    }
}

/// Target options for [`delete_services`]. Mirrors
/// [`ApplyOptions`](crate::apply::ApplyOptions): the cluster is required and
/// the library never reads `~/.oabctl/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteOptions {
    /// ECS cluster name or ARN the services were deployed into.
    pub cluster: String,
    /// Optional control-plane bucket override. When absent, resolution uses
    /// `OAB_CONTROL_PLANE_BUCKET`, then `oab-control-plane-{account}` from
    /// the caller's AWS identity, matching apply's shared resolver.
    pub control_plane_bucket: Option<String>,
}

impl DeleteOptions {
    pub fn new(cluster: impl Into<String>) -> Self {
        Self {
            cluster: cluster.into(),
            control_plane_bucket: None,
        }
    }

    pub fn with_control_plane_bucket(mut self, bucket: impl Into<String>) -> Self {
        self.control_plane_bucket = Some(bucket.into());
        self
    }
}

/// Teardown outcome for one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedService {
    pub namespace: String,
    pub name: String,
    pub ecs_service_name: String,
    /// Non-fatal diagnostics retained for API compatibility. Examples include
    /// resuming from an already-absent ECS service or skipping dependent ingress
    /// cleanup when no exact ingress identity was recorded. Exact-identity
    /// dependent and S3 cleanup failures remain fatal for programmatic calls so
    /// the durable checkpoint remains available for retry.
    pub warnings: Vec<String>,
}

/// Structured result of a successful (or partially completed) delete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteReport {
    pub services: Vec<DeletedService>,
}

/// High-level phase in which delete failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteErrorKind {
    /// Contract violation caught before any AWS call.
    Validation,
    /// The deployment target (bucket resolution) could not be established.
    Target,
    /// Teardown of a specific service failed. Cleanup is resumable: calling
    /// [`delete_services`] again with the same target continues from the
    /// remaining resources.
    Teardown,
}

/// Structured delete failure. Teardown failures identify the failed service
/// and retain the report for all services completed before it.
#[derive(Debug)]
pub struct DeleteError {
    pub kind: DeleteErrorKind,
    pub failed_service: Option<DeleteTarget>,
    pub completed: DeleteReport,
    source: anyhow::Error,
}

impl DeleteError {
    fn validation(source: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: DeleteErrorKind::Validation,
            failed_service: None,
            completed: DeleteReport::default(),
            source: source.into(),
        }
    }

    fn target(source: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: DeleteErrorKind::Target,
            failed_service: None,
            completed: DeleteReport::default(),
            source: source.into(),
        }
    }

    fn teardown(
        failed_service: DeleteTarget,
        completed: DeleteReport,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            kind: DeleteErrorKind::Teardown,
            failed_service: Some(failed_service),
            completed,
            source: source.into(),
        }
    }

    pub fn source_error(&self) -> &anyhow::Error {
        &self.source
    }
}

impl fmt::Display for DeleteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.failed_service {
            Some(service) => write!(
                f,
                "delete {:?} error for {}/{}: {}",
                self.kind, service.namespace, service.name, self.source
            ),
            None => write!(f, "delete {:?} error: {}", self.kind, self.source),
        }
    }
}

impl std::error::Error for DeleteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

const DELETE_CHECKPOINT_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeleteCheckpoint {
    version: u8,
    namespace: String,
    name: String,
    bucket: String,
    partition: String,
    account: String,
    region: String,
    requested_cluster: String,
    cluster_arn: String,
    service_arn: String,
    /// ECS `createdAt` as epoch nanoseconds; distinguishes a recreated same-name service.
    service_created_at: i128,
    registry_arn: Option<String>,
    api_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceIdentityState {
    Live,
    RetryGone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArnParts<'a> {
    partition: &'a str,
    service: &'a str,
    region: &'a str,
    account: &'a str,
    resource: &'a str,
}

fn parse_arn(value: &str) -> Option<ArnParts<'_>> {
    let mut parts = value.splitn(6, ':');
    if parts.next()? != "arn" {
        return None;
    }
    let arn = ArnParts {
        partition: parts.next()?,
        service: parts.next()?,
        region: parts.next()?,
        account: parts.next()?,
        resource: parts.next()?,
    };
    if arn.partition.is_empty()
        || arn.service.is_empty()
        || arn.region.is_empty()
        || arn.account.is_empty()
        || arn.resource.is_empty()
    {
        return None;
    }
    Some(arn)
}

fn cluster_reference_matches(cluster_arn: &str, reference: &str) -> bool {
    let Some(cluster) = parse_arn(cluster_arn) else {
        return false;
    };
    if reference.starts_with("arn:") {
        return reference == cluster_arn;
    }
    cluster.resource.strip_prefix("cluster/") == Some(reference)
}

fn validate_checkpoint_arns(
    checkpoint: &DeleteCheckpoint,
    expected_service_name: &str,
    partition: &str,
    account: &str,
    region: &str,
) -> Result<()> {
    let cluster = parse_arn(&checkpoint.cluster_arn)
        .context("delete checkpoint contains an invalid ECS cluster ARN")?;
    if cluster.partition != partition
        || cluster.service != "ecs"
        || cluster.account != account
        || cluster.region != region
    {
        anyhow::bail!("delete checkpoint ECS cluster ARN is outside the caller boundary");
    }
    let cluster_name = cluster
        .resource
        .strip_prefix("cluster/")
        .filter(|value| !value.is_empty() && !value.contains('/'))
        .context("delete checkpoint contains an invalid ECS cluster resource")?;

    let service = parse_arn(&checkpoint.service_arn)
        .context("delete checkpoint contains an invalid ECS service ARN")?;
    if service.partition != cluster.partition
        || service.service != "ecs"
        || service.account != account
        || service.region != region
    {
        anyhow::bail!("delete checkpoint ECS service ARN is outside the cluster boundary");
    }
    let (service_cluster, service_name) = service
        .resource
        .strip_prefix("service/")
        .and_then(|resource| resource.split_once('/'))
        .context("delete checkpoint ECS service ARN lacks its cluster identity")?;
    if service_cluster != cluster_name || service_name != expected_service_name {
        anyhow::bail!("delete checkpoint ECS service ARN does not match the target cluster/service");
    }

    if let Some(registry_arn) = checkpoint.registry_arn.as_deref() {
        let registry = parse_arn(registry_arn)
            .context("delete checkpoint contains an invalid Cloud Map service ARN")?;
        if registry.partition != cluster.partition
            || registry.service != "servicediscovery"
            || registry.account != account
            || registry.region != region
            || registry
                .resource
                .strip_prefix("service/")
                .filter(|value| !value.is_empty() && !value.contains('/'))
                .is_none()
        {
            anyhow::bail!(
                "delete checkpoint Cloud Map ARN is outside the ECS account/region boundary"
            );
        }
    }
    Ok(())
}

fn classify_service_identity(
    has_checkpoint: bool,
    service_present: bool,
    status: Option<&str>,
    failure_reasons: &[&str],
) -> std::result::Result<ServiceIdentityState, String> {
    if !failure_reasons.is_empty() {
        if has_checkpoint
            && !service_present
            && failure_reasons.len() == 1
            && failure_reasons[0].eq_ignore_ascii_case("MISSING")
        {
            return Ok(ServiceIdentityState::RetryGone);
        }
        return Err(format!(
            "DescribeServices returned failure(s): {}",
            failure_reasons.join(", ")
        ));
    }

    match (service_present, status) {
        (true, Some("ACTIVE")) | (true, Some("DRAINING")) => Ok(ServiceIdentityState::Live),
        // INACTIVE is retry-gone only after the caller has validated the exact
        // service ARN, cluster ARN, and incarnation discriminator.
        (true, Some("INACTIVE")) if has_checkpoint => Ok(ServiceIdentityState::RetryGone),
        // An empty successful response is ambiguous, and a checkpoint only
        // authorizes exactly zero services plus one MISSING failure.
        (false, None) => Err(
            "ECS returned no service without an explicit MISSING failure; refusing cleanup"
                .to_string(),
        ),
        (true, Some("INACTIVE")) => Err(
            "ECS returned INACTIVE without a matching delete checkpoint".to_string(),
        ),
        (true, None) => Err("ECS returned a service without status".to_string()),
        (true, Some(other)) => Err(format!(
            "unexpected ECS service status during delete: {other}"
        )),
        (false, Some(_)) => Err("ECS returned status without a service identity".to_string()),
    }
}

fn checkpoint_key(namespace: &str, name: &str) -> String {
    crate::identity::delete_checkpoint_key(namespace, name)
}

#[allow(clippy::too_many_arguments)]
fn validate_checkpoint(
    checkpoint: &DeleteCheckpoint,
    namespace: &str,
    name: &str,
    cluster: &str,
    bucket: &str,
    partition: &str,
    account: &str,
    region: &str,
) -> Result<()> {
    if checkpoint.version != DELETE_CHECKPOINT_VERSION {
        anyhow::bail!(
            "unsupported delete checkpoint version {}",
            checkpoint.version
        );
    }
    if checkpoint.namespace != namespace
        || checkpoint.name != name
        || checkpoint.bucket != bucket
        || checkpoint.partition != partition
        || checkpoint.account != account
        || checkpoint.region != region
    {
        anyhow::bail!(
            "delete checkpoint identity does not match namespace/name, bucket, or caller boundary"
        );
    }
    if !cluster_reference_matches(&checkpoint.cluster_arn, &checkpoint.requested_cluster)
        || !cluster_reference_matches(&checkpoint.cluster_arn, cluster)
    {
        anyhow::bail!("delete checkpoint canonical cluster does not match the requested cluster");
    }
    if checkpoint.cluster_arn.trim().is_empty()
        || checkpoint.service_arn.trim().is_empty()
        || checkpoint.service_created_at <= 0
    {
        anyhow::bail!("delete checkpoint is missing exact ECS identity or service incarnation");
    }
    validate_checkpoint_arns(
        checkpoint,
        &crate::identity::physical_service_name(namespace, name),
        partition,
        account,
        region,
    )?;
    if let Some(api_id) = checkpoint.api_id.as_deref() {
        if api_id.trim().is_empty() || checkpoint.registry_arn.is_none() {
            anyhow::bail!(
                "delete checkpoint API identity requires a non-empty API ID and registry ARN"
            );
        }
    }
    Ok(())
}

fn validate_delete_request(
    targets: &[DeleteTarget],
    opts: &DeleteOptions,
) -> std::result::Result<(), DeleteError> {
    if targets.is_empty() {
        return Err(DeleteError::validation(anyhow::anyhow!(
            "no targets to delete (empty target set)"
        )));
    }
    if opts.cluster.trim().is_empty() {
        return Err(DeleteError::validation(anyhow::anyhow!(
            "DeleteOptions.cluster must not be empty"
        )));
    }

    let mut seen = std::collections::HashSet::with_capacity(targets.len());
    for target in targets {
        // Shared injective identity rule (see `crate::identity`): non-empty
        // components and a hyphen-free namespace, matching what apply-side
        // manifest validation accepts.
        crate::identity::validate_injective_identity(&target.namespace, &target.name)
            .map_err(DeleteError::validation)?;
        if !seen.insert((&target.namespace, &target.name)) {
            return Err(DeleteError::validation(anyhow::anyhow!(
                "duplicate delete target '{}/{}'",
                target.namespace,
                target.name
            )));
        }
    }
    Ok(())
}

fn record_delete_result(
    mut report: DeleteReport,
    target: &DeleteTarget,
    outcome: Result<Vec<String>>,
) -> std::result::Result<DeleteReport, DeleteError> {
    match outcome {
        Ok(warnings) => {
            report.services.push(DeletedService {
                namespace: target.namespace.clone(),
                name: target.name.clone(),
                ecs_service_name: target.ecs_service_name(),
                warnings,
            });
            Ok(report)
        }
        Err(error) => Err(DeleteError::teardown(target.clone(), report, error)),
    }
}

/// Tear down OAB services programmatically, without reading CLI home
/// configuration or writing progress to process-global stdout/stderr.
///
/// Contract (enforced before any AWS call):
/// - the target set must be non-empty
/// - [`DeleteOptions::cluster`] must be non-empty; its optional bucket override
///   follows the shared control-plane resolver
/// - every target must satisfy the shared injective identity rule (see
///   [`crate::identity`]): non-empty `namespace`/`name` and a hyphen-free
///   namespace, the same rule apply-side manifest validation enforces. Legacy
///   deployments created under a hyphenated namespace are not accepted here;
///   remove them with the `oabctl delete` CLI, which performs the
///   control-plane ownership check for such targets.
///
/// Additionally, before any destructive mutation, delete verifies in the
/// control plane that no other recorded logical pair claims the target's
/// physical `oab-{namespace}-{name}` identity, and fails closed if one does.
///
/// # Concurrency
///
/// This function is not serialized with [`crate::apply::apply_manifests`].
/// Callers must serialize mutations for the same AWS account, Region,
/// control-plane bucket, ECS cluster, and physical service identity. This
/// includes every alias-equivalent logical pair that could map to the same
/// `oab-{namespace}-{name}` value (for example `prod/team-bot` and
/// `prod-team/bot`). If that precondition is violated, stop concurrent
/// writers, inspect the retained checkpoint, then either re-apply the intended
/// desired state or retry delete.
///
/// Before ECS mutation, delete persists an exact-identity checkpoint containing
/// the caller account/region, bucket, canonical cluster and service ARNs, and
/// any ECS registry/API IDs. The control-plane bucket must provide default
/// server-side encryption and S3 versioning; this library writes application
/// JSON but does not weaken or validate those bucket-level policies per request.
/// An absent ECS service or ambiguous `DescribeServices` response is rejected
/// unless that matching durable checkpoint already exists. Cleanup uses only IDs
/// from the checkpoint and removes the checkpoint last, making partial failures
/// safe to retry. The legacy CLI uses best-effort warnings for dependent/S3
/// cleanup, while this programmatic API keeps those failures fatal.
pub async fn delete_services(
    aws_config: &aws_config::SdkConfig,
    targets: &[DeleteTarget],
    opts: &DeleteOptions,
) -> std::result::Result<DeleteReport, DeleteError> {
    crate::apply::with_progress_suppressed(async {
        validate_delete_request(targets, opts)?;
        let bucket = crate::control_plane::resolve_bucket(
            aws_config,
            opts.control_plane_bucket.as_deref(),
        )
        .await
        .map_err(DeleteError::target)?;

        let mut report = DeleteReport::default();
        for target in targets {
            let outcome = run_with_bucket(
                aws_config,
                "oabservice",
                &target.name,
                &opts.cluster,
                &target.namespace,
                &bucket,
                CleanupMode::Strict,
            )
            .await;
            report = record_delete_result(report, target, outcome)?;
        }
        Ok(report)
    })
    .await
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainPollAction {
    Complete,
    Retry,
    TimedOut,
}

fn drain_poll_action(is_gone: bool, attempt: u32, max_attempts: u32) -> DrainPollAction {
    debug_assert!(max_attempts > 0);
    debug_assert!(attempt < max_attempts);
    if is_gone {
        DrainPollAction::Complete
    } else if attempt + 1 == max_attempts {
        DrainPollAction::TimedOut
    } else {
        DrainPollAction::Retry
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
            CleanupMode::BestEffort,
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
    run_with_bucket(
        aws_config,
        resource,
        name,
        cluster,
        namespace,
        &bucket,
        CleanupMode::BestEffort,
    )
    .await
    .map(|_warnings| ())
}

async fn caller_context(aws_config: &aws_config::SdkConfig) -> Result<(String, String, String)> {
    let identity = aws_sdk_sts::Client::new(aws_config)
        .get_caller_identity()
        .send()
        .await
        .context("failed to identify delete caller")?;
    let account = identity
        .account()
        .context("STS response missing caller account")?
        .to_string();
    let caller_arn = identity.arn().context("STS response missing caller ARN")?;
    let mut caller_arn_parts = caller_arn.splitn(3, ':');
    if caller_arn_parts.next() != Some("arn") {
        anyhow::bail!("STS returned an invalid caller ARN");
    }
    let partition = caller_arn_parts
        .next()
        .filter(|value| !value.is_empty())
        .context("STS caller ARN is missing its partition")?
        .to_string();
    let region = aws_config
        .region()
        .context("AWS region must be resolved before delete")?
        .to_string();
    Ok((partition, account, region))
}

async fn load_checkpoint(
    s3: &aws_sdk_s3::Client, bucket: &str, namespace: &str, name: &str,
) -> Result<Option<DeleteCheckpoint>> {
    let key = checkpoint_key(namespace, name);
    match s3.get_object().bucket(bucket).key(&key).send().await {
        Ok(response) => {
            let bytes = response.body.collect().await
                .context("failed to read delete checkpoint body")?.into_bytes();
            Ok(Some(serde_json::from_slice(&bytes)
                .context("invalid delete checkpoint payload")?))
        }
        Err(error) if matches!(error.code(), Some("NoSuchKey" | "NotFound")) => Ok(None),
        Err(error) => Err(error).context("failed to read delete checkpoint"),
    }
}

async fn save_checkpoint(s3: &aws_sdk_s3::Client, checkpoint: &DeleteCheckpoint) -> Result<()> {
    let key = checkpoint_key(&checkpoint.namespace, &checkpoint.name);
    let body = serde_json::to_vec_pretty(checkpoint)?;
    s3.put_object().bucket(&checkpoint.bucket).key(&key)
        .body(ByteStream::from(body)).content_type("application/json").send().await
        .context("failed to persist exact delete identity checkpoint")?;
    Ok(())
}

fn failure_reasons(
    response: &aws_sdk_ecs::operation::describe_services::DescribeServicesOutput,
) -> Vec<&str> {
    response.failures().iter()
        .map(|failure| failure.reason().unwrap_or("UNKNOWN"))
        .collect()
}


fn single_described_service(
    response: &aws_sdk_ecs::operation::describe_services::DescribeServicesOutput,
) -> Result<Option<&aws_sdk_ecs::types::Service>> {
    match response.services() {
        [] => Ok(None),
        [service] => Ok(Some(service)),
        services => anyhow::bail!(
            "ECS returned {} services for one delete target",
            services.len()
        ),
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupMode {
    Strict,
    BestEffort,
}

enum PreparedIdentity {
    Exact(Box<(DeleteCheckpoint, ServiceIdentityState)>),
}

#[allow(clippy::too_many_arguments)]
async fn prepare_identity(
    aws_config: &aws_config::SdkConfig,
    ecs: &aws_sdk_ecs::Client,
    s3: &aws_sdk_s3::Client,
    namespace: &str,
    name: &str,
    cluster: &str,
    bucket: &str,
) -> Result<PreparedIdentity> {
    let (partition, account, region) = caller_context(aws_config).await?;
    if let Some(checkpoint) = load_checkpoint(s3, bucket, namespace, name).await? {
        validate_checkpoint(
            &checkpoint,
            namespace,
            name,
            cluster,
            bucket,
            &partition,
            &account,
            &region,
        )?;
        let response = ecs.describe_services()
            .cluster(&checkpoint.cluster_arn).services(&checkpoint.service_arn)
            .send().await.context("failed to describe checkpointed ECS service")?;
        let service = single_described_service(&response)?;
        let reasons = failure_reasons(&response);
        if let Some(service) = service {
            validate_returned_service(&checkpoint, service)?;
        }
        let state = classify_service_identity(
            true,
            service.is_some(),
            service.and_then(|service| service.status()),
            &reasons,
        )
        .map_err(anyhow::Error::msg)?;
        return Ok(PreparedIdentity::Exact(Box::new((checkpoint, state))));
    }

    let service_name = crate::identity::physical_service_name(namespace, name);
    let response = ecs.describe_services().cluster(cluster).services(&service_name)
        .send().await.context("failed to describe ECS service before delete")?;
    let service = single_described_service(&response)?;
    let reasons = failure_reasons(&response);
    let state = classify_service_identity(
        false,
        service.is_some(),
        service.and_then(|service| service.status()),
        &reasons,
    )
    .map_err(anyhow::Error::msg)?;
    let service = service.context("ECS service identity unexpectedly absent")?;
    let service_arn = service.service_arn().filter(|value| !value.trim().is_empty())
        .context("ECS service response missing service ARN")?.to_string();
    let cluster_arn = service.cluster_arn().filter(|value| !value.trim().is_empty())
        .context("ECS service response missing canonical cluster ARN")?.to_string();
    let service_created_at = service
        .created_at()
        .map(|created_at| created_at.as_nanos())
        .context("ECS service response missing createdAt incarnation")?;
    let registry_arn = match service.service_registries() {
        [] => None,
        [registry] => Some(
            registry
                .registry_arn()
                .filter(|value| !value.trim().is_empty())
                .context("ECS service registry is missing its exact ARN")?
                .to_string(),
        ),
        _ => anyhow::bail!(
            "ECS service has multiple registry identities; refusing ambiguous dependent cleanup"
        ),
    };
    let api_id = match registry_arn.as_deref() {
        Some(registry_arn) => crate::ingress::resolve_api_id_for_registry(
            aws_config, namespace, name, registry_arn,
        ).await?,
        None => None,
    };
    let checkpoint = DeleteCheckpoint {
        version: DELETE_CHECKPOINT_VERSION,
        namespace: namespace.to_string(),
        name: name.to_string(),
        bucket: bucket.to_string(),
        partition,
        account,
        region,
        requested_cluster: cluster.to_string(),
        cluster_arn,
        service_arn,
        service_created_at,
        registry_arn,
        api_id,
    };
    validate_checkpoint(
        &checkpoint,
        namespace,
        name,
        cluster,
        bucket,
        &checkpoint.partition,
        &checkpoint.account,
        &checkpoint.region,
    )?;
    save_checkpoint(s3, &checkpoint).await?;
    Ok(PreparedIdentity::Exact(Box::new((checkpoint, state))))
}

fn validate_service_incarnation(expected: i128, actual: Option<i128>) -> Result<()> {
    if actual != Some(expected) {
        anyhow::bail!(
            "ECS response belongs to a recreated same-name service (createdAt mismatch)"
        );
    }
    Ok(())
}

fn validate_returned_service(
    checkpoint: &DeleteCheckpoint,
    service: &aws_sdk_ecs::types::Service,
) -> Result<()> {
    if service.service_arn() != Some(checkpoint.service_arn.as_str())
        || service.cluster_arn() != Some(checkpoint.cluster_arn.as_str())
    {
        anyhow::bail!("ECS response returned a conflicting service identity");
    }
    let created_at = service
        .created_at()
        .map(|created_at| created_at.as_nanos());
    validate_service_incarnation(checkpoint.service_created_at, created_at)?;
    let registry_arn = match service.service_registries() {
        [] => None,
        [registry] => Some(
            registry
                .registry_arn()
                .filter(|value| !value.trim().is_empty())
                .context("ECS response returned a registry without an ARN")?,
        ),
        _ => anyhow::bail!("ECS response returned multiple registry identities"),
    };
    if registry_arn != checkpoint.registry_arn.as_deref() {
        anyhow::bail!("ECS response returned a conflicting registry identity");
    }
    Ok(())
}

async fn refresh_checkpointed_ecs(
    ecs: &aws_sdk_ecs::Client,
    checkpoint: &DeleteCheckpoint,
    context: &str,
) -> Result<ServiceIdentityState> {
    let response = ecs
        .describe_services()
        .cluster(&checkpoint.cluster_arn)
        .services(&checkpoint.service_arn)
        .send()
        .await
        .with_context(|| context.to_string())?;
    let reasons = failure_reasons(&response);
    let service = single_described_service(&response)?;
    if let Some(service) = service {
        validate_returned_service(checkpoint, service)?;
    }
    classify_service_identity(
        true,
        service.is_some(),
        service.and_then(|service| service.status()),
        &reasons,
    )
    .map_err(anyhow::Error::msg)
}

async fn delete_checkpointed_ecs(
    ecs: &aws_sdk_ecs::Client,
    checkpoint: &DeleteCheckpoint,
    state: ServiceIdentityState,
) -> Result<()> {
    if state == ServiceIdentityState::RetryGone {
        println!("  ✓ ECS service already absent; resuming exact dependent cleanup");
        return Ok(());
    }

    let response = ecs
        .describe_services()
        .cluster(&checkpoint.cluster_arn)
        .services(&checkpoint.service_arn)
        .send()
        .await
        .context("failed to refresh ECS service before mutation")?;
    let reasons = failure_reasons(&response);
    let service = single_described_service(&response)?;
    if let Some(service) = service {
        validate_returned_service(checkpoint, service)?;
    }
    let state = classify_service_identity(
        true,
        service.is_some(),
        service.and_then(|service| service.status()),
        &reasons,
    )
    .map_err(anyhow::Error::msg)?;
    if state == ServiceIdentityState::RetryGone {
        return Ok(());
    }
    let service = service.context("checkpointed ECS service disappeared ambiguously")?;
    let delete_phase = ecs_delete_phase(service.status())?;

    if delete_phase == EcsDeletePhase::Delete {
        match ecs
            .update_service()
            .cluster(&checkpoint.cluster_arn)
            .service(&checkpoint.service_arn)
            .desired_count(0)
            .send()
            .await
        {
            Ok(_) => {
                println!("  ✓ Scaled to 0");
                // The service may disappear or be recreated between scale and
                // delete. Re-describe the exact checkpointed ARN/incarnation
                // immediately before issuing delete_service.
                match refresh_checkpointed_ecs(
                    ecs,
                    checkpoint,
                    "failed to revalidate ECS service before delete",
                )
                .await?
                {
                    ServiceIdentityState::RetryGone => {
                        println!("  ✓ ECS service disappeared after scaling; skipping delete")
                    }
                    ServiceIdentityState::Live => {
                        match ecs
                            .delete_service()
                            .cluster(&checkpoint.cluster_arn)
                            .service(&checkpoint.service_arn)
                            .force(true)
                            .send()
                            .await
                        {
                            Ok(_) => println!("  ✓ ECS service deleted"),
                            Err(error) if error.code() == Some("ServiceNotFoundException") => {
                                println!("  ✓ ECS service disappeared while deleting")
                            }
                            Err(error) => {
                                return Err(error).context("failed to delete ECS service");
                            }
                        }
                    }
                }
            }
            Err(error) if error.code() == Some("ServiceNotFoundException") => {
                // A successful scale is not a prerequisite for exact polling;
                // never follow this response with delete_service.
                println!("  ✓ ECS service disappeared while scaling; skipping delete")
            }
            Err(error) => return Err(error).context("failed to scale ECS service to zero"),
        }
    } else {
        println!("  ✓ ECS service is already draining; resuming delete cleanup");
    }

    const DRAIN_POLL_ATTEMPTS: u32 = 12;
    const DRAIN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
    eprint!("  ⏳ Waiting for drain to complete...");
    for attempt in 0..DRAIN_POLL_ATTEMPTS {
        let complete = match ecs.describe_services()
            .cluster(&checkpoint.cluster_arn).services(&checkpoint.service_arn)
            .send().await {
            Ok(response) => {
                let reasons = failure_reasons(&response);
                let service = single_described_service(&response)?;
                if let Some(service) = service {
                    validate_returned_service(checkpoint, service)?;
                }
                match classify_service_identity(
                    true,
                    service.is_some(),
                    service.and_then(|service| service.status()),
                    &reasons,
                ) {
                    Ok(ServiceIdentityState::RetryGone) => true,
                    Ok(ServiceIdentityState::Live) => false,
                    Err(error) => {
                        eprintln!("\n  ⚠ ambiguous DescribeServices response (retrying): {error}");
                        false
                    }
                }
            }
            Err(error) => {
                eprintln!("\n  ⚠ describe_services error (retrying): {error}");
                false
            }
        };
        match drain_poll_action(complete, attempt, DRAIN_POLL_ATTEMPTS) {
            DrainPollAction::Complete => { eprintln!(" done"); return Ok(()); }
            DrainPollAction::Retry => {
                eprint!(".");
                tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
            }
            DrainPollAction::TimedOut => anyhow::bail!(
                "checkpointed ECS service did not reach an unambiguous absent state; dependent cleanup was not started (safe to retry)"
            ),
        }
    }
    unreachable!()
}

async fn cleanup_s3(
    s3: &aws_sdk_s3::Client, bucket: &str, namespace: &str, name: &str,
) -> Result<()> {
    let manifest_key = crate::identity::manifest_key(namespace, name);
    s3.delete_object().bucket(bucket).key(&manifest_key).send().await
        .context("failed to delete control-plane manifest")?;
    println!("  ✓ Manifest removed from S3");

    let artifact_prefix = format!("artifacts/{namespace}/{name}/");
    let mut continuation_token = None;
    loop {
        let response = s3.list_objects_v2().bucket(bucket).prefix(&artifact_prefix)
            .set_continuation_token(continuation_token).send().await
            .context("failed to list control-plane config artifacts")?;
        for object in response.contents() {
            if let Some(key) = object.key() {
                s3.delete_object().bucket(bucket).key(key).send().await
                    .context("failed to delete control-plane config artifact")?;
            }
        }
        continuation_token = response.next_continuation_token().map(str::to_owned);
        if continuation_token.is_none() { break; }
    }
    println!("  ✓ Config artifacts removed from S3");
    Ok(())
}

async fn run_with_bucket(
    aws_config: &aws_config::SdkConfig,
    resource: &str,
    name: &str,
    cluster: &str,
    namespace: &str,
    bucket: &str,
    cleanup_mode: CleanupMode,
) -> Result<Vec<String>> {
    if resource != "oabservice" {
        anyhow::bail!("unknown resource type: {resource}. Use 'oabservice'");
    }
    if namespace.trim().is_empty() || name.trim().is_empty() {
        anyhow::bail!(
            "delete requires a non-empty namespace and name (got '{namespace}'/'{name}')"
        );
    }
    let ecs = aws_sdk_ecs::Client::new(aws_config);
    let s3 = aws_sdk_s3::Client::new(aws_config);
    if namespace.contains('-') {
        // Legacy-deployment policy: hyphenated namespaces predate the shared
        // injective identity rule (see `crate::identity`). Programmatic
        // targets are rejected earlier; the CLI keeps them deletable, guarded
        // by the exclusive-ownership check below.
        eprintln!(
            "  ⚠ Namespace '{namespace}' contains '-', which predates the hyphen-free \
             namespace rule; continuing only if the control plane records no colliding \
             deployment for '{}'",
            crate::identity::physical_service_name(namespace, name)
        );
    }
    // Shared ownership rule: refuse to touch a physical identity that another
    // recorded logical pair (e.g. a legacy hyphenated namespace) claims.
    crate::identity::ensure_exclusive_physical_identity(&s3, bucket, namespace, name).await?;
    crate::apply::ensure_no_pending_ingress_teardown(&s3, bucket, namespace, name)
        .await?;
    println!("Deleting {name}...");

    let mut warnings = Vec::new();
    match prepare_identity(
        aws_config, &ecs, &s3, namespace, name, cluster, bucket,
    )
    .await? {
        PreparedIdentity::Exact(boxed) => {
            let (checkpoint, state) = *boxed;
            if state == ServiceIdentityState::RetryGone {
                warnings.push(
                    "ECS service was already absent; resumed exact dependent cleanup".to_string(),
                );
            }
            if checkpoint.registry_arn.is_none() && checkpoint.api_id.is_none() {
                warnings.push(
                    "no exact ingress identity was recorded; dependent ingress cleanup was skipped"
                        .to_string(),
                );
            }
            delete_checkpointed_ecs(&ecs, &checkpoint, state).await?;

            match cleanup_mode {
                CleanupMode::Strict => {
                    crate::ingress::delete_exact(
                        aws_config,
                        namespace,
                        name,
                        checkpoint.api_id.as_deref(),
                        checkpoint.registry_arn.as_deref(),
                        &checkpoint.partition,
                        &checkpoint.account,
                        &checkpoint.region,
                    )
                    .await?;
                    cleanup_s3(&s3, bucket, namespace, name).await?;
                    let key = checkpoint_key(namespace, name);
                    s3.delete_object().bucket(bucket).key(&key).send().await
                        .context("cleanup completed but failed to remove delete checkpoint")?;
                    println!("  ✓ Delete checkpoint removed");
                }
                CleanupMode::BestEffort => {
                    let mut cleanup_failed = false;
                    if let Err(error) = crate::ingress::delete_exact(
                        aws_config,
                        namespace,
                        name,
                        checkpoint.api_id.as_deref(),
                        checkpoint.registry_arn.as_deref(),
                        &checkpoint.partition,
                        &checkpoint.account,
                        &checkpoint.region,
                    )
                    .await
                    {
                        let warning = format!("ingress cleanup skipped: {error}");
                        eprintln!("  ⚠ {warning}");
                        warnings.push(warning);
                        cleanup_failed = true;
                    }
                    if let Err(error) = cleanup_s3(&s3, bucket, namespace, name).await {
                        let warning = format!("S3 cleanup incomplete (checkpoint retained): {error}");
                        eprintln!("  ⚠ {warning}");
                        warnings.push(warning);
                        cleanup_failed = true;
                    }
                    if !cleanup_failed {
                        let key = checkpoint_key(namespace, name);
                        if let Err(error) = s3.delete_object().bucket(bucket).key(&key).send().await {
                            let warning = format!("delete checkpoint removal skipped (checkpoint retained): {error}");
                            eprintln!("  ⚠ {warning}");
                            warnings.push(warning);
                            cleanup_failed = true;
                        } else {
                            println!("  ✓ Delete checkpoint removed");
                        }
                    }
                    if cleanup_failed {
                        let warning =
                            "delete checkpoint retained for a later exact-identity retry".to_string();
                        eprintln!("  ⚠ {warning}");
                        warnings.push(warning);
                    }
                }
            }
        }
    }
    println!("\n✓ {name} deleted");
    Ok(warnings)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn test_sdk_config() -> aws_config::SdkConfig {
        aws_config::SdkConfig::builder()
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build()
    }

    fn checkpoint() -> DeleteCheckpoint {
        DeleteCheckpoint {
            version: DELETE_CHECKPOINT_VERSION,
            namespace: "prod".to_string(),
            name: "bot".to_string(),
            bucket: "control-plane".to_string(),
            partition: "aws".to_string(),
            account: "123456789012".to_string(),
            region: "us-east-1".to_string(),
            requested_cluster: "cluster".to_string(),
            cluster_arn: "arn:aws:ecs:us-east-1:123456789012:cluster/cluster".to_string(),
            service_arn: "arn:aws:ecs:us-east-1:123456789012:service/cluster/oab-prod-bot".to_string(),
            service_created_at: 1_700_000_000,
            registry_arn: Some(
                "arn:aws:servicediscovery:us-east-1:123456789012:service/srv-123".to_string(),
            ),
            api_id: Some("api-123".to_string()),
        }
    }

    #[tokio::test]
    async fn delete_services_rejects_empty_target_set() {
        let cfg = test_sdk_config();
        let err = delete_services(
            &cfg,
            &[],
            &DeleteOptions::new("cluster").with_control_plane_bucket("control-plane"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, DeleteErrorKind::Validation);
        assert!(err.to_string().contains("empty target set"), "{err}");
    }

    #[tokio::test]
    async fn delete_services_rejects_empty_cluster() {
        let cfg = test_sdk_config();
        let targets = [DeleteTarget::new("prod", "bot")];
        let err = delete_services(
            &cfg,
            &targets,
            &DeleteOptions::new("").with_control_plane_bucket("control-plane"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, DeleteErrorKind::Validation);
        assert!(err.to_string().contains("cluster must not be empty"), "{err}");
    }

    #[test]
    fn delete_options_support_optional_bucket_override() {
        let options = DeleteOptions::new("cluster").with_control_plane_bucket("control-plane");
        assert_eq!(options.cluster, "cluster");
        assert_eq!(options.control_plane_bucket.as_deref(), Some("control-plane"));
    }

    #[tokio::test]
    async fn delete_services_rejects_blank_target_fields() {
        let cfg = test_sdk_config();
        let targets = [DeleteTarget::new("prod", "  ")];
        let err = delete_services(
            &cfg,
            &targets,
            &DeleteOptions::new("cluster").with_control_plane_bucket("control-plane"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, DeleteErrorKind::Validation);
    }

    #[test]
    fn delete_request_rejects_duplicate_targets_before_aws() {
        let targets = [
            DeleteTarget::new("prod", "bot"),
            DeleteTarget::new("prod", "bot"),
        ];
        let error = validate_delete_request(&targets, &DeleteOptions::new("cluster"))
            .expect_err("duplicate targets must fail validation");
        assert_eq!(error.kind, DeleteErrorKind::Validation);
        assert!(error.to_string().contains("duplicate delete target"), "{error}");
    }

    #[test]
    fn completed_delete_retains_non_fatal_warnings() {
        let target = DeleteTarget::new("prod", "bot");
        let report = record_delete_result(
            DeleteReport::default(),
            &target,
            Ok(vec!["resumed from an existing checkpoint".to_string()]),
        )
        .expect("completed delete should produce a report");
        assert_eq!(
            report.services[0].warnings,
            vec!["resumed from an existing checkpoint"]
        );
    }

    #[test]
    fn initial_describe_failures_and_missing_service_fail_closed() {
        assert!(classify_service_identity(false, false, None, &["MISSING"]).is_err());
        assert!(classify_service_identity(false, false, None, &["ACCESS_DENIED"]).is_err());
        assert!(classify_service_identity(false, false, None, &[]).is_err());
        assert!(classify_service_identity(false, true, Some("INACTIVE"), &[]).is_err());
        assert!(classify_service_identity(false, true, None, &[]).is_err());
    }

    #[test]
    fn exact_retry_checkpoint_authorizes_only_unambiguous_missing_identity() {
        assert_eq!(
            classify_service_identity(true, false, None, &["MISSING"]).unwrap(),
            ServiceIdentityState::RetryGone
        );
        assert_eq!(
            classify_service_identity(true, true, Some("INACTIVE"), &[]).unwrap(),
            ServiceIdentityState::RetryGone
        );
        assert!(classify_service_identity(true, false, None, &["ACCESS_DENIED"]).is_err());
        assert!(classify_service_identity(
            true,
            false,
            None,
            &["MISSING", "ACCESS_DENIED"]
        )
        .is_err());
        assert!(classify_service_identity(
            true,
            true,
            Some("DRAINING"),
            &["MISSING"]
        )
        .is_err());
        assert!(classify_service_identity(
            true,
            false,
            None,
            &["MISSING", "MISSING"]
        )
        .is_err());
        assert!(classify_service_identity(true, false, None, &[]).is_err());
        assert!(classify_service_identity(true, true, None, &[]).is_err());
        assert!(validate_service_incarnation(1_700_000_000, Some(1_700_000_001)).is_err());
        assert!(validate_service_incarnation(1_700_000_000, None).is_err());
        assert!(validate_service_incarnation(1_700_000_000, Some(1_700_000_000)).is_ok());
        assert_eq!(
            classify_service_identity(true, true, Some("DRAINING"), &[]).unwrap(),
            ServiceIdentityState::Live
        );
    }

    #[test]
    fn checkpoint_rejects_boundary_mismatch_and_accepts_exact_retry() {
        let checkpoint = checkpoint();
        validate_checkpoint(
            &checkpoint,
            "prod",
            "bot",
            "cluster",
            "control-plane",
            "aws",
            "123456789012",
            "us-east-1",
        )
        .unwrap();
        validate_checkpoint(
            &checkpoint,
            "prod",
            "bot",
            &checkpoint.cluster_arn,
            "control-plane",
            "aws",
            "123456789012",
            "us-east-1",
        )
        .unwrap();
        assert!(validate_checkpoint(
            &checkpoint,
            "prod",
            "bot",
            "cluster",
            "other-bucket",
            "aws",
            "123456789012",
            "us-east-1",
        )
        .is_err());
        assert!(validate_checkpoint(
            &checkpoint,
            "prod",
            "bot",
            "cluster",
            "control-plane",
            "aws",
            "999999999999",
            "us-east-1",
        )
        .is_err());

        let mut mismatched_cluster = checkpoint.clone();
        mismatched_cluster.requested_cluster = "other-cluster".to_string();
        assert!(validate_checkpoint(
            &mismatched_cluster,
            "prod",
            "bot",
            "cluster",
            "control-plane",
            "aws",
            "123456789012",
            "us-east-1",
        )
        .is_err());
        assert!(validate_checkpoint(
            &checkpoint,
            "prod",
            "bot",
            "other-cluster",
            "control-plane",
            "aws",
            "123456789012",
            "us-east-1",
        )
        .is_err());

        let mut mismatched_partition = checkpoint.clone();
        mismatched_partition.partition = "aws-cn".to_string();
        assert!(validate_checkpoint(
            &mismatched_partition,
            "prod",
            "bot",
            "cluster",
            "control-plane",
            "aws",
            "123456789012",
            "us-east-1",
        )
        .is_err());

        let mut invalid_dependency = checkpoint.clone();
        invalid_dependency.registry_arn = None;
        assert!(validate_checkpoint(
            &invalid_dependency,
            "prod",
            "bot",
            "cluster",
            "control-plane",
            "aws",
            "123456789012",
            "us-east-1",
        )
        .is_err());

        invalid_dependency.api_id = None;
        invalid_dependency.registry_arn = Some("srv-not-an-arn".to_string());
        assert!(validate_checkpoint(
            &invalid_dependency,
            "prod",
            "bot",
            "cluster",
            "control-plane",
            "aws",
            "123456789012",
            "us-east-1",
        )
        .is_err());

        let mut invalid_incarnation = checkpoint.clone();
        invalid_incarnation.service_created_at = 0;
        assert!(validate_checkpoint(
            &invalid_incarnation,
            "prod",
            "bot",
            "cluster",
            "control-plane",
            "aws",
            "123456789012",
            "us-east-1",
        )
        .is_err());
    }

    #[test]
    fn checkpoint_rejects_arn_boundary_mismatches() {
        let cases = [
            (
                "arn:aws:ecs:us-east-1:999999999999:cluster/cluster",
                "arn:aws:ecs:us-east-1:123456789012:service/cluster/oab-prod-bot",
                "arn:aws:servicediscovery:us-east-1:123456789012:service/srv-123",
            ),
            (
                "arn:aws:ecs:us-west-2:123456789012:cluster/cluster",
                "arn:aws:ecs:us-east-1:123456789012:service/cluster/oab-prod-bot",
                "arn:aws:servicediscovery:us-east-1:123456789012:service/srv-123",
            ),
            (
                "arn:aws:ecs:us-east-1:123456789012:cluster/cluster",
                "arn:aws:ecs:us-east-1:123456789012:service/other/oab-prod-bot",
                "arn:aws:servicediscovery:us-east-1:123456789012:service/srv-123",
            ),
            (
                "arn:aws:ecs:us-east-1:123456789012:cluster/cluster",
                "arn:aws:ecs:us-east-1:123456789012:service/cluster/oab-prod-other",
                "arn:aws:servicediscovery:us-east-1:123456789012:service/srv-123",
            ),
            (
                "arn:aws:ecs:us-east-1:123456789012:cluster/cluster",
                "arn:aws:ecs:us-east-1:123456789012:service/cluster/oab-prod-bot",
                "arn:aws:servicediscovery:us-east-1:999999999999:service/srv-123",
            ),
            (
                "arn:aws:ecs:us-east-1:123456789012:cluster/cluster",
                "arn:aws:ecs:us-east-1:123456789012:service/cluster/oab-prod-bot",
                "arn:aws:servicediscovery:us-west-2:123456789012:service/srv-123",
            ),
            (
                "arn:aws:ecs:us-east-1:123456789012:cluster/cluster",
                "arn:aws-cn:ecs:us-east-1:123456789012:service/cluster/oab-prod-bot",
                "arn:aws:servicediscovery:us-east-1:123456789012:service/srv-123",
            ),
        ];

        for (cluster_arn, service_arn, registry_arn) in cases {
            let mut candidate = checkpoint();
            candidate.cluster_arn = cluster_arn.to_string();
            candidate.service_arn = service_arn.to_string();
            candidate.registry_arn = Some(registry_arn.to_string());
            assert!(validate_checkpoint(
                &candidate,
                "prod",
                "bot",
                "cluster",
                "control-plane",
                "aws",
                "123456789012",
                "us-east-1",
            )
            .is_err());
        }
    }

    #[test]
    fn delete_target_derives_ecs_service_name() {
        assert_eq!(
            DeleteTarget::new("prod", "nest-my-oab").ecs_service_name(),
            "oab-prod-nest-my-oab"
        );
    }

    #[tokio::test]
    async fn ambiguous_namespace_delimiter_is_rejected_before_aws() {
        let opts = DeleteOptions::new("cluster");
        let accepted = [DeleteTarget::new("prod", "team-bot")];
        let rejected = [DeleteTarget::new("prod-team", "bot")];
        assert_eq!(
            accepted[0].ecs_service_name(),
            rejected[0].ecs_service_name()
        );
        assert!(validate_delete_request(&accepted, &opts).is_ok());

        let error = delete_services(&test_sdk_config(), &rejected, &opts)
            .await
            .unwrap_err();
        assert_eq!(error.kind, DeleteErrorKind::Validation);
        assert!(error.to_string().contains("must not contain '-'"), "{error}");
    }

    #[test]
    fn colliding_logical_pairs_guard_every_entry_point() {
        // `prod/team-bot` and `prod-team/bot` share the physical identity
        // `oab-prod-team-bot`. Cross-entry-point regression for the shared
        // rule in `crate::identity`:
        //
        // 1. Apply cannot create the hyphenated-namespace side (domain rule in
        //    manifest validation, exercised in `manifest`/`apply` tests) and
        //    programmatic delete cannot target it
        //    (`ambiguous_namespace_delimiter_is_rejected_before_aws` above).
        assert!(
            crate::identity::validate_injective_identity("prod-team", "bot").is_err(),
            "hyphenated namespace must be outside the accepted identity domain"
        );
        // 2. For the accepted side, the pre-mutation ownership probe shared by
        //    apply (`apply_ecs`) and every delete entry point
        //    (`run_with_bucket`) checks exactly the legacy pair's manifest and
        //    checkpoint keys, so a recorded `prod-team/bot` blocks overwrite,
        //    checkpointing, and deletion of `oab-prod-team-bot` via
        //    `prod/team-bot` — and vice versa.
        assert_eq!(
            crate::identity::collision_aliases("prod", "team-bot"),
            vec![("prod-team".to_string(), "bot".to_string())]
        );
        assert!(crate::identity::collision_aliases("prod-team", "bot")
            .contains(&("prod".to_string(), "team-bot".to_string())));
        // 3. The probed ownership keys are derived from the same helpers the
        //    mutation paths use, so probe and mutation cannot drift apart.
        assert_eq!(
            crate::identity::ownership_keys("prod-team", "bot")[1],
            checkpoint_key("prod-team", "bot"),
        );
    }

    #[test]
    fn later_failure_preserves_the_completed_partial_report() {
        let first = DeleteTarget::new("prod", "first");
        let second = DeleteTarget::new("prod", "second");
        let report = record_delete_result(DeleteReport::default(), &first, Ok(Vec::new()))
            .expect("first target should complete");
        let error = record_delete_result(
            report,
            &second,
            Err(anyhow::anyhow!("synthetic teardown failure")),
        )
        .unwrap_err();

        assert_eq!(error.kind, DeleteErrorKind::Teardown);
        assert_eq!(error.failed_service.as_ref(), Some(&second));
        assert_eq!(error.completed.services.len(), 1);
        assert_eq!(error.completed.services[0].name, "first");
    }

    #[test]
    fn pending_apply_ingress_teardown_blocks_delete_completion() {
        assert!(crate::apply::require_no_pending_ingress_teardown(false).is_ok());
        let error = crate::apply::require_no_pending_ingress_teardown(true).unwrap_err();
        assert!(error.to_string().contains("re-run the ingress-free apply"));
    }

    #[test]
    fn drain_poll_action_requires_completion_before_cleanup() {
        assert_eq!(drain_poll_action(true, 0, 12), DrainPollAction::Complete);
        assert_eq!(drain_poll_action(false, 0, 12), DrainPollAction::Retry);
        assert_eq!(drain_poll_action(false, 11, 12), DrainPollAction::TimedOut);
    }

    #[test]
    fn delete_phase_requests_delete_only_for_active_service() {
        assert_eq!(ecs_delete_phase(Some("ACTIVE")).unwrap(), EcsDeletePhase::Delete);
        assert_eq!(ecs_delete_phase(Some("DRAINING")).unwrap(), EcsDeletePhase::Drain);
        assert_eq!(ecs_delete_phase(Some("INACTIVE")).unwrap(), EcsDeletePhase::Cleanup);
        assert_eq!(ecs_delete_phase(None).unwrap(), EcsDeletePhase::Cleanup);
        assert!(ecs_delete_phase(Some("UNKNOWN")).is_err());
    }
}
