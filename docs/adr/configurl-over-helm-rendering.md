# ADR: Shift from Helm ConfigMap Rendering to External Config URL

- **Status:** Proposed
- **Date:** 2026-07-01
- **Author:** @chaodu-agent
- **Implementation:** `configToml` is implemented in the companion PR #1276 (open, not yet merged as of this writing). This ADR documents the design rationale that #1276 implements; merge order is #1276 → this ADR → #1277 (legacy path removal).

---

## 1. Problem Statement

OpenAB has historically relied on Helm chart templates to render `config.toml` into a Kubernetes ConfigMap. This approach served early users well — it provided a single `values.yaml` surface for all configuration, bundled security defaults, and automated Secret creation.

However, as OpenAB matured, two key features eliminated the core reasons for ConfigMap rendering:

1. **`configUrl` support** — OpenAB can now fetch its config directly from an external URL (`https://`, `s3://`) at boot time via `openab run -c <url>`.
2. **`aws-sm://` secrets resolution** — Credentials are resolved in-app from AWS Secrets Manager (or exec providers), removing the need for Kubernetes Secrets entirely.

This leaves the Helm chart's ConfigMap rendering as a **maintenance burden with diminishing value**:

- Every new config feature (e.g. `pre_seed`, `ambient`, `trust`, `hooks`) requires a synchronized PR to update `values.yaml`, `templates/configmap.yaml`, and chart tests.
- Users must reason backwards from `values.yaml` through Helm templates to understand their actual config — they never see the final `config.toml` directly.
- The rendering logic contains ~300+ lines of conditionals, enum validations, and platform-specific assembly.

Meanwhile, declarative tooling (`ecsctl`, `oabctl`, Operator CRDs) operates on the final config state directly, making the Helm rendering layer an outlier in the architecture.

**This ADR is not only about reducing chart maintenance.** OpenAB is a multi-platform project — Kubernetes (via Helm/Operator), ECS Fargate (via `ecsctl`), Zeabur, and AgentCore Runtime are all first-class deployment targets. Kubernetes must not be treated as the "default" platform that dictates config architecture for everyone else. `configUrl` is the one config path that works identically across **all** of them, because it lives entirely in the OpenAB process (`openab run -c <url>`), not in any platform-specific rendering layer. That portability — not just chart LOC reduction — is the primary reason `configUrl` is the recommended path.

## 2. Decision

1. **`configUrl` is the primary, platform-agnostic configuration path for all deployment targets** — Kubernetes, ECS Fargate, Zeabur, and AgentCore Runtime alike. It requires no Helm chart, no ConfigMap, no platform-specific rendering — only a URL (`s3://`, `https://`) passed to the process at boot. This is the path every other configuration mode is judged against.
2. **Helm chart retains responsibility only for runtime posture** on Kubernetes — this is where Helm continues to deliver real value that raw config cannot:
   - **Non-root execution** — enforce `runAsUser`/`runAsGroup` so the container never runs as root, reducing blast radius of container escapes.
   - **Read-only root filesystem** — `readOnlyRootFilesystem: true` with `drop: ALL` capabilities ensures the container cannot be tampered with at runtime; only the HOME PVC is writable.
   - **HOME PVC persistence** — dedicated PersistentVolumeClaim mounted at the agent's `$HOME`, providing durable workspace (git repos, session state, caches) that survives pod restarts.
   - Image version pinning and pull policy
   - ServiceAccount assignment (for IRSA)
   - Recreate strategy (RWO PVC constraint)
