# oabctl — OAB Agent Provisioner

CLI tool that provisions and manages OpenAB agents on Amazon ECS Fargate (with Kubernetes support planned).

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

Or skip the review step:
```bash
oabctl create my-bot --auto-apply   # generate + deploy in one shot
```

## Complete Workflow

### Step 1: Bootstrap (one-time)

```bash
oabctl bootstrap
```

Creates all required AWS infrastructure with one command. Shows a plan and asks for confirmation before creating anything.

### Step 2: Create Agent (wizard)

```bash
oabctl create my-bot
```

Interactive wizard that:
1. Selects backend platform (kiro/claude-code/codex/gemini/copilot/opencode)
2. Selects release channel (stable/beta) → resolves official image URI
3. Prompts for Discord bot token (masked input) → stores in Secrets Manager
4. Prompts for STT API key (optional, masked) → stores in same secret
5. Selects runtime (ECS)
6. Selects capacity provider (FARGATE_SPOT/FARGATE)
7. Selects VPC
8. Auto-selects subnets (private+NAT priority, 2-3 AZ)
9. Selects or creates security group
10. Generates local files → confirms → applies

Output:
```
my-bot/
├── config.toml      ← OpenAB agent configuration
└── manifest.yaml    ← oabctl deployment manifest
```

### Step 3: Day-to-day Operations

```bash
oabctl get oabservice                # list agents
oabctl exec my-bot -- bash           # shell into container
oabctl cp data.bin my-bot:/tmp/      # upload file
oabctl sync ./skills my-bot:/home/agent/.kiro/skills/  # sync directory
```

### Updating Config

```bash
vim my-bot/config.toml                           # edit locally
oabctl apply -f my-bot/manifest.yaml             # syncs config + redeploys
oabctl apply -f my-bot/manifest.yaml --no-sync   # redeploy only (skip config sync)
```

### Fleet Deploy

```bash
oabctl apply -f fleet.yaml           # deploy 10+ agents from one file
```

## Configuration

### Local Config (`~/.oabctl/config.toml`)

Auto-created by `bootstrap`. Stores persistent settings:

```toml
[defaults]
namespace = "prod"
cluster = "oab"
# region = "us-east-1"

[bootstrap]
bucket = "oab-control-plane-123456789"
```

**Priority:** config.toml > `OAB_CONTROL_PLANE_BUCKET` env var > auto-derive from account

### Agent Config (`{name}/config.toml`)

Generated by `oabctl create`, uploaded to S3 via `apply --sync`. This is the OpenAB runtime configuration:

```toml
[secrets.refs]
discord_bot_token = "aws-sm://oab/prod/my-bot#DISCORD_BOT_TOKEN"
stt_api_key = "aws-sm://oab/prod/my-bot#STT_API_KEY"

[discord]
bot_token = "${secrets.discord_bot_token}"
allow_all_channels = true
allow_all_users = true
max_bot_turns = 1000
message_processing_mode = "per-thread"

[agent]
inherit_env = ["AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", "AWS_DEFAULT_REGION"]

[pool]
max_sessions = 5
session_ttl_hours = 1

[reactions]
enabled = true

[stt]
enabled = true
api_key = "${secrets.stt_api_key}"
model = "whisper-large-v3-turbo"
base_url = "https://api.groq.com/openai/v1"

[cron]
usercron_enabled = true
usercron_path = "cronjob.toml"
```

Secrets are resolved by OpenAB at runtime using the task role's Secrets Manager permissions.

## Manifest Schema (oab.dev/v2)

### OABService — single agent

```yaml
apiVersion: oab.dev/v2
kind: OABService
metadata:
  name: my-bot
  namespace: prod
spec:
  image: public.ecr.aws/oablab/kiro:stable
  resources:
    cpu: "256"
    memory: "512"
  configFrom: s3://oab-control-plane-123456789/artifacts/prod/my-bot/config.toml
  runtime:
    type: ecs
    capacityProvider: FARGATE_SPOT
    networking:
      subnets: [subnet-aaa, subnet-bbb]
      securityGroups: [sg-xxx]
```

### Ingress — inbound webhooks (Telegram / LINE)

