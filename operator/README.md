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

The crate exposes a deliberately narrow manifest + apply facade for control
planes that should not shell out to the CLI:

```rust,no_run
use oabctl::{apply_manifests, ApplyOptions, OABServiceManifest};

async fn deploy(
    aws: &aws_config::SdkConfig,
    manifest: OABServiceManifest,
) -> Result<(), oabctl::ApplyError> {
    let report = apply_manifests(
        aws,
        &[manifest],
        &ApplyOptions::new("production-cluster").with_wait(true),
    )
    .await?;
    for service in report.services {
        println!("{}: {:?}", service.ecs_service_name, service.action);
    }
    Ok(())
}
```

Programmatic apply emits no progress to process-global stdout/stderr. Success
returns per-service actions, webhook URLs, and warnings; reconciliation errors
identify the failed service and include the report completed before the failure.
Both CLI and programmatic apply verify that the target cluster exists and is
`ACTIVE` before mutation, so the caller identity requires
`ecs:DescribeClusters`. This ECS action does not support resource-level
permissions; its IAM statement must use `Resource: "*"`.
The library never reads `~/.oabctl/config.toml`: set
`with_control_plane_bucket(...)` explicitly when needed, otherwise bucket
resolution uses `OAB_CONTROL_PLANE_BUCKET` and then the caller's AWS account.
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
