# ADR: Secrets Management

- **Status:** Proposed
- **Date:** 2026-06-08
- **Author:** @chaodu-agent

---

## 1. Problem Statement

OpenAB's `config.toml` and agent environment frequently contain credentials (API keys, bot tokens, OAuth secrets). Today these are either:

1. Hardcoded in `config.toml` (insecure, leaks into git history)
2. Injected via environment variables (visible in process listings, container inspect, logs)

Both approaches lack centralized rotation, encryption at rest, and audit trails.

**Goal:** Allow operators to store secrets in an external secrets manager and reference them in `config.toml`. OpenAB resolves references at boot time, holds values in memory only, and never writes them to disk.

**Benefits:**
- Environment variables contain zero credentials
- Secrets exist only in OAB process memory at runtime
- No secrets written to local disk
- External lifecycle management (rotation, encryption, access control, audit)

---

## 2. Approaches Considered

### A. Environment Variables Only (Status Quo)

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
```

**Pros:**
- Simple, well-understood
- Works everywhere

**Cons:**
- Visible in `/proc/<pid>/environ`, `docker inspect`, CI logs
- No rotation without pod restart
- No audit trail
- No encryption at rest (depends on orchestrator)

### B. Native SDK Integration (AWS Secrets Manager)

Build the AWS SDK into the openab binary and resolve secret references directly:

```toml
[secrets.refs]
discord_token = "aws-sm://openab/prod#discord_bot_token"
```

**Pros:**
- Zero external dependencies at runtime
- Fast, reliable (direct API call)
- IAM-based auth (no extra credentials needed on AWS)

**Cons:**
- Increases binary size (~3–5 MB for AWS SDK)
- AWS-specific (vendor lock-in if it's the only option)

### C. Exec Provider (External Script)

Delegate secret fetching to an external script/binary:

```toml
[secrets.refs]
vault_token = "exec://scripts/get-secret.sh vault/openab token"
```

**Pros:**
- Zero SDK dependencies — binary size unchanged
- Supports any provider (Vault, GCP, Azure, 1Password, sops, etc.)
- Users bring their own tooling

**Cons:**
- Requires external binary/script to be present at boot time
- Depends on `[hooks.pre_boot]` to provision scripts (ordering constraint)
- Slightly slower (process spawn + CLI overhead)
- Harder to validate errors (script output parsing)

### D. Feature-Gated Multi-Provider SDKs

Build each provider as a Cargo feature flag:

```toml
[features]
default = ["secrets-aws"]
secrets-vault = ["vaultrs"]
secrets-gcp = ["google-cloud-secretmanager"]
```

**Pros:**
- Users opt-in to only what they need
- Official image stays lean (AWS only)

**Cons:**
- Users wanting Vault/GCP must build custom images
- More CI matrix complexity

---

## 3. Decision

**Combine approaches B + C:**

| Provider | Mechanism | Included in default build |
|----------|-----------|--------------------------|
| AWS Secrets Manager | Native SDK | ✅ Yes (default feature) |
| Exec (any provider) | External script | ✅ Yes (no dependency) |
| HashiCorp Vault | Cargo feature flag | ❌ Opt-in |
| GCP Secret Manager | Cargo feature flag | ❌ Opt-in |

**Rationale:**
- AWS Secrets Manager covers the majority of OAB deployments (EKS, ECS, EC2)
- `exec://` provides a universal escape hatch for any other provider with zero binary cost
- Runtime memory impact of unused SDK code is negligible (no allocations until called)
- Optional feature flags allow power users to add native Vault/GCP support in custom images

---

## 4. Specification

### Config Syntax

Secret references use URI-style strings under `[secrets.refs]` in `config.toml`:

```toml
[secrets.refs]
# AWS Secrets Manager: aws-sm://<secret-name>#<json-key>
discord_token = "aws-sm://openab/prod#discord_bot_token"
openai_key    = "aws-sm://openab/prod#openai_api_key"

# Exec provider: exec://<script-path> <key> <attribute>
vault_token   = "exec:///home/agent/.local/bin/get-secret.sh vault/openab token"
custom_key    = "exec:///home/agent/.local/bin/get-secret.sh myservice api_key"
```

### Reference Format

**AWS Secrets Manager:**
```
aws-sm://<secret-id>#<json-key>
```
- `<secret-id>` — ARN or friendly name of the secret
- `<json-key>` — key within the JSON value stored in the secret

**Exec provider:**
```
exec://<script-path> <key> <attribute>
```
- `<script-path>` — absolute path to executable (must not contain spaces)
- `<key>` — first argument: which secret to fetch
- `<attribute>` — second argument: which field/attribute within that secret
- Script must output the secret value to stdout (single line, no trailing newline)
- Non-zero exit code = failure

### Resolution Lifecycle

