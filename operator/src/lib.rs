//! Programmatic OAB manifest validation and ECS reconciliation.
//!
//! The public facade contains the manifest model and the structured apply
//! and delete APIs. CLI implementation details and other resource-management
//! helpers remain private.
//!
//! # Example
//!
//! ```no_run
//! use oabctl::{apply_manifests, ApplyOptions, OABServiceManifest};
//!
//! # async fn deploy() -> Result<(), Box<dyn std::error::Error>> {
//! let manifest: OABServiceManifest = serde_yaml::from_str(r#"
//! apiVersion: oab.dev/v2
//! kind: OABService
//! metadata: { name: bot, namespace: prod }
//! spec:
//!   image: example.com/openab:latest
//!   resources: { cpu: "256", memory: "512" }
//!   configFrom: s3://example/config.toml
//!   runtime:
//!     type: ecs
//!     networking: { subnets: [subnet-123], securityGroups: [sg-123] }
//! "#)?;
//! let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
//! let report = apply_manifests(
//!     &aws,
//!     &[manifest],
//!     &ApplyOptions::new("production-cluster"),
//! ).await?;
//! println!("reconciled {} service(s)", report.services.len());
//! # Ok(())
//! # }
//! ```
//!
//! Both CLI and programmatic apply perform an ECS cluster preflight. The
//! caller identity therefore requires `ecs:DescribeClusters`; this ECS action
//! does not support resource-level permissions, so its IAM statement must use
//! `Resource: "*"` even though the request targets the configured cluster.
//!
//! `aws-sm://<secret-id>#<json-key>` values whose `<secret-id>` is not already
//! an ARN require the apply caller to have `secretsmanager:DescribeSecret`, so
//! oabctl can resolve the name to the full ARN required by ECS.
//!
//! Programmatic apply and delete never read `~/.oabctl/config.toml`. Use
//! [`ApplyOptions::with_control_plane_bucket`] or
//! [`DeleteOptions::with_control_plane_bucket`] for an explicit bucket;
//! otherwise both use the shared `OAB_CONTROL_PLANE_BUCKET` then caller-account
//! resolution chain.
//!
//! [`delete_services`] tears down by `namespace`+`name` ([`DeleteTarget`]).
//! Before ECS mutation it durably records the resolved bucket, caller
//! partition/account/region, canonical cluster/service ARNs, the ECS service
//! `createdAt` incarnation, and exact ingress IDs in S3. Initial identity
//! ambiguity fails closed; retries use only checkpointed IDs and remove the
//! checkpoint last. There is no name-only orphan cleanup fallback.

pub mod apply;
mod bootstrap;
mod cli;
mod config;
mod control_plane;
mod create;
pub mod delete;
mod get;
mod ingress;
pub mod manifest;
mod scale;
mod secrets;

pub use apply::{
    apply_manifests, AppliedService, ApplyAction, ApplyError, ApplyErrorKind, ApplyOptions,
    ApplyReport, ServiceTarget,
};
pub use delete::{
    delete_services, DeleteError, DeleteErrorKind, DeleteOptions, DeleteReport, DeleteTarget,
    DeletedService,
};
pub use manifest::{
    AgentOverride, EcsNetworking, EcsRuntime, FleetMetadata, FleetSpec, FleetTemplate, Ingress,
    KubernetesRuntime, Metadata, OABFleetManifest, OABServiceManifest, RawManifest, Resources,
    Runtime, Spec,
};

#[doc(hidden)]
pub use cli::run_cli;
