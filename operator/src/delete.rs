use anyhow::{Context, Result};
use aws_sdk_ecs::error::ProvideErrorMetadata;
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
/// addressed by `namespace` + `name` alone.
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
        format!("oab-{}-{}", self.namespace, self.name)
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
    /// the caller's AWS identity — the same chain as apply, so delete always
    /// cleans the bucket apply wrote to.
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
    /// Best-effort steps that were skipped (e.g. ingress teardown pieces
    /// already gone). The core teardown (ECS service, S3 manifest and
    /// artifacts) either completed or the whole target failed.
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

fn validate_delete_request(
    targets: &[DeleteTarget],
    cluster: &str,
) -> std::result::Result<(), DeleteError> {
    if targets.is_empty() {
        return Err(DeleteError::validation(anyhow::anyhow!(
            "no targets to delete (empty target set)"
        )));
    }
    if cluster.trim().is_empty() {
        return Err(DeleteError::validation(anyhow::anyhow!(
            "DeleteOptions.cluster must not be empty"
        )));
    }
    for target in targets {
        if target.namespace.trim().is_empty() || target.name.trim().is_empty() {
            return Err(DeleteError::validation(anyhow::anyhow!(
                "delete target namespace and name must not be empty (got '{}'/'{}')",
                target.namespace,
                target.name
            )));
        }
    }
    Ok(())
}