3. **`configToml` is a secondary, dev-oriented, Kubernetes-only convenience** — not a peer of `configUrl`. Users paste a raw TOML string into `values.yaml`, or load an external `config.toml` file as-is via `helm --set-file agents.<name>.configToml=./config.toml`. Helm mounts the content verbatim into a ConfigMap — no template rendering, no conditionals, no enum validation. This is useful for local iteration with no external dependencies, but it is chart-coupled and does not extend to ECS/Zeabur/AgentCore. It should be presented as "the low-barrier option for people not ready to stand up external config," not as an equally-weighted alternative to `configUrl`.
4. **Deprecate legacy ConfigMap rendering** — existing template logic remains for backward compatibility but is **no longer maintained**. No bug fixes, no new config features, no chart PRs for this path. Users on legacy rendering are encouraged to migrate to `configUrl` (preferred) or `configToml` (Kubernetes-only fallback).
5. **Config lives externally by default** — users maintain `config.toml` in S3 (`s3://`) or HTTPS. This is what makes config **shared and hot-swappable across an entire fleet**: N agents/bots pointed at the same URL pick up a change on their next restart, with no chart, no CI/CD, and no Kubernetes required. `configToml` (inline or `--set-file`) exists only as a fallback for users who haven't set up external storage yet.
6. **Secrets live in AWS Secrets Manager** — referenced via `aws-sm://` in config.toml. No Kubernetes Secret objects required. This also has a second-order effect worth calling out: once credentials are fully out of `config.toml`, the file itself becomes safe to **share or publish**. A `configUrl` pointing at a public/shared config lets any user reproduce someone else's exact agent behavior instantly — the config becomes a shareable artifact, closer to a public gist than a private deployment secret.

## 3. Target Architecture

```
┌─────────────────────────────────────────────────────┐
│ Helm / kubectl / ecsctl / Operator                  │
│ (runtime posture only: image, security, PVC, SA)    │
└──────────────────────┬──────────────────────────────┘
                       │ deploys pod with:
                       │   args: ["openab", "run", "-c", "s3://..."]
                       ▼
┌─────────────────────────────────────────────────────┐
│ OpenAB process                                       │
│  1. Fetch config.toml from s3://bucket/key           │
│  2. Run pre_boot hooks                               │
│  3. Resolve aws-sm:// secrets                        │
│  4. Start agent sessions                             │
└─────────────────────────────────────────────────────┘
```

## 3a. Why `configUrl` Is the Primary Path, Not Just "the production option"

This section elevates `configUrl` beyond a Kubernetes production tier — it is the config path that makes OpenAB deployable identically on any platform, and it unlocks fleet-scale workflows the other modes cannot.

### Platform parity — Kubernetes is not the default

OpenAB currently ships deployment tooling for Kubernetes (Helm/Operator), ECS Fargate (`ecsctl`), Zeabur, and AgentCore Runtime. Only `configUrl` works the same way on all four:

| Platform | How it consumes `configUrl` | Needs Helm/ConfigMap? |
|----------|------------------------------|------------------------|
| Kubernetes | `args: ["openab", "run", "-c", "s3://..."]` in pod spec | No |
| ECS Fargate (`ecsctl`) | Same `-c` flag in task definition container command | No |
| Zeabur | Same `-c` flag in service start command | No |
| AgentCore Runtime | Same `-c` flag passed to the runtime container | No |

Users who have never touched Kubernetes should be able to run OpenAB on ECS, Zeabur, or AgentCore with the exact same config workflow as a Kubernetes user. `configToml` and legacy rendering cannot make this claim — they are Helm chart features and only exist for the Kubernetes path. They should be documented as **Kubernetes-specific conveniences**, not as peers of `configUrl` in a platform-neutral comparison.

### Fleet-scale 1:N shared config with hot-reload-by-restart

Because `configUrl` is just a URL fetched at process boot, an operator running many agents/bots (N) can point **all of them at the same `s3://` or `https://` config**. Updating that one object and restarting the fleet — `kubectl rollout restart`, an ECS service force-deploy, or a Zeabur/AgentCore restart — propagates the change to every instance in seconds, with no chart change, no CI/CD pipeline, and no per-instance edit:

