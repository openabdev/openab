# oabctl — OAB Agent Provisioner

CLI tool that provisions and manages OpenAB agents on Amazon ECS Fargate (with Kubernetes support planned).

> 📖 **Full usage guide** — installation, manifest schema, ingress/webhooks,
> secrets, bootstrap, and the commands reference: **[docs/oabctl.md](../docs/oabctl.md)**

## How It Works

```
┌─────────────────────────────────────────────────────────────────────────┐
│  Developer Machine                                                       │
│                                                                          │
│  oabctl bootstrap ──► Creates: ECS Cluster, IAM Roles, S3, SG, Logs    │
│                                                                          │
│  oabctl create ─────► Wizard → config.toml + manifest.yaml (local)      │
│       │                  │                                               │
│       │                  └─► Secrets Manager: oab/{ns}/{name}            │
│       │                                                                  │
│  oabctl apply                                                            │
│       │                                                                  │
│       ├─► S3: Upload config.toml to artifacts/{ns}/{name}/              │
│       ├─► ECS: Register Task Definition                                  │
│       └─► ECS: Create/Update Service                                     │
│                                                                          │
│  oabctl exec/cp/sync ──► ecsctl library ──► ECS Exec (SSM)             │
└──────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────┐
│  AWS Cloud                                                               │
│                                                                          │
│  ┌─────────────┐     ┌──────────────────────────────────────────┐       │
│  │ S3 Bucket   │     │ ECS Cluster (oab)                        │       │
│  │             │     │                                          │       │
│  │ bootstrap/  │     │  ┌─────────────────────────────────┐    │       │
│  │   state.json│     │  │ Fargate Task (agent)             │    │       │
│  │             │     │  │                                  │    │       │
│  │ manifests/  │     │  │  ┌────────────────────────────┐ │    │       │
│  │   *.yaml    │     │  │  │ OpenAB Container           │ │    │       │
│  │             │     │  │  │                            │ │    │       │
│  │ artifacts/  │◄────┼──┼──│ 1. Download config.toml    │ │    │       │
│  │   config.toml     │  │  │ 2. Resolve [secrets.refs]  │─┼────┼──►SM  │
│  │             │     │  │  │ 3. Start agent             │ │    │       │
│  └─────────────┘     │  │  └────────────────────────────┘ │    │       │
│                       │  └─────────────────────────────────┘    │       │
│  ┌──────────────┐    └──────────────────────────────────────────┘       │
│  │ Secrets Mgr  │                                                        │
│  │ oab/{ns}/{n} │    ┌───────────────┐                                  │
│  │  BOT_TOKEN   │    │ CloudWatch    │                                  │
│  │  STT_API_KEY │    │ /oab/agents   │                                  │
│  └──────────────┘    └───────────────┘                                  │
└─────────────────────────────────────────────────────────────────────────┘
```

## Quick Start

```bash
# 1. Bootstrap infrastructure (one-time)
oabctl bootstrap

# 2. Create an agent (generates config + manifest)
oabctl create my-bot

# 3. Review generated files, then deploy
oabctl apply -f my-bot/manifest.yaml --wait

# 4. Done! Agent is running.
oabctl exec my-bot -- bash
```

See **[docs/oabctl.md](../docs/oabctl.md)** for installation instructions,
the full manifest schema (including ingress/webhooks for Telegram and LINE),
secrets formats, bootstrap details, IAM permission tables, and the complete
commands reference.

## Library API

The crate exposes a deliberately narrow manifest + apply/delete facade for
control planes that should not shell out to the CLI:

```rust,no_run
use oabctl::{
    apply_manifests, delete_services, ApplyOptions, DeleteOptions, DeleteTarget,
    OABServiceManifest,
};

async fn reconcile(
    aws: &aws_config::SdkConfig,
    manifest: OABServiceManifest,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = apply_manifests(
        aws,
        &[manifest],
        &ApplyOptions::new("production-cluster").with_wait(true),
    )
    .await?;
    for service in report.services {
        println!("{}: {:?}", service.ecs_service_name, service.action);
    }

    delete_services(
        aws,
        &[DeleteTarget::new("prod", "bot")],
        // Omit the bucket override to use OAB_CONTROL_PLANE_BUCKET,
        // then oab-control-plane-{account} from the caller identity.
        &DeleteOptions::new("production-cluster"),
    )
    .await?;
    Ok(())
}
```

Programmatic apply/delete emit no progress to process-global stdout/stderr.
Apply success returns per-service actions, webhook URLs, and warnings;
reconciliation errors identify the failed service and include the report
completed before the failure. Both CLI and programmatic apply verify that the
target cluster exists and is `ACTIVE` before mutation, so the caller identity
requires `ecs:DescribeClusters`. This ECS action does not support resource-level
permissions; its IAM statement must use `Resource: "*"`.

