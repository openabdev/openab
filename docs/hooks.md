# Lifecycle Hooks

OpenAB supports lifecycle hooks that run at specific points during the container lifecycle. All lifecycle phases are configured in `config.toml` under the `[hooks]` table.

## Lifecycle Order

```
hooks.pre_seed → hooks.pre_boot → (agent running) → hooks.pre_shutdown
```

| Phase | When | Purpose | Config | Action Type |
|-------|------|---------|--------|-------------|
| `pre_seed` | First — before `pre_boot` | Download & extract S3 archives to seed the environment | `[hooks.pre_seed]` | Built-in S3 download + extract |
| `pre_boot` | After seed, before agent pool starts | Run custom setup scripts before agent pool creation | `[hooks.pre_boot]` | User script |
| `pre_shutdown` | After pool stops, before exit | Run custom cleanup scripts after pool shutdown | `[hooks.pre_shutdown]` | User script |

## Pre-Seed Phase

The `pre_seed` phase runs **before** `pre_boot`. It downloads archives from S3 and extracts them into the agent's home directory (or a custom target). Supported formats: `.zip`, `.tar.gz`, and `.tgz` (auto-detected via magic bytes). This eliminates the need for users to install AWS CLI and write download scripts in `pre_boot`.

> `pre-seed` is enabled by default. No feature flag needed.

### Configuration

```toml
[hooks.pre_seed]
sources = [
  "s3://my-bucket/base-env.tar.gz",
  "s3://my-bucket/shared-memory.zip",
  "s3://my-bucket/agent-overrides.tgz",
]
# target = "/home/agent"                  # default: $HOME
# max_bytes = 104857600                   # max compressed size per archive (default: 100 MiB)
# timeout_seconds = 300                   # per-source timeout (default: 300)
# on_failure = "abort"                    # "abort" or "warn" (default: "abort")
# region = "us-west-2"                    # optional: override AWS region
# endpoint_url = "http://localhost:4566"  # optional: LocalStack / VPC endpoint
```

### Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `sources` | string[] | `[]` | S3 URIs of archives (`.zip`, `.tar.gz`, `.tgz`). Max 5. Extracted in order. |
| `target` | string | `$HOME` | Extraction target directory. |
| `max_bytes` | u64 | `104857600` | Max compressed archive size in bytes (100 MiB). |
| `timeout_seconds` | u64 | `300` | Per-source download+extract timeout. |
| `on_failure` | string | `"abort"` | `"abort"` exits openab; `"warn"` logs and continues. |
| `region` | string | — | Override AWS region. |
| `endpoint_url` | string | — | Override S3 endpoint URL. |

### Layer Concept

Sources are extracted sequentially (first → last). Files from later archives overwrite earlier ones — like layers in a container image:

```
Layer 3 (last)   ─── highest priority, overwrites all below
Layer 2          ─── overwrites layer 1
Layer 1 (first)  ─── base layer
─────────────────
     $HOME
```

### Safety

- **Integrity verification**: two layers of protection:
  1. **S3-native checksum (automatic)**: if the object was uploaded with `--checksum-algorithm SHA256`, OpenAB automatically verifies it on download — no config needed
  2. **User-provided `sha256s` (optional)**: explicit checksums in config for additional defense-in-depth
- **Size cap**: downloads exceeding `max_bytes` are rejected before extraction
- **Atomic extraction**: archives are first extracted to a temp directory, then moved into target — if extraction fails, target is not corrupted. Note: the move phase is per-file; if it fails mid-way with `on_failure = "warn"`, the target may be partially updated.
- **Path traversal prevention**: zip uses `enclosed_name()`; tarball uses `unpack_in()` which rejects `..` escapes
- **Permission hardening**: suid/sgid/sticky bits are stripped from extracted files

### Constraints

- Maximum **5** sources
- Only `s3://` URIs supported
- Supported formats: `.zip`, `.tar.gz`, `.tgz` (auto-detected via gzip magic bytes)
- Uses the standard AWS credential chain (IRSA, ECS task role, env vars)
- Optional `region`/`endpoint_url` override for LocalStack or VPC endpoints

### IAM Policy

```json
{
  "Effect": "Allow",
  "Action": ["s3:GetObject"],
  "Resource": [
    "arn:aws:s3:::my-bucket/base-env.zip",
    "arn:aws:s3:::my-bucket/shared-memory.zip",
    "arn:aws:s3:::my-bucket/agent-overrides.zip"
  ]
}
```