```
                 ┌─────────────────────────────┐
                 │ s3://bucket/shared/config.toml │
                 └──────────────┬──────────────┘
        ┌────────────────┬──────┴──────┬────────────────┐
        ▼                ▼             ▼                ▼
   agent-1 (K8s)   agent-2 (ECS)  agent-3 (Zeabur)  agent-N (AgentCore)
```

This is the same mental model as editing a GitHub gist in place, or `gh gist edit` / `aws s3 cp` followed by a restart — no build step, no deploy pipeline, edit-and-go. For operators iterating quickly across dozens of bots, this is materially faster than a chart-per-agent model.

This pattern extends beyond the top-level config: as `config.toml` becomes more modular (e.g. `pre_boot`, `pre_shutdown` hooks), those sections can themselves point to a shared external endpoint. Changing behavior for an entire fleet of bots becomes "edit one URL, restart" rather than "edit N config files" or "ship N chart upgrades."

### Config as a shareable, credential-free artifact

Because secrets are resolved via `aws-sm://` (or other exec providers) rather than embedded in `config.toml`, the file itself no longer needs to be treated as sensitive. A `configUrl` can be made public and shared directly — anyone pointing their own deployment at that URL reproduces the exact same agent behavior. This turns `config.toml` into a distributable "recipe," not just a private deployment artifact, and is a capability unique to the externalized-config model.

### Not a Kubernetes-only concern

Chart maintenance cost is a real driver for this ADR, but it is a secondary one. The primary driver is that Kubernetes should not be the platform that dictates config architecture for users who are on ECS, Zeabur, or AgentCore and have no reason to adopt Helm. `configUrl` gives every platform — K8s included — the same low-friction, fleet-shareable, hot-restartable config workflow. Kubernetes users additionally get `configToml` (inline or `--set-file`) as a Helm-specific convenience, but that convenience should never be presented as equal in importance to `configUrl`.

## 4. Configuration Modes

| Mode | Source | When to use | Platform scope | Helm interaction |
|------|--------|-------------|-----------------|-------------------|
| **`configUrl`** (primary) | S3, HTTPS, R2 | **Default recommendation for all deployments** — production, fleet-scale, cross-platform, shareable configs | Kubernetes, ECS, Zeabur, AgentCore — identical workflow everywhere | `helm install`/equivalent once; config changes need only a restart, no redeploy |
| **`configToml`** (secondary) | Inline TOML string in values.yaml, or an external `config.toml` loaded as-is via `--set-file` | Local iteration with no external deps yet — a stepping stone, not an end state | Kubernetes (Helm) only | `helm upgrade` picks up value/file changes |
| **Legacy rendering** ⚠️ | `values.yaml` → template → ConfigMap | **Deprecated — not maintained** | Kubernetes (Helm) only | Every config change = chart PR |

### configUrl mode (primary — recommended for all deployments, all platforms)

```yaml
agents:
  kiro:
    configUrl: "s3://my-bucket/openab/kiro/config.toml"
    serviceAccountName: "openab"
```

Pod starts with `openab run -c s3://...` — config fetched at boot. The exact same `-c s3://...` flag is what `ecsctl` puts in an ECS task definition and what a Zeabur or AgentCore service start command uses — no Kubernetes required to get this workflow.

### configToml mode (secondary — Kubernetes-only convenience for local iteration)