The library never reads `~/.oabctl/config.toml`. Apply and delete resolve the
control-plane bucket through the shared chain: an explicit
`with_control_plane_bucket(...)` override, then `OAB_CONTROL_PLANE_BUCKET`, then
`oab-control-plane-{account}` from the caller identity. Before deleting ECS,
`delete_services` stores the resolved bucket, caller partition/account/region,
canonical ECS cluster/service ARNs, the service `createdAt` incarnation, and
exact ingress IDs in `delete-checkpoints/<namespace>/<name>.json`. The checkpoint
is written before ECS mutation and removed last. Initial DescribeServices
responses must identify exactly one expected live service; missing, empty-success,
ambiguous, or mixed-failure responses fail closed. Retries use only checkpointed
IDs after ECS explicitly reports exactly one `MISSING` failure with zero services
or a matching original `INACTIVE` incarnation. Duplicate APIs fail closed,
and a sole same-name API is checkpointed only when an integration URI exactly
matches the ECS registry ARN.
There is no name-only API, Cloud Map, or S3 orphan cleanup fallback. If a
per-target apply-side `ingress-teardown-checkpoints/<namespace>/<name>.json`
record exists, delete fails before destructive mutation so it cannot discard
unfinished exact cleanup identity. Re-run the ingress-free apply to completion,
then retry delete.

The manifest is persisted as the infrastructure desired state before ECS
mutation. If a later AWS/configuration step fails, correct the underlying issue
and re-run `apply`/`apply_manifests` to reconcile the persisted desired state;
apply has no rollback checkpoint.
The control-plane bucket must enforce default server-side encryption and S3
versioning for durable checkpoint recovery. The library writes application JSON
but does not weaken or validate those bucket-level policies per request. The
legacy CLI warns on a drain timeout, continues dependent/S3 cleanup, retains the
checkpoint for retry, and continues after individual S3 object failures, while
`delete_services` keeps exact-identity and checkpoint cleanup failures fatal so
callers can retry safely.

Apply and delete share one injective logical-identity rule: namespaces must not
contain `-`; names may contain it, so the physical `oab-{namespace}-{name}`
identity always parses back to exactly one `namespace`/`name` pair. Namespaces
must match `[a-z0-9]+` and names must match `[a-z0-9][a-z0-9-]*`; this also
prevents `/`, whitespace, `_`, `.`, and Unicode from becoming S3 path
components. Manifest validation (CLI apply, fleet expansion, and
`apply_manifests`) and `delete_services` all reject hyphenated namespaces before
any AWS call. The legacy CLI delete path permits hyphenated namespaces only
when the same S3-safe character rules otherwise pass. Before mutating, apply
and every delete entry point additionally verify in the control plane that no
*other* recorded logical pair (manifest, delete checkpoint, or
ingress-teardown checkpoint) claims the same physical name, and fail closed
with the colliding pair named in the error.

Legacy policy: deployments created under a hyphenated namespace before this
rule cannot be re-applied — apply fails with a migration message. They remain
deletable through `oabctl delete`, which accepts a hyphenated namespace for
teardown, prints a warning, and relies on the same control-plane ownership
check to refuse contested physical names. Migrate by deleting the legacy
deployment and re-creating it under a hyphen-free namespace.

Programmatic apply and delete do not provide an internal same-target lock.
Callers must serialize mutations for the same AWS account, Region, control-plane
bucket, ECS cluster, and physical service identity. This includes every
alias-equivalent logical pair that could map to the same
`oab-{namespace}-{name}` value (for example `prod/team-bot` and
`prod-team/bot`). The complete delete target set is validated before any AWS
request; duplicate logical targets are rejected. If overlapping calls occur,
stop concurrent writers, inspect the retained checkpoint, then explicitly
re-apply the desired state or retry delete.

For `aws-sm://<secret-id>#<json-key>`, a non-ARN `<secret-id>` requires the
caller to have `secretsmanager:DescribeSecret`; full-ARN shorthand does not
need that lookup.

## Source Layout

```
operator/
├── src/
│   ├── main.rs        # Thin binary entrypoint (`oabctl::run_cli()`)
│   ├── cli.rs         # Private CLI definitions and subcommand dispatch
│   ├── manifest.rs    # Publicly re-exported manifest model + validation
│   ├── apply.rs       # apply: ECS task def registration, service create/update
│   ├── bootstrap.rs   # bootstrap: cluster/IAM/S3/SG/log-group provisioning
│   ├── ingress.rs     # ingress: Cloud Map + VPC Link + API Gateway reconciliation
│   ├── secrets.rs     # spec.secrets value resolution (ECS-native + aws-sm:// shorthand)
│   ├── create.rs      # create: interactive wizard
│   ├── get.rs         # get: list/describe agents
│   └── delete.rs      # delete: teardown
└── schema/
    └── oabservice-v2.json  # JSON Schema for IDE validation
```

## JSON Schema

[`schema/oabservice-v2.json`](schema/oabservice-v2.json) — supports both OABService and OABFleet for IDE validation.