### Recommended: Enable S3 Checksums on Upload

For automatic integrity verification without maintaining `sha256s` in config, upload zip archives with SHA-256 checksums enabled:

```bash
# Upload with SHA-256 checksum (recommended)
aws s3 cp env.zip s3://my-bucket/env.zip --checksum-algorithm SHA256

# Verify it was stored
aws s3api head-object --bucket my-bucket --key env.zip --checksum-mode ENABLED
```

When objects have S3-native SHA-256 checksums, OpenAB verifies them automatically on download — no `sha256s` config needed. This is the simplest path to integrity verification.

> **Note:** If `sha256s` is also provided in config, both checks run. The S3-native check uses the base64-encoded checksum from the `x-amz-checksum-sha256` response header. If neither is available, download proceeds without integrity verification (relies on IAM + bucket policy for trust).

---

## Available Hooks

The hooks below are **script-based** (`pre_boot`, `pre_shutdown`) and share the `script` / `inline` / `url` configuration described in this section. `pre_seed` is configured separately — see [Pre-Seed Phase](#pre-seed-phase) above.

| Hook | Timing | Use Case |
|------|--------|----------|
| `pre_boot` | Before agent pool creation | Bootstrap files, sync from S3, install CLIs |
| `pre_shutdown` | After pool shutdown, before exit | Backup state, sync to S3 |

## Configuration

Each hook supports exactly **one** script source:

### Option A: File path

```toml
[hooks.pre_boot]
script = "/etc/openab/pre-boot.sh"
timeout_seconds = 60
on_failure = "abort"
```

### Option B: Inline script

```toml
[hooks.pre_boot]
inline = '''
#!/bin/sh
set -e
aws s3 sync "$BOOTSTRAP_URI" "$HOME/"
'''
timeout_seconds = 120
on_failure = "abort"
```

### Option C: Remote URL (with SHA-256 verification)

```toml
[hooks.pre_boot]
url = "https://raw.githubusercontent.com/acme/config/main/pre-boot.sh"
sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
timeout_seconds = 60
on_failure = "abort"
```

## Fields

| Field | Default | Description |
|-------|---------|-------------|
| `script` | — | Absolute path to an executable script |
| `inline` | — | Script content (written to temp file and executed) |
| `url` | — | Remote script URL (max 1 MiB) |
| `sha256` | — | Required with `url` — hex-encoded SHA-256 of the script |
| `timeout_seconds` | `60` | Max wall-clock time before the script is killed |
| `on_failure` | `"abort"` | `"abort"` exits openab; `"warn"` logs and continues |

## Validation Rules

- Exactly one of `script`, `inline`, or `url` must be set
- `url` requires `sha256`
- `script` must be an absolute path

Validation runs at startup — config errors are caught before any side effects.

## Environment

Scripts run with a sanitized environment:

**Always passed:**
- `HOME`, `PATH`, `USER` (unix) / `USERPROFILE`, `USERNAME`, `SystemRoot`, `SystemDrive` (windows)

**Cloud credentials (auto-detected and passed through):**
- `AWS_*`, `AMAZON_*`, `ECS_CONTAINER_METADATA_URI*`
- `GOOGLE_*`, `GCLOUD_*`, `CLOUDSDK_*`
- `AZURE_*`

**Bootstrap variables (passed if set):**
- `BOOTSTRAP_URI`, `BOOTSTRAP_BASE_URI`, `BOOTSTRAP_PERSONAL_URI`
- `STATE_BUCKET`, `TASK_FAMILY`

**OpenAB identity (passed if set):**
- `OPENAB_AGENT_NAME` — the agent's configured name
- `OPENAB_BACKEND_AGENT` — the backend agent type (e.g. `claude`, `codex`)

> **Note:** `DISCORD_BOT_TOKEN` and other openab secrets are NOT exposed to hook scripts.

## Security

- Temp files are created atomically with `0700` permissions (unix)
- Remote scripts require SHA-256 verification — openab refuses to execute on mismatch
- Scripts run as the container's UID (not root, unless the container runs as root)
- Remote script size is capped at 1 MiB

## Examples

### Clone a config/steering repo on startup

`pre_boot` is for **running logic** at boot — cloning a repo, installing a tool, rendering config from env. For pulling plain S3 archives into `$HOME`, prefer [`pre_seed`](#pre-seed-phase) (path-traversal protection, size caps, and checksums built in — no script needed).

```toml
[hooks.pre_boot]
timeout_seconds = 120
on_failure = "abort"
inline = '''
#!/bin/sh
set -e
# Pull steering/config from a private git repo (something pre_seed can't do)
if [ ! -d "$HOME/.config/steering/.git" ]; then
  git clone --depth 1 "$STEERING_REPO_URL" "$HOME/.config/steering"
else
  git -C "$HOME/.config/steering" pull --ff-only
fi
'''
```

> **Auth ordering:** cloning a **private** repo needs credentials. Because `pre_seed` runs *before* `pre_boot`, seed the GitHub auth there — include `~/.config/gh/` (the `gh` CLI OAuth token) or `~/.git-credentials` in one of your `pre_seed` archives. By the time this `pre_boot` script runs, `git`/`gh` is already authenticated and the clone succeeds. (Alternatively, inject a token via `[agent].env` and embed it in the clone URL — but seeding via `pre_seed` keeps the token out of config.)

### Backup state on shutdown (ECS Fargate)

```toml
[hooks.pre_shutdown]
timeout_seconds = 30
on_failure = "warn"
inline = '''
#!/bin/sh
aws s3 sync "$HOME/" "s3://$STATE_BUCKET/$TASK_FAMILY/" \
  --exclude "aws-cli/*" --exclude "bin/*" --quiet
'''
```

### Conditional OAuth token refresh

```toml
[hooks.pre_boot]
script = "/etc/openab/pre-boot.sh"
timeout_seconds = 60
on_failure = "warn"
```

```bash
#!/bin/sh
# /etc/openab/pre-boot.sh
set -e
if [ -f "$HOME/.kiro/auth.json" ]; then
  EXPIRES=$(jq -r '.expires' "$HOME/.kiro/auth.json")
  NOW=$(date +%s)
  if [ "$NOW" -gt "$EXPIRES" ]; then
    kiro-cli auth refresh || true
  fi
fi
```

## Real-World Example: S3 restore + backup round-trip

A common pattern: bots run on stateless compute (ECS Fargate Spot, Kubernetes with `emptyDir`) where the home directory is wiped on every restart. To survive restarts, each bot:

1. **Restores** its home directory from S3 on boot via `pre_seed`
2. **Backs up** its home directory to S3 on shutdown via `pre_shutdown`

The backup key written on shutdown is exactly the key restored on the next boot — a closed loop that gives persistent state without a PVC.

```
                     pre_seed (boot)
   ┌──────────────────────────────────────────────┐
   │   s3://$STATE_BUCKET/$OPENAB_AGENT_NAME-home.tar.gz
   ▼                                                │
 $HOME  ◄── extract ◄── download                    │  upload ──► tar $HOME
   │                                                ▲
   └──────────────────────────────────────────────┘
                  pre_shutdown (shutdown)
```

### 1. Restore on boot (`pre_seed`)

```toml
[hooks.pre_seed]
sources = [
  "s3://${STATE_BUCKET}/${OPENAB_AGENT_NAME}-home.tar.gz",   # layer 1: this agent's saved home
  "s3://${STATE_BUCKET}/shared/default.tar.gz",              # layer 2: shared defaults / steering
]
timeout_seconds = 120
on_failure = "abort"
max_bytes = 629145600   # 600 MiB per archive
```

> **Syntax matters.** In `pre_seed.sources`, use **`${VAR}`** (with braces). OpenAB expands these from its **own process environment at config-load time** — they are *not* shell variables and are *not* expanded at download time. Sources are extracted first → last, so `shared/default.tar.gz` (layer 2) overwrites any same-path files from layer 1. Order them so the layer you want to win comes last.

`${STATE_BUCKET}` and `${OPENAB_AGENT_NAME}` must be present in the OpenAB container's environment — you supply them at deployment time (see step 3).

### 2. Backup on shutdown (`pre_shutdown`)

This inlines a [reference `pre-shutdown.sh`](https://gist.github.com/chaodu-agent/ffc614ce670e79761c6c3c98d5472737) — it tars `$HOME` (skipping caches and toolchains that bloat the archive) and uploads it to the exact key `pre_seed` restores from:

```toml
[hooks.pre_shutdown]
timeout_seconds = 120
on_failure = "warn"
inline = '''
#!/bin/sh
# Tar up $HOME and sync to S3 (preserves permissions, symlinks)
# Env vars: OPENAB_AGENT_NAME, STATE_BUCKET
export PATH="$HOME/bin:$PATH"

tar czf /tmp/home.tar.gz -C "$HOME" \
  --exclude="./.cache" \
  --exclude="./.npm" \
  --exclude="./node_modules" \
  --exclude="./.rustup" \
  --exclude="./.cargo" \
  --exclude="./.local/share/uv" \
  --exclude="./aws-cli" \
  --exclude="./.local/aws-cli" \
  . 2>/dev/null

aws s3 cp /tmp/home.tar.gz "s3://$STATE_BUCKET/$OPENAB_AGENT_NAME-home.tar.gz" --quiet || true
rm -f /tmp/home.tar.gz
'''
```

> **Syntax matters (the other way).** Inside `inline` scripts use **`$VAR`** (no braces). The shell resolves them at runtime when the script runs. If you wrote `${VAR}` here, OpenAB's config loader would expand it at load time instead of leaving it for the shell. The exclude list keeps the archive small enough to stay under `max_bytes`.

> `on_failure = "warn"` is deliberate: a failed backup should log and let the container exit cleanly rather than block shutdown. The matching `pre_seed` uses `"abort"` so a missing/corrupt restore fails loudly at boot.

### 3. Supply `STATE_BUCKET` and `OPENAB_AGENT_NAME` at deployment

These are plain environment variables — pass them however your platform injects env.

**Helm (`--set`):**

```bash
helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.kiro.env.STATE_BUCKET="my-openab-state" \
  --set agents.kiro.env.OPENAB_AGENT_NAME="bot1"
```

**Helm (`values.yaml`):**

```yaml
agents:
  kiro:
    env:
      STATE_BUCKET: my-openab-state
      OPENAB_AGENT_NAME: bot1
```

**ECS task definition (`environment`):**

```json
{
  "containerDefinitions": [
    {
      "name": "openab",
      "image": "ghcr.io/openabdev/openab:latest",
      "environment": [
        { "name": "STATE_BUCKET", "value": "my-openab-state" },
        { "name": "OPENAB_AGENT_NAME", "value": "bot1" }
      ]
    }
  ]
}
```

**[ecsctl](https://github.com/oablab/ecsctl) manifest** (`ecsctl export <service>` renders the live service as YAML):

```yaml
apiVersion: ecsctl/v1
kind: Service
metadata:
  name: openab-bot1
  cluster: openab
spec:
  image: ghcr.io/openabdev/openab:beta-kiro
  cpu: '2048'
  memory: '4096'
  arch: X86_64
  capacity: FARGATE_SPOT
  desiredCount: 1
  execEnabled: true
  env:
    GHPOOL_URL: http://ghpool.openab.local:8080
    OPENAB_BACKEND_AGENT: openab-kiro
    RUST_LOG: openab=debug,openab_core=debug
    OPENAB_AGENT_NAME: bot1
    GITHUB_API_URL: http://ghpool.openab.local:8080
    STATE_BUCKET: my-openab-state
```

The `spec.env` map carries `OPENAB_AGENT_NAME` and `STATE_BUCKET` into the container — `pre_seed` and `pre_shutdown` pick them up automatically. Apply with `ecsctl apply -f service.yaml`.

Run a second bot by deploying another release/task with a different `OPENAB_AGENT_NAME` (e.g. `bot2`, `bot3`) — each gets its own `<name>-home.tar.gz` key while sharing the same `shared/default.tar.gz` base layer and `STATE_BUCKET`.

### 4. IAM policy

The container's role (IRSA on EKS, task role on ECS) needs read for restore and write for backup:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "PreSeedRestore",
      "Effect": "Allow",
      "Action": ["s3:GetObject"],
      "Resource": [
        "arn:aws:s3:::my-openab-state/bot1-home.tar.gz",
        "arn:aws:s3:::my-openab-state/shared/default.tar.gz"
      ]
    },
    {
      "Sid": "PreShutdownBackup",
      "Effect": "Allow",
      "Action": ["s3:PutObject"],
      "Resource": "arn:aws:s3:::my-openab-state/bot1-home.tar.gz"
    }
  ]
}
```

---

## Platform Comparison

| Option | Best for | Requires redeploy? | Network at boot? |
|--------|----------|--------------------|-------------------|
| `script` | k8s (ConfigMap mount), EFS, image bake | Only if image-baked | No |
| `inline` | ECS, Docker Compose, bare metal | Config change only | No |
| `url` + `sha256` | Central script repo, multi-cluster | No (update sha256 to roll) | Yes |