`configToml` (proposed and implemented in the companion PR #1276, which this ADR's decision motivates) accepts a raw TOML string and mounts it verbatim into the ConfigMap — no template rendering, no conditionals, no enum validation. It supports two equivalent usage patterns, both backed by the same field:

**Inline** — paste the TOML directly into `values.yaml`:

```yaml
agents:
  kiro:
    configToml: |
      [discord]
      bot_token = "${DISCORD_BOT_TOKEN}"
      allow_all_channels = true
    serviceAccountName: "openab"
```

**As-is from a standalone file** — keep `config.toml` as a real, standalone file (full IDE syntax highlighting and TOML schema validation) and load it verbatim at deploy time with Helm's built-in `--set-file`:

```bash
helm upgrade kiro ./charts/openab \
  --set-file agents.kiro.configToml=./my-config.toml \
  -f values.yaml
```

`--set-file` reads the file's raw content and assigns it to `agents.kiro.configToml` as a string, merging the same way `--set` does. This gives WYSIWYG, standalone-file editing **without any chart changes** — a dedicated `configFile` field (with `.Files.Get`) is not needed, since `--set-file` already covers the "edit a real file, load it as-is" use case using the existing `configToml` field. The tradeoff versus `configUrl` is that this path is still Kubernetes/Helm-only and requires a `helm upgrade` (not just a restart) to pick up changes.

The user maintains a **real `config.toml`** either way — what they write is exactly what the agent reads. No values-to-template translation layer.

### Legacy rendering (⚠️ deprecated — unmaintained)

Existing `values.yaml` → `templates/configmap.yaml` rendering continues to work for backward compatibility but is **no longer maintained**. It will not receive bug fixes, new config features, or support for new platforms.

> **Community notice:** We recommend all users migrate to `configUrl` — the platform-agnostic, fleet-shareable path that works identically on Kubernetes, ECS, Zeabur, and AgentCore. `configToml` (inline or `--set-file`) remains available as a Kubernetes-only convenience for local iteration. The legacy `values.yaml` ConfigMap rendering path will not be updated going forward. All non-legacy paths give you full visibility into your actual config.toml — no more guessing what Helm templates produce.

## 5. Minimal Helm Values (configUrl mode)

```yaml
image:
  repository: ghcr.io/openabdev/openab
  tag: "0.9.0-beta.6"

agents:
  kiro:
    configUrl: "s3://my-bucket/openab/kiro/config.toml"
    serviceAccountName: "openab"  # IRSA for S3 + Secrets Manager
    persistence:
      enabled: true
      size: 1Gi
    # ── This is what Helm enforces (you don't touch config for this) ──
    securityContext:
      runAsUser: 1000
      runAsGroup: 1000
      runAsNonRoot: true
      readOnlyRootFilesystem: true
      allowPrivilegeEscalation: false
      capabilities:
        drop: ["ALL"]
```

**Why these three matter:**

| Helm-managed concern | What it prevents |
|---------------------|-----------------|
| `runAsUser: 1000` (non-root) | Container escape → host root access |
| `readOnlyRootFilesystem` | Runtime binary tampering, malware persistence outside HOME |
| HOME PVC (`persistence.enabled`) | Agent state loss on restart; provides durable workspace isolated from the immutable image |

## 6. Boot Behavior

OpenAB uses **fail-closed** boot semantics when `configUrl` is set:

- If the config source (S3/HTTPS) is unreachable at startup, the process exits with a non-zero code.
- Kubernetes will restart the pod per the Deployment's restart policy, providing automatic retry.
- There is no local cache or fallback — this is intentional to guarantee config freshness and avoid split-brain states.

This design choice is acceptable because:
- S3 provides 99.99% availability SLA.
- HTTPS endpoints (CDN-backed) have comparable availability.
- Pod restart loops are visible via standard Kubernetes monitoring (CrashLoopBackOff alerts).

## 7. Config Change Workflow

The recommended workflow is **edit-and-restart**:

1. Update `config.toml` in S3 (or HTTPS source).
2. Restart the pod: `kubectl rollout restart deployment/<agent>` or equivalent.
3. Pod fetches fresh config on boot.

**Hot-reload is explicitly out of scope for v1.** A future ADR may propose watch/poll mode, but the current design prioritizes simplicity and predictability.

## 8. Migration Path

For existing users on full Helm ConfigMap rendering:

### Step 1: Export current config

```bash
# Extract rendered config from the running ConfigMap
kubectl get configmap <agent>-config -o jsonpath='{.data.config\.toml}' > config.toml
```

### Step 2: Upload to S3

```bash
# Recommended bucket structure
aws s3 cp config.toml s3://my-openab-configs/agents/<name>/config.toml

# Enable versioning for rollback capability
aws s3api put-bucket-versioning \
  --bucket my-openab-configs \
  --versioning-configuration Status=Enabled
```

### Step 3: Configure IRSA

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": ["s3:GetObject"],
      "Resource": "arn:aws:s3:::my-openab-configs/agents/*"
    },
    {
      "Effect": "Allow",
      "Action": ["secretsmanager:GetSecretValue"],
      "Resource": "arn:aws:secretsmanager:*:*:secret:openab/*"
    }
  ]
}
```

### Step 4: Switch Helm values

```yaml
# Before (ConfigMap rendering)
agents:
  kiro:
    discord:
      token: "aws-sm://openab/kiro/discord-token"
    # ... 50+ lines of config in values.yaml

