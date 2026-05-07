# Reference Architecture: OpenAB on AWS ECS Fargate Spot

> **This doc is meant to be used with Kiro or any coding CLI.** Prompt your AI agent with something like:
>
> ```
> per https://github.com/openabdev/openab/blob/main/docs/refarch/aws-ecs-fargate-spot.md deploy an openab on ECS Fargate Spot for me in my AWS account
> ```
>
> and it will guide you through (or handle) the full setup on AWS.

Deploy a single OpenAB bot on ECS Fargate Spot for ~$2.7/month with persistent auth via S3.

## Architecture

```
+-- AWS -------------------------------------------------------+
|                                                              |
|  +-- ECS Fargate Spot Task --------------------------------+ |
|  |                                                         | |
|  |  +-----------+  +----------------+  +------------+      | |
|  |  |s3-restore |  |    openab      |  |  s3-sync   |      | |
|  |  |(init)     |->|(main container)|  | (sidecar)  |      | |
|  |  |pull auth  |  | kiro-cli acp   |  | push auth  |      | |
|  |  |from S3    |  | Discord bot    |  | every 5min |      | |
|  |  +-----------+  +----------------+  +------------+      | |
|  |       |              |      |            |              | |
|  |       +--------------+- /data volume ----+              | |
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

- AWS CLI configured with permissions for ECS, IAM, S3, Secrets Manager, CloudWatch Logs, EC2
- A Discord bot token (from Discord Developer Portal)
- Kiro CLI subscription (for OAuth login)

## Deployment Steps

### Phase 1: Store the Discord bot token

Create a Secrets Manager secret with key `DISCORD_BOT_TOKEN`:

```bash
aws secretsmanager create-secret --name openab \
  --secret-string '{"DISCORD_BOT_TOKEN":"YOUR_BOT_TOKEN_HERE"}' \
  --region us-east-1
```

Note the secret ARN for later.

### Phase 2: Create IAM roles

Create two roles for ECS tasks:

1. **Execution role** (`openab-ecs-execution-role`):
   - Trust: `ecs-tasks.amazonaws.com`
   - Attach: `AmazonECSTaskExecutionRolePolicy`
   - Inline policy: `secretsmanager:GetSecretValue` on the secret ARN

2. **Task role** (`openab-ecs-task-role`):
   - Trust: `ecs-tasks.amazonaws.com`
   - Inline policies:
     - S3: `s3:GetObject`, `s3:PutObject`, `s3:ListBucket`, `s3:DeleteObject` on the state bucket
     - SSM (for ECS Exec): `ssmmessages:CreateControlChannel`, `CreateDataChannel`, `OpenControlChannel`, `OpenDataChannel`

### Phase 3: Create infrastructure

1. **S3 bucket** for auth state persistence (e.g. `openab-state-<account-id>`)
2. **CloudWatch log group** `/ecs/openab`
3. **ECS cluster** named `openab` with capacity providers `FARGATE_SPOT` + `FARGATE`
4. **Security group** — egress-only (no inbound rules needed)

### Phase 4: Create the config.toml

Host `config.toml` as a GitHub Gist (recommended) or any HTTPS URL. OpenAB fetches it at startup via `openab run -c <URL>`.

Create a **secret gist** (or public if you prefer) with your config:

```bash
gh gist create --filename config.toml --desc "OpenAB ECS config" - <<'EOF'
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
allow_all_channels = true
allow_all_users = true
allow_bot_messages = "mentions"
allow_user_messages = "multibot-mentions"
max_bot_turns = 1000
message_processing_mode = "per-thread"

[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"

[pool]
max_sessions = 3
session_ttl_hours = 1

[reactions]
enabled = true
remove_after_reply = false
EOF
```

Use the raw gist URL (e.g. `https://gist.githubusercontent.com/<user>/<id>/raw/<sha>/config.toml`) in Phase 5.

### Phase 5: Register task definition and create service

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

### Phase 6: Authenticate Kiro CLI (one-time)

After the task starts, exec in and login:

```bash
TASK_ID=$(aws ecs list-tasks --cluster openab --service-name openab-kiro \
  --desired-status RUNNING --query 'taskArns[0]' --output text | awk -F/ '{print $NF}')

aws ecs execute-command --cluster openab --task $TASK_ID \
  --container openab --interactive \
  --command "kiro-cli login --use-device-flow"
```

Then copy auth to the shared volume for S3 persistence:

```bash
aws ecs execute-command --cluster openab --task $TASK_ID \
  --container openab --interactive \
  --command "cp /home/agent/.local/share/kiro-cli/data.sqlite3 /data/data.sqlite3"
```

The sidecar syncs to S3 within 5 minutes. Future task restarts auto-restore auth.

### Phase 7: Verify

Mention `@YourBot` in a Discord channel. Check logs:

```bash
aws logs tail /ecs/openab --follow --region us-east-1
```

Look for: `discord bot connected` → `spawning agent` → streaming response.

## Important Notes

- **Spot interruption**: Task may be reclaimed with 2-min notice. Auth persists via S3; bot reconnects automatically on new task launch.
- **Auth file ownership**: The S3 restore step must `chown 1000:1000` the auth file — ECS Exec runs as root but kiro-cli runs as uid 1000 (`agent`).
- **Config via URL**: `openab run -c <URL>` fetches config over HTTPS. Use `${ENV_VAR}` for secrets — expanded at runtime from container environment.
- **No NAT needed**: Public subnet + `assignPublicIp: ENABLED` gives direct internet access.
- **Memory**: 512MB is tight (~370MB idle). Bump to 1024MB if sessions OOM.
