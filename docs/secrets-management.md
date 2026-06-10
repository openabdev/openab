# Secrets Management

OpenAB supports resolving secrets from external providers at boot time. Credentials are held in memory only — never written to disk, never exposed in environment variables or logs.

## Quick Start

```toml
[secrets.refs]
discord_token = "aws-sm://openab/prod#discord_bot_token"

[discord]
bot_token = "${secrets.discord_token}"
```

That's it. OpenAB fetches the secret from AWS Secrets Manager on startup and injects it into the config.

## Providers

### AWS Secrets Manager (`aws-sm://`)

```
aws-sm://<secret-id>#<json-key>
```

- `<secret-id>` — Secret name or full ARN
- `<json-key>` — Key within the JSON-structured secret value

**Authentication:** Uses the default AWS credential chain (env vars → IMDS → IRSA → ECS task role). No extra credentials needed.

**Example** — one secret storing multiple keys:

```json
{
  "discord_bot_token": "MTQ5...",
  "openai_api_key": "sk-...",
  "github_pat": "ghp_..."
}
```

```toml
[secrets.refs]
discord_token = "aws-sm://openab/prod#discord_bot_token"
openai_key    = "aws-sm://openab/prod#openai_api_key"
github_pat    = "aws-sm://openab/prod#github_pat"
```

**Optional configuration:**

```toml
[secrets.aws]
region = "ap-northeast-1"          # override region
endpoint_url = "http://localhost:4566"  # LocalStack
```

### Exec Provider (`exec://`)

```
exec://<script-path> <key> <attribute>
```

- `<script-path>` — Absolute path to executable (must not contain spaces)
- `<key>` — First argument: which secret to fetch
- `<attribute>` — Second argument: which field within that secret

The script must output the secret value to stdout (single line). Non-zero exit = failure.

**Example:**

```toml
[secrets.refs]
vault_token = "exec:///home/agent/.local/bin/get-secret.sh vault/openab token"

[secrets.exec]
timeout_seconds = 15
```

Where `get-secret.sh`:
```bash
#!/bin/sh
# Usage: get-secret.sh <path> <key>
vault kv get -field="$2" "$1"
```

**Security:** Exec scripts run with a sanitized environment (same as `[hooks.pre_boot]`). Only `HOME`, `PATH`, `USER`, and cloud credential vars (`AWS_*`, `GOOGLE_*`, `AZURE_*`) are passed through.

## Boot Sequence

```
1. Parse config.toml (env vars expanded)
2. Run [hooks.pre_boot]          ← scripts provisioned here
3. Resolve [secrets.refs]        ← fetch from AWS SM / exec
4. Substitute ${secrets.*}       ← inject into config
5. Re-parse config               ← final config with real values
6. Start agent sessions
```

**Critical:** Secrets resolution runs AFTER `pre_boot` hooks. If your `exec://` scripts are downloaded by a hook, they will be available.

## Error Handling

OpenAB is **fail-closed** — if any secret cannot be resolved, the process exits with a non-zero code. This prevents starting with missing credentials.

| Scenario | Behavior |
|----------|----------|
| AWS API error | Log error, exit 1 |
| Exec script not found | Log hint about pre_boot hooks, exit 1 |
| Exec script timeout | Kill process, log error, exit 1 |
| JSON key not found | Log error, exit 1 |

## Cost

AWS Secrets Manager: **$0.40/secret/month** + $0.05 per 10,000 API calls. Store all your credentials in one JSON secret to minimize cost.

## EKS / IRSA Setup

1. Create an IAM policy:

```json
{
  "Effect": "Allow",
  "Action": ["secretsmanager:GetSecretValue"],
  "Resource": "arn:aws:secretsmanager:*:*:secret:openab/*"
}
```

2. Attach to a ServiceAccount via IRSA:

```yaml
serviceAccount:
  annotations:
    eks.amazonaws.com/role-arn: arn:aws:iam::123456789012:role/openab-secrets-reader
```

3. Reference in config:

```toml
[secrets.refs]
discord_token = "aws-sm://openab/prod#discord_bot_token"
```

No additional credentials needed — the pod's IRSA role handles authentication.

## Feature Flags

| Provider | Cargo Feature | Default |
|----------|--------------|---------|
| AWS Secrets Manager | `secrets-aws` | ✅ Yes |
| Exec (any provider) | always built | ✅ Yes |
| HashiCorp Vault | `secrets-vault` | ❌ Opt-in |
| GCP Secret Manager | `secrets-gcp` | ❌ Opt-in |

To build with additional providers:

```dockerfile
ARG FEATURES="default,secrets-vault"
RUN cargo build --release --features "${FEATURES}"
```