# After (configUrl mode)
agents:
  kiro:
    configUrl: "s3://my-openab-configs/agents/kiro/config.toml"
    serviceAccountName: "openab"
```

### Step 5: Deploy and verify

```bash
helm upgrade openab charts/openab -f values.yaml
kubectl logs -f deployment/kiro | head -20
# Look for: "Config loaded from s3://..."
```

## 9. Pre-deploy Validation

Since Helm template-time checks (e.g. Discord ID precision, enum validation) no longer apply in `configUrl` mode, validation shifts to:

1. **Fail-closed boot** — OAB validates config on startup and exits with clear error messages if invalid.
2. **`openab config validate`** (planned) — CLI command to validate a config.toml before uploading, suitable for CI pipelines.
3. **S3 versioning** — enables instant rollback to last-known-good config if a bad config is deployed.

## 10. Consequences

### Positive

- **Platform parity** — `configUrl` gives Kubernetes, ECS, Zeabur, and AgentCore users the identical config workflow; no platform is forced to adopt Helm to get a good experience.
- **Fleet-scale hot-reload-by-restart** — N agents sharing one `configUrl` pick up a change fleet-wide in seconds by restarting, with no chart change and no CI/CD pipeline required.
- **Shareable, credential-free config** — with secrets resolved via `aws-sm://`, a `config.toml` (and its `configUrl`) can be published or shared so others reproduce the exact same agent behavior instantly.
- **Zero chart maintenance for new config features** — schema changes never propagate to Helm.
- **Users see the full config** — no mental model of values → template → ConfigMap required.
- **Edit-and-restart workflow** — change config in S3/gist, restart pod (or ECS/Zeabur/AgentCore equivalent), done.
- **Aligned with declarative tooling** — ecsctl, oabctl, and Operator all operate on final config state.
- **Reduced issue surface** — eliminates "my Helm values don't render correctly" class of bugs.
- **S3 availability** — `s3://` path gives 99.99% SLA, private access via IAM, versioning, and CloudTrail audit.

### Negative

- **Boot-time dependency on S3/network** — if the config source is unreachable, OAB cannot start. Mitigated by S3's extreme availability and pod restart policy.
- **Backward compatibility** — existing users on full Helm rendering need the migration path above.
- **No pre-deploy validation** — Helm template-time checks no longer catch errors before deploy. Mitigated by fail-closed boot and planned `openab config validate` CLI command.

### Neutral

- Helm is not deprecated — it remains the recommended way to enforce runtime security posture on Kubernetes. Its scope simply narrows, and it is one of several platform integrations (alongside ecsctl, Zeabur, AgentCore) rather than the architecture's default assumption.
- Multi-agent deployments still benefit from Helm's `agents.<name>` loop for generating multiple Deployments/PVCs from a single release.

## 11. References

- `docs/config-reference.md` — s3:// config source documentation
- `docs/secrets-management.md` — aws-sm:// provider
- `crates/openab-core/src/config.rs` — s3:// URI parser
- `operator/examples/fleet.yaml` — fleet-scale s3:// configFrom usage