Discord bots are outbound-only and need no ingress. Webhook platforms (Telegram,
LINE, ...) POST *into* the task, so they need a public HTTPS endpoint. Adding an
optional `spec.ingress` block makes `oabctl apply` provision the cheapest
AWS-native path in one shot — API Gateway HTTP API → VPC Link → Cloud Map → the
task — instead of running ~7 manual `aws` commands, replacing the manual steps
implemented here. For a Kubernetes/Cloudflare-Tunnel alternative, see
[`docs/refarch/telegram-cloudflare-tunnel.md`](../docs/refarch/telegram-cloudflare-tunnel.md).
A dedicated AWS-native refarch doc covering this path in depth is tracked in
[#1274](https://github.com/openabdev/openab/pull/1274); once merged it will be
linked here.

```yaml
spec:
  image: public.ecr.aws/oablab/kiro:beta
  resources: { cpu: "256", memory: "512" }
  configFrom: s3://.../config.toml
  runtime:
    type: ecs
    capacityProvider: FARGATE_SPOT
    networking:
      subnets: [subnet-aaa, subnet-bbb]
      securityGroups: [sg-xxx]
  ingress:
    type: apigateway          # only supported type (default)
    cloudMapNamespace: oab    # reused across bots in the VPC (default: oab)
    containerPort: 8080       # OpenAB listen port (default: 8080)
    paths:
      - /webhook/telegram
      - /webhook/line
```

On `apply` this reconciles (idempotently, reused by name):

1. **Cloud Map** private DNS namespace (`<cloudMapNamespace>-<vpc-id>`, shared per-VPC) + a per-service **SRV** record (carries the container port; a plain A record does not work as a VPC-Link integration target)
2. **ECS service registry** wiring (attached at service creation)
3. **VPC Link** (`oab-vpc-link-<vpc-id>`, shared per-VPC), waits until `AVAILABLE`
4. **API Gateway HTTP API** (`oab-webhook-<ns>-<name>`, one per bot) + `HTTP_PROXY` integration over the VPC Link
5. One **route** per path + a `prod` auto-deploy **stage**
6. A self-referencing **security-group** inbound rule on `containerPort`

`apply` then prints the stable webhook URL(s) to register with BotFather / the
LINE console:

```
🔗 Webhook URL(s) for my-bot:
   https://{api-id}.execute-api.{region}.amazonaws.com/prod/webhook/telegram
   https://{api-id}.execute-api.{region}.amazonaws.com/prod/webhook/line
```

> **Security note:** the API Gateway endpoint itself is public and unauthenticated
> at the transport layer (no IAM auth, no API key). OpenAB's webhook handlers add
> their own app-layer verification on top: Telegram validates the
> `X-Telegram-Bot-Api-Secret-Token` header (`TELEGRAM_SECRET_TOKEN`) and the
> request's source IP against Telegram's published webhook subnets; LINE
> verifies an HMAC-SHA256 signature using `LINE_CHANNEL_SECRET`. Set
> `TELEGRAM_SECRET_TOKEN` when registering the webhook with BotFather to enable
> that check.
>
> **Adding/fixing service discovery never requires recreating the service:**
> if an existing ECS service has no Cloud Map registry, or has one pointing at a
> different Cloud Map service than the one currently resolved for
> `ingress.cloudMapNamespace` (e.g. the namespace was changed after the service
> was created), `apply`'s `update_service` call attaches or replaces the
> registry directly — ECS has supported adding/updating/removing
> `serviceRegistries` on an existing service via a normal rolling replacement
> (new tasks start with the new registry, old tasks stop once healthy — no
> downtime gap) since March 2022. This requires the `AWSServiceRoleForECS`
> service-linked role, which ECS creates automatically the first time any
> service in the account uses service discovery — no setup needed.
>
> **Shared per-VPC (not per-account):** all ingress-enabled bots in the *same VPC*
> share one VPC Link (`oab-vpc-link-<vpc-id>`) and one Cloud Map namespace
> (`<cloudMapNamespace>-<vpc-id>`) — both are named by VPC ID so bots in different
> VPCs never collide or reuse each other's link/namespace. A VPC Link's
> subnets/security groups are fixed at creation and cannot be changed, so every
> ingress bot in a given VPC must use the same `networking.subnets` /
> `securityGroups` as whichever bot created that VPC's link first. `apply`
> verifies the reused link's actual security groups match the manifest and warns
> loudly on a mismatch (subnets aren't exposed by the API, so those can only be
> reminded, not verified).
>
> **Teardown:** `oabctl delete oabservice <name>` permanently removes the bot's
> per-bot ingress resources — its exact Cloud Map service (resolved by the ECS
> service's own registry ARN, not a name search, so same-named bots in different
> VPCs/environments can't collide) and its HTTP API (`oab-webhook-<ns>-<name>`,
> including the API resource itself this time, since the bot is gone for good) —
> on a best-effort basis (it never blocks service deletion). If you instead edit
> a manifest to remove `spec.ingress` while keeping the bot, `apply` runs the same
> Cloud Map + routes/integration/stage cleanup automatically, but **keeps the HTTP
> API** in case ingress is re-added later. The **shared** VPC Link and the
> security-group inbound rule are always left in place for other bots. If the
> Cloud Map service still has registered instances, teardown retries for ~25s
> before falling back to a warning with the manual cleanup command.
>
> **Changing `paths`:** `apply` prunes routes on the bot's API that are no longer
> in the manifest's `ingress.paths`, so renaming or removing a webhook path never
> leaves a dangling route.
>
> **Per-bot API, no path collisions:** each ingress bot gets its own HTTP API, so
> two bots can both use `/webhook/telegram` without clashing — each has a distinct
> `{api-id}` endpoint URL that stays stable across recreates.

### OABFleet — batch deploy

```yaml
apiVersion: oab.dev/v2
kind: OABFleet
metadata:
  name: my-team
  namespace: prod
spec:
  template:
    image: public.ecr.aws/oablab/kiro:stable
    resources: { cpu: "256", memory: "512" }
    runtime:
      type: ecs
      capacityProvider: FARGATE_SPOT
      networking: { subnets: [...], securityGroups: [...] }
  agents:
    - name: bot-a
      configFrom: s3://.../bot-a/config.toml
    - name: bot-b
      configFrom: s3://.../bot-b/config.toml
      resources: { cpu: "1024", memory: "2048" }  # override
```

**Fleet features:**
- Template inheritance with per-agent overrides (`image`, `resources`, `bootstrapFrom`, `secrets`, `ingress`)
- `${name}` interpolation in `configFrom`, `bootstrapFrom`
- Runtime shared across fleet (not overridable per-agent)
- Validate-all-before-apply (no partial deploys)

### Design Principles

- **Manifest = infra desired state** — image, CPU, networking
- **Agent config is external** — `configFrom` points to config.toml (managed via `--sync`)
- **Secrets resolved by OpenAB** — `[secrets.refs]` in config.toml, not in manifest
- **Runtime-agnostic spec** — same top-level fields regardless of ECS or K8S

## Bootstrap

One-time infrastructure setup — similar to `cdk bootstrap`:

```bash
oabctl bootstrap                          # create all (with plan + Y/n)
oabctl bootstrap --status                 # show current state
oabctl bootstrap --delete                 # teardown (only managed resources)
oabctl bootstrap --cluster my-cluster     # import existing resources
```

### Resources Created

| Resource | Name | Purpose |
|----------|------|---------|
| ECS Cluster | `oab` | FARGATE + FARGATE_SPOT |
| IAM Role | `oab-task-execution` | Pull images from ECR |
| IAM Role | `oab-task-role` | ECS Exec + S3 artifacts + Secrets Manager |
| S3 Bucket | `oab-control-plane-{account}` | State + artifacts |
| Security Group | `oab-agents` | Outbound-only |
| Log Group | `/oab/agents` | CloudWatch logs |

### IAM Task Role Permissions

Attached to `oab-task-role` — the identity the *running container* assumes.

| Policy | Permissions | Resource |
|--------|------------|----------|
| `oab-ecs-exec` | ssmmessages:* | * (ECS Exec requirement) |
| `oab-s3-artifacts` | s3:GetObject, s3:PutObject | `{bucket}/artifacts/*` |
| `oab-secrets` | secretsmanager:GetSecretValue | `arn:aws:secretsmanager:*:*:secret:oab/*` |

### IAM Execution Role Permissions

Attached to `oab-task-execution` — the identity **ECS itself** assumes to pull
the image and resolve `spec.secrets` values *before* the container starts.
This is a different role from the task role above; a manifest's `spec.secrets`
values are fetched by this role, not by the running container.

| Policy | Permissions | Resource |
|--------|------------|----------|
| `AmazonECSTaskExecutionRolePolicy` (AWS managed) | ECR image pulls, CloudWatch log delivery | * |
| `oab-secrets` | secretsmanager:GetSecretValue | `arn:aws:secretsmanager:*:*:secret:oab/*` |

### Import Existing Resources

```bash
oabctl bootstrap \
  --cluster my-existing-cluster \
  --vpc vpc-12345 \
  --subnets subnet-a,subnet-b \
  --security-group sg-existing \
  --execution-role arn:aws:iam::123:role/my-role \
  --task-role arn:aws:iam::123:role/my-task-role
```

Imported resources are tracked but not deleted on `bootstrap --delete`.

## State Store

```
s3://oab-control-plane-{account}/
├── bootstrap/state.json              ← infra state (resource ARNs, managed flags)
├── manifests/{namespace}/{name}.yaml ← desired state (generation tracked)
└── artifacts/{namespace}/{name}/     ← agent configs, accessible by task role
    └── config.toml
```

## Commands

| Command | Description |
|---------|-------------|
| `oabctl bootstrap` | One-time infra setup (plan + confirm) |
| `oabctl bootstrap --delete` | Teardown managed resources |
| `oabctl bootstrap --status` | Show bootstrap state |
| `oabctl create <name>` | Interactive wizard → generate config + manifest |
| `oabctl create <name> --auto-apply` | Generate + deploy immediately |
| `oabctl apply -f <file\|dir>` | Sync config + deploy (default) |
| `oabctl apply -f <file> --no-sync` | Deploy without syncing config |
| `oabctl get oabservice [name]` | List agents and status |
| `oabctl delete oabservice <name>` | Teardown agent |
| `oabctl exec <agent> -- <cmd>` | Execute command in container |
| `oabctl cp <src> <dst>` | Copy files to/from container |
| `oabctl sync <src> <dst>` | Sync directories (bidirectional) |

## JSON Schema

[`schema/oabservice-v2.json`](schema/oabservice-v2.json) — supports both OABService and OABFleet for IDE validation.

## Prerequisites

With `oabctl bootstrap`, most prerequisites are handled automatically. You only need:

1. **AWS credentials** — IAM user/role with permissions to create the above resources
2. **Docker** — to build custom images (optional if using official images)

### Additional permissions for `spec.ingress`

The resources bootstrap creates cover outbound-only (Discord) deployments. If
any manifest sets `spec.ingress`, the **caller of `oabctl apply`/`delete`**
(not the task role) also needs:

| Service | Actions |
|---------|---------|
| Cloud Map | `servicediscovery:CreatePrivateDnsNamespace`, `CreateService`, `DeleteService`, `ListNamespaces`, `ListServices`, `GetOperation` |
| API Gateway | `apigateway:CreateVpcLink`, `CreateApi`, `CreateIntegration`, `CreateRoute`, `CreateStage`, `DeleteRoute`, `DeleteIntegration`, `DeleteStage`, `DeleteApi`, `GetVpcLinks`, `GetVpcLink`, `GetApis`, `GetIntegrations`, `GetRoutes`, `GetStages` |
| EC2 | `ec2:DescribeSubnets`, `AuthorizeSecurityGroupIngress` |
| ECS | `ecs:UpdateService` with `serviceRegistries` (requires the `AWSServiceRoleForECS` service-linked role, which ECS creates automatically the first time any service in the account uses service discovery) |

`AdministratorAccess`-equivalent or a broad `servicediscovery:*`/`apigateway:*`
during development is fine; the table above is for scoping a least-privilege
policy.

