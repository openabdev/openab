# Reference Architecture: OpenAB on AWS ECS Fargate Spot

> **This doc is meant to be used with Kiro or any coding CLI.** Prompt your AI agent with something like:
>
> ```
> per https://github.com/openabdev/openab/blob/main/docs/refarch/aws-ecs-fargate-spot.md deploy an openab on ECS Fargate Spot for me in my AWS account
> ```
>
> and it will guide you through (or handle) the full setup on AWS.

Deploy a single OpenAB bot on ECS Fargate Spot for ~$2.7/month with persistent auth via S3.

## Why This Architecture

- **Fully managed** — Amazon ECS with Fargate eliminates cluster management. No EC2 instances, no patching, no capacity planning.
- **Enterprise-grade isolation** — Fargate runs each task in a dedicated microVM powered by [Firecracker](https://firecracker-microvm.github.io/). Hardware-level isolation between tenants — critical for enterprise workloads.
- **Cost effective** — `FARGATE_SPOT` provides up to 70% savings over on-demand. Ideal for non-critical, interruptible workloads like AI agent bots (~$2.7/month for 0.25 vCPU).
- **Secrets & IAM native** — Bot tokens and API keys stored in AWS Secrets Manager, injected at runtime. Fine-grained IAM roles per task — no shared credentials.
- **Zero networking overhead** — No NAT gateway, no load balancer needed. Public IP + egress-only security group keeps it simple and cheap.
- **Resilient** — Spot interruptions auto-recover. Auth persists to S3 via hooks. ECS restarts tasks automatically.

## Architecture

```
+-- AWS -------------------------------------------------------+
|                                                              |
|  +-- ECS Fargate Spot Task --------------------------------+ |
|  |                                                         | |
|  |  +----------------+                                     | |
|  |  |    openab      |                                     | |
|  |  |(main container)|                                     | |
|  |  | kiro-cli acp   |                                     | |
|  |  | Discord bot    |                                     | |
|  |  +----------------+                                     | |
|  |         |                                               | |
|  +---------------------------------------------------------+ |
|                                                              |
|      |                     |                                 |
|  S3 Bucket           Secrets Manager                         |
|  (auth state)         (bot token)                            |
|                                                              |
+--------------------------------------------------------------+
                            |               |
                       Discord API   +-- GitHub ------+
                      (bot gateway)  | Gist           |
                                     | (config.toml)  |
                                     +----------------+
```

## Cost

| Resource | Spec | Spot Price/mo |
|----------|------|---------------|
| Fargate Task | 0.25 vCPU + 512MB | ~$2.7 |
| S3 | < 1MB state | ~$0 |
| Secrets Manager | 1 secret | $0.40 |
| CloudWatch Logs | minimal | ~$0 |
| **Total** | | **~$3.1/month** |

## Prerequisites

1. **AWS credentials** — IAM user or IAM Identity Center (`aws sso login`)
2. **[ecsctl](https://github.com/oablab/ecsctl)** installed
3. **Discord bot token** — from [Discord Developer Portal](https://discord.com/developers/applications)
4. **Kiro CLI subscription** — for the agent backend

### Install ecsctl

[ecsctl](https://github.com/oablab/ecsctl) is an open-source CLI from the OpenAB Lab project that gives you a `kubectl`-like experience on Amazon ECS — deploy, exec, copy files, tail logs, and manage services declaratively.

```bash
# macOS (Apple Silicon)
curl -sL https://github.com/oablab/ecsctl/releases/latest/download/ecsctl-darwin-arm64.tar.gz | tar xz -O > ~/.local/bin/ecsctl && chmod +x ~/.local/bin/ecsctl

# Linux (x86_64)
curl -sL https://github.com/oablab/ecsctl/releases/latest/download/ecsctl-linux-amd64.tar.gz | tar xz -O > ~/.local/bin/ecsctl && chmod +x ~/.local/bin/ecsctl

# Linux (ARM64)
curl -sL https://github.com/oablab/ecsctl/releases/latest/download/ecsctl-linux-arm64.tar.gz | tar xz -O > ~/.local/bin/ecsctl && chmod +x ~/.local/bin/ecsctl
```

## One-Time Setup

Before your first deploy, create these AWS resources (once per account):

### 1. ECS Cluster

```bash
aws ecs create-cluster --cluster-name openab \
  --capacity-providers FARGATE_SPOT FARGATE \
  --region us-east-1
```

### 2. IAM Roles

**Execution role** (`openab-ecs-execution-role`) — ECS uses this to pull images and inject secrets:
- Trust: `ecs-tasks.amazonaws.com`
- Attach: `AmazonECSTaskExecutionRolePolicy`
- Inline: `secretsmanager:GetSecretValue` on your secret ARN

**Task role** (`openab-ecs-task-role`) — the running container uses this:
- Trust: `ecs-tasks.amazonaws.com`
- Inline: S3 access (`s3:GetObject`, `s3:PutObject`, `s3:ListBucket`) for state persistence
- Inline: SSM for ECS Exec (`ssmmessages:CreateControlChannel`, `CreateDataChannel`, `OpenControlChannel`, `OpenDataChannel`)

### 3. Store Discord bot token

```bash
aws secretsmanager create-secret --name openab \
  --secret-string '{"DISCORD_BOT_TOKEN":"YOUR_BOT_TOKEN_HERE"}' \
  --region us-east-1
```

### 4. Supporting infra

- **S3 bucket** for auth state (e.g. `openab-state-<account-id>`)
- **CloudWatch log group** `/ecs/openab`
- **Security group** — egress-only (no inbound needed)

### 5. Config gist

Host your `config.toml` as a GitHub Gist. OpenAB fetches it at startup.

```bash
gh gist create --filename config.toml --desc "OpenAB ECS config" - <<'EOF'
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
allow_all_channels = true
allow_all_users = true
allow_bot_messages = "mentions"
allow_user_messages = "multibot-mentions"
message_processing_mode = "per-thread"

[pool]
max_sessions = 3
session_ttl_hours = 1

[reactions]
enabled = true
remove_after_reply = false
EOF
```

Note the raw gist URL for the next step.

## Deploy

### Create `service.yaml`

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/oablab/ecsctl/master/schemas/service.schema.json
apiVersion: ecsctl/v1
kind: Service
metadata:
  name: openab-mybot
  cluster: openab
spec:
  image: ghcr.io/openabdev/openab:latest
  cpu: "256"
  memory: "512"
  arch: ARM64
  capacity: FARGATE_SPOT
  desiredCount: 1
  execEnabled: true
  containerName: openab
  executionRoleArn: arn:aws:iam::<ACCOUNT_ID>:role/openab-ecs-execution-role
  taskRoleArn: arn:aws:iam::<ACCOUNT_ID>:role/openab-ecs-task-role
  subnets: [subnet-xxx, subnet-yyy]
  securityGroups: [sg-xxx]
  assignPublicIp: true
  logGroup: /ecs/openab
  env:
    OPENAB_AGENT_NAME: mybot
    OPENAB_BACKEND_AGENT: openab
    STATE_BUCKET: openab-state-<ACCOUNT_ID>
  secrets:
    DISCORD_BOT_TOKEN: arn:aws:secretsmanager:us-east-1:<ACCOUNT_ID>:secret:openab-XXXXXX:DISCORD_BOT_TOKEN::
  command:
    - sh
    - -c
    - "exec /usr/local/bin/openab run -c https://gist.githubusercontent.com/<user>/<gist_id>/raw/config.toml"
```

### Apply

```bash
ecsctl apply -f service.yaml --wait
```

### Authenticate (one-time)

After the task starts, exec in and run the auth command:

```bash
ecsctl exec mybot -- sh -c '$OPENAB_AGENT_AUTH_COMMAND'
```

Follow the device code flow in your browser. Auth is persisted via the `[hooks.pre_boot]` S3 sync configured in your gist.

### Verify

```bash
ecsctl log mybot -f
```

Look for: `discord bot connected` → `spawning agent` → streaming response.

Mention `@YourBot` in a Discord channel to confirm it responds.

## Day-2 Operations

```bash
# Update config and redeploy
ecsctl apply -f service.yaml --wait

# Scale up for more capacity
ecsctl apply -f service.yaml --set spec.cpu=512 --set spec.memory=1024 --wait

# Clone to a second bot
ecsctl apply -f service.yaml --set metadata.name=openab-mybot2

# Force restart after auth refresh
ecsctl restart mybot

# Export current running state to YAML
ecsctl export mybot -f service.yaml

# Check task status
ecsctl get mybot

# Copy files to/from container
ecsctl cp ./skills.tar.gz mybot:/tmp/

# Remove the service
ecsctl delete mybot
```

## Important Notes

- **Secrets Management**: Store all credentials (bot tokens, API keys, OAuth tokens) in AWS Secrets Manager. Reference them in the `secrets:` block of your service YAML — they're injected as environment variables at runtime, never baked into images or gists.
- **Spot interruption**: Task may be reclaimed with 2-min notice. Auth persists via S3; bot reconnects automatically on new task launch.
- **Config via URL**: `openab run -c <URL>` fetches config over HTTPS. Use `${ENV_VAR}` for secrets — expanded at runtime from container environment.
- **No NAT needed**: Public subnet + `assignPublicIp: ENABLED` gives direct internet access.
- **Memory**: 512MB is tight (~370MB idle). Bump to 1024MB if sessions OOM.
- **Round-trip workflow**: `ecsctl export` → edit YAML → `ecsctl apply` for iterative changes.

## Advanced: Bootstrap & Backup with Hooks

OpenAB supports `[hooks.pre_boot]` and `[hooks.pre_shutdown]` in the config gist. These replace the need for sidecar containers.

```
┌─────────────────────────────────────────────────────────────┐
│  Task Lifecycle                                             │
│                                                             │
│  ┌──────────────┐    ┌──────────┐    ┌────────────────┐    │
│  │  pre_boot    │───▶│  openab  │───▶│ pre_shutdown   │    │
│  │              │    │  (main)  │    │                │    │
│  │ • S3 → HOME │    │          │    │ • HOME → S3   │    │
│  │ • install    │    │ agent    │    │ • cleanup     │    │
│  │   tools      │    │ sessions │    │               │    │
│  │ • restore    │    │          │    │               │    │
│  │   auth       │    │          │    │               │    │
│  └──────────────┘    └──────────┘    └────────────────┘    │
│                                                             │
│       ▲                                        │           │
│       │            S3 Bucket                   │           │
│       └────────── (state/auth) ◀───────────────┘           │
└─────────────────────────────────────────────────────────────┘
```

**pre_boot** — runs before the agent starts:
- Restore auth tokens and state from S3
- Install extra tools (AWS CLI, gh, custom binaries)
- Sync shared assets (AGENTS.md, skills)

**pre_shutdown** — runs on graceful stop (Spot interruption, scale-down):
- Tar up HOME and push to S3
- Persist auth, conversation history, installed tools

Add hooks to your config gist:

```toml
[hooks.pre_boot]
timeout_seconds = 120
on_failure = "abort"
url = "https://gist.githubusercontent.com/<user>/<id>/raw/pre-boot.sh"

[hooks.pre_shutdown]
timeout_seconds = 120
url = "https://gist.githubusercontent.com/<user>/<id>/raw/pre-shutdown.sh"
```

This eliminates the init/sidecar container pattern — a single container handles everything.

## Manual Deployment (without ecsctl)

<details>
<summary>Click to expand AWS CLI-based deployment</summary>

If you prefer to use the AWS CLI directly instead of ecsctl, replace Phase 5 with a manual task definition registration.

Register a task definition with three containers:

| Container | Image | Role | Essential |
|-----------|-------|------|-----------|
| `s3-restore` | `amazon/aws-cli` | Pull auth from S3 + `chown 1000:1000` | No (init) |
| `openab` | `ghcr.io/openabdev/openab:latest` | Main bot process | Yes |
| `s3-sync` | `amazon/aws-cli` | Push auth to S3 every 5 min | No (sidecar) |

Key settings:
- CPU: 256 (0.25 vCPU), Memory: 512 MB
- Network mode: `awsvpc`, assign public IP
- Capacity provider: `FARGATE_SPOT`
- Enable ECS Exec for interactive login
- `openab` container depends on `s3-restore` (condition: SUCCESS)
- `openab` entrypoint: restore auth from shared volume, then `exec openab run -c <CONFIG_URL>`
- Inject `DISCORD_BOT_TOKEN` from Secrets Manager via container `secrets`
- Shared volume (`agent-data`) mounted at `/data` across all containers

Create an ECS service with `desiredCount: 1`.

For auth, exec in manually:

```bash
TASK_ID=$(aws ecs list-tasks --cluster openab --service-name openab-mybot \
  --desired-status RUNNING --query 'taskArns[0]' --output text | awk -F/ '{print $NF}')

aws ecs execute-command --cluster openab --task $TASK_ID \
  --container openab --interactive \
  --command "kiro-cli login --use-device-flow"
```

</details>