/// Tear down OAB services programmatically, without reading CLI home
/// configuration or writing progress to process-global stdout/stderr.
///
/// Contract (enforced before any AWS call):
/// - the target set must be non-empty
/// - [`DeleteOptions::cluster`] must be a non-empty cluster name
/// - every target's `namespace`/`name` must be non-empty
///
/// Teardown per target is resumable: an `ACTIVE` service is scaled down and
/// deleted, a `DRAINING` service resumes its drain wait, and an absent one
/// proceeds straight to ingress/S3 cleanup. Best-effort ingress steps are
/// reported through [`DeletedService::warnings`]; S3 cleanup failures fail
/// the target (safe to retry).
pub async fn delete_services(
    aws_config: &aws_config::SdkConfig,
    targets: &[DeleteTarget],
    opts: &DeleteOptions,
) -> std::result::Result<DeleteReport, DeleteError> {
    crate::apply::with_progress_suppressed(async {
        validate_delete_request(targets, &opts.cluster)?;
        let bucket = crate::control_plane::resolve_bucket(
            aws_config,
            opts.control_plane_bucket.as_deref(),
        )
        .await
        .map_err(DeleteError::target)?;

        let mut report = DeleteReport::default();
        for target in targets {
            match run_with_bucket(
                aws_config,
                "oabservice",
                &target.name,
                &opts.cluster,
                &target.namespace,
                &bucket,
            )
            .await
            {
                Ok(warnings) => report.services.push(DeletedService {
                    namespace: target.namespace.clone(),
                    name: target.name.clone(),
                    ecs_service_name: target.ecs_service_name(),
                    warnings,
                }),
                Err(error) => {
                    return Err(DeleteError::teardown(target.clone(), report, error));
                }
            }
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

fn collect_best_effort_warnings(
    warnings: &mut Vec<String>,
    result: Result<Vec<String>>,
    failure_context: &str,
) {
    match result {
        Ok(step_warnings) => warnings.extend(step_warnings),
        Err(error) => {
            let warning = format!("{failure_context}: {error}");
            eprintln!("  ⚠ {warning}");
            warnings.push(warning);
        }
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
    run_with_bucket(aws_config, resource, name, cluster, namespace, &bucket)
        .await
        .map(|_warnings| ())
}

async fn run_with_bucket(
    aws_config: &aws_config::SdkConfig,
    resource: &str,
    name: &str,
    cluster: &str,
    namespace: &str,
    bucket: &str,
) -> Result<Vec<String>> {
    if resource != "oabservice" {
        anyhow::bail!("unknown resource type: {resource}. Use 'oabservice'");
    }

    let mut warnings = Vec::new();

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
            match drain_poll_action(is_gone, attempt, DRAIN_POLL_ATTEMPTS) {
                DrainPollAction::Complete => {
                    if attempt == 0 {
                        eprintln!(" done (immediate)");
                    } else {
                        let elapsed = u64::from(attempt) * DRAIN_POLL_INTERVAL.as_secs();
                        eprintln!(" done ({elapsed}s)");
                    }
                    break;
                }
                DrainPollAction::Retry => {
                    eprint!(".");
                    tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
                }
                DrainPollAction::TimedOut => {
                    let elapsed = u64::from(attempt) * DRAIN_POLL_INTERVAL.as_secs();
                    eprintln!(" timed out ({elapsed}s)");
                    anyhow::bail!(
                        "ECS service {service_name} is still draining after {elapsed}s; dependent cleanup was not started (safe to retry)"
                    );
                }
            }
        }
    }

    let ingress_result =
        crate::ingress::teardown(aws_config, namespace, name, registry_arn.as_deref()).await;
    collect_best_effort_warnings(&mut warnings, ingress_result, "ingress teardown skipped");

    let delete_api_result = crate::ingress::delete_api(aws_config, namespace, name)
        .await
        .map(|()| Vec::new());
    collect_best_effort_warnings(&mut warnings, delete_api_result, "HTTP API cleanup skipped");

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

    #[tokio::test]
    async fn delete_services_rejects_empty_target_set() {
        let cfg = test_sdk_config();
        let err = delete_services(&cfg, &[], &DeleteOptions::new("cluster"))
            .await
            .unwrap_err();
        assert_eq!(err.kind, DeleteErrorKind::Validation);
        assert!(err.to_string().contains("empty target set"), "{err}");
    }

    #[tokio::test]
    async fn delete_services_rejects_empty_cluster() {
        let cfg = test_sdk_config();
        let targets = [DeleteTarget::new("prod", "bot")];
        let err = delete_services(&cfg, &targets, &DeleteOptions::new(""))
            .await
            .unwrap_err();
        assert_eq!(err.kind, DeleteErrorKind::Validation);
        assert!(err.to_string().contains("cluster must not be empty"), "{err}");
    }

    #[tokio::test]
    async fn delete_services_rejects_blank_target_fields() {
        let cfg = test_sdk_config();
        let targets = [DeleteTarget::new("prod", "  ")];
        let err = delete_services(&cfg, &targets, &DeleteOptions::new("cluster"))
            .await
            .unwrap_err();
        assert_eq!(err.kind, DeleteErrorKind::Validation);
    }

    #[test]
    fn delete_target_derives_ecs_service_name() {
        assert_eq!(
            DeleteTarget::new("prod", "nest-my-oab").ecs_service_name(),
            "oab-prod-nest-my-oab"
        );
    }

    #[test]
    fn drain_poll_action_requires_completion_before_cleanup() {
        assert_eq!(drain_poll_action(true, 0, 12), DrainPollAction::Complete);
        assert_eq!(drain_poll_action(false, 0, 12), DrainPollAction::Retry);
        assert_eq!(drain_poll_action(false, 11, 12), DrainPollAction::TimedOut);
    }

    #[test]
    fn best_effort_warnings_preserve_step_warnings_and_errors() {
        let mut warnings = Vec::new();
        collect_best_effort_warnings(
            &mut warnings,
            Ok(vec!["route cleanup incomplete".to_string()]),
            "ingress teardown skipped",
        );
        collect_best_effort_warnings(
            &mut warnings,
            Err(anyhow::anyhow!("access denied")),
            "HTTP API cleanup skipped",
        );

        assert_eq!(
            warnings,
            vec![
                "route cleanup incomplete",
                "HTTP API cleanup skipped: access denied",
            ]
        );
    }

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