```
┌─────────────────────────────────────────────────────┐
│ openab boot sequence                                │
│                                                     │
│  1. Parse config.toml                               │
│  2. Execute [hooks.pre_boot]     ← scripts land here│
│  3. Resolve [secrets.refs] references ← THIS FEATURE  │
│  4. Spawn agent sessions                            │
└─────────────────────────────────────────────────────┘
```

**Critical ordering:** Secrets resolution runs AFTER `pre_boot` hooks. This ensures `exec://` scripts provisioned by hooks are available.

### Resolution Semantics

1. openab reads `[secrets.refs]` table entries (each value is a URI: `aws-sm://` or `exec://`)
2. For each reference, calls the appropriate provider
3. Resolved values are stored in an in-memory `HashMap<String, String>`
4. `${secrets.<key>}` placeholders elsewhere in config are replaced with resolved values (TOML-escaped)
5. Config is re-parsed with substituted values
6. On resolution failure: log error and exit non-zero (fail-closed)

### AWS Secrets Manager Provider

- Uses the default AWS credential chain (env vars → IMDS → EKS IRSA → ECS task role)
- Region from `AWS_REGION` / `AWS_DEFAULT_REGION` or instance metadata
- Optional config:

```toml
[secrets.aws]
region = "us-west-2"          # override region
endpoint_url = "http://..."   # LocalStack / VPC endpoint
```

### Exec Provider

- Script must exist and be executable at resolution time
- Timeout: 10 seconds per invocation (configurable)
- Environment: inherits the same sanitized env as `[hooks.pre_boot]`
- stderr is captured and logged on failure
- Script is invoked once per secret reference (no batching)

```toml
[secrets.exec]
timeout_seconds = 10   # per-invocation timeout (default: 10)
```

### Error Handling

| Scenario | Behavior |
|----------|----------|
| AWS API error (AccessDenied, not found) | Log error, exit 1 |
| Exec script not found | Log error with hint about pre_boot hooks, exit 1 |
| Exec script timeout | Kill process, log error, exit 1 |
| Exec script non-zero exit | Log stderr, exit 1 |
| JSON key not found in AWS secret | Log error, exit 1 |
| Network timeout (AWS) | Retry 2x with backoff, then exit 1 |

All failures are **fail-closed** — openab will not start with unresolved secrets.

### Security Considerations

- Resolved secrets are never logged (even at debug level)
- Secrets are never written to disk
- Secrets are not exposed to `[hooks.pre_boot]` scripts (resolved after hooks)
- `exec://` scripts run with the same UID as openab (not root)
- AWS SDK uses IAM — no additional credentials needed in config

---

## 5. Helm Chart Integration

```yaml
# values.yaml
secrets:
  enabled: true
  aws:
    region: ""              # defaults to pod's region
    endpointUrl: ""         # optional VPC endpoint
  exec:
    timeoutSeconds: 10
  refs:
    discord_token: "aws-sm://openab/prod#discord_bot_token"
    openai_key: "aws-sm://openab/prod#openai_api_key"
```

The chart renders these into the `[secrets.refs]` section of the generated `config.toml`.

For AWS, the chart should include a `ServiceAccount` annotation for IRSA:

```yaml
serviceAccount:
  annotations:
    eks.amazonaws.com/role-arn: arn:aws:iam::123456789012:role/openab-secrets-reader
```

---

## 6. Examples

### Minimal: AWS Secrets Manager on EKS

```toml
[secrets.refs]
discord_token = "aws-sm://openab/prod#discord_bot_token"
openai_key    = "aws-sm://openab/prod#openai_api_key"

[discord]
bot_token = "${secrets.discord_token}"
```

IAM policy on the pod's service account:

```json
{
  "Effect": "Allow",
  "Action": ["secretsmanager:GetSecretValue"],
  "Resource": "arn:aws:secretsmanager:*:*:secret:openab/*"
}
```

### Medium: Exec with HashiCorp Vault CLI

Pre-boot hook downloads the script:

```toml
[hooks.pre_boot]
inline = '''
#!/bin/sh
vault login -method=kubernetes role=openab > /dev/null
'''

[secrets.refs]
db_password = "exec:///home/agent/.local/bin/vault-get.sh secret/openab db_password"
```

Where `vault-get.sh`:
```bash
#!/bin/sh
# Usage: vault-get.sh <path> <key>
vault kv get -field="$2" "$1"
```

### Advanced: Mixed providers

```toml
[hooks.pre_boot]
script = "/etc/openab/pre-boot.sh"

[secrets.refs]
# AWS-managed secrets
discord_token = "aws-sm://openab/prod#discord_bot_token"
# Vault-managed secrets via exec
github_pat    = "exec:///home/agent/.local/bin/get-secret.sh vault/openab github_pat"

[secrets.aws]
region = "ap-northeast-1"

[secrets.exec]
timeout_seconds = 15
```
