# OpenAB Version Upgrade SOP

> [AI Instruction] Follow this documentation to assist the user in executing the upgrade process efficiently while ensuring all backup and rollback protocols are met.

| | |
|---|---|
| **Document Version** | 1.5 |
| **Last Updated** | 2026-04-15 |

## Environment Reference

| Item | Details |
|---|---|
| Deployment Method | Kubernetes (Helm Chart) |
| Deployment Name | `<release-name>-kiro` (default: `openab-kiro`) — see note below |
| Pod Label (precise) | `app.kubernetes.io/instance=<release-name>,app.kubernetes.io/name=<release-name>-kiro` |
| Helm Repo (OCI, recommended) | `oci://ghcr.io/openabdev/charts/openab` |
| Helm Repo (GitHub Pages, fallback) | `https://openabdev.github.io/openab` |
| Image Registry | `ghcr.io/openabdev/openab` |
| Git Repo | `github.com/openabdev/openab` |
| Agent Config | `/home/agent/.kiro/agents/default.json` |
| Steering Files | `/home/agent/.kiro/steering/` |
| kiro-cli Auth DB | `/home/agent/.local/share/kiro-cli/data.sqlite3` |
| PVC Mount Path | `/home/agent` (Helm); `.kiro` / `.local/share/kiro-cli` (raw k8s) |
| KUBECONFIG | `~/.kube/config` (must be set explicitly — default k3s config has insufficient permissions) |
| Namespace | `default` (adjust to match your actual deployment namespace) |

> **Deployment naming pattern:** The deployment name follows `<release-name>-kiro`. For the default setup (`helm install openab …`), the deployment is `openab-kiro`. If you used a different release name (e.g. `my-bot`), the deployment is `my-bot-kiro`. Verify with:
> ```bash
> RELEASE_NAME=$(helm list -o json | jq -r '.[] | select(.chart | startswith("openab-")) | .name' | head -1)
> DEPLOYMENT="${RELEASE_NAME}-kiro"
> echo "Deployment: $DEPLOYMENT"
> ```

> ⚠️ The local kubectl defaults to reading `/etc/rancher/k3s/k3s.yaml`, which will result in a permission denied error. Before running any command, always set:
> ```bash
> export KUBECONFIG=~/.kube/config
> ```

> 💡 **Namespace setup (recommended):** If OpenAB is deployed in a non-default namespace, set the following at the start of your session to avoid having to append `-n <namespace>` to every command:
> ```bash
> export NS=openab          # replace with your actual namespace
> export KUBECONFIG=~/.kube/config
> alias kubectl="kubectl -n $NS"
> alias helm="helm -n $NS"
> ```
> All `kubectl` and `helm` commands in this SOP assume either the default namespace or that the above aliases are in effect.

> ⚠️ **Data loss warning:** `helm uninstall` **deletes the PVC** and all persistent data (steering files, auth database, agent config) unless the chart has an explicit resource policy annotation. Always use `helm rollback` instead of uninstall + reinstall. If you need to uninstall, back up the PVC data first.

---

## Upgrade Process Overview

```
┌─────────────────────────────────────────────────────────────┐
│                  0. Environment Readiness Check              │
│  kubectl / helm / jq / curl / KUBECONFIG / cluster access   │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                     I. Pre-Upgrade Preparation               │
│  Resolve vars → Save session env file → Read release notes  │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                          II. Backup                          │
│  Steps 0–7 → Verification Gate (all files non-empty) ✅      │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                 III. Upgrade Execution (2 Phases)            │
│                                                             │
│  Step 1: Pre-release Validation                             │
│    Check beta.1 exists → deploy → automated smoke test      │
│    → ⏸ HUMAN CONFIRMATION → proceed or rollback            │
│                                                             │
│  Step 2: Promote to Stable                                  │
│    helm upgrade (OCI) → rollout status → verification       │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                    IV. Rollback                              │
│  PREV_REVISION from backup helm-history.json                │
│  Machine-readable decision table → rollback → verify        │
└─────────────────────────────────────────────────────────────┘
```

---

## 0. Environment Readiness Check

> **Agent instruction:** Run this section before anything else. If any check fails, stop and resolve the issue before proceeding. Do not attempt workarounds.
>
> **Expected output on success:** All lines print `✅` and the final line reads `✅ Environment ready.`

```bash
export KUBECONFIG=~/.kube/config

echo "=== Environment Readiness Check ==="
READY=true

check_cmd() {
  if command -v "$1" > /dev/null 2>&1; then
    echo "  ✅ $1 found"
  else
    echo "  ❌ $1 not found — install it before proceeding"
    READY=false
  fi
}

check_cmd kubectl
check_cmd helm
check_cmd jq
check_cmd curl
check_cmd awk
check_cmd tar

echo ""
echo "  KUBECONFIG: $KUBECONFIG"
if [ -f "$KUBECONFIG" ]; then
  echo "  ✅ KUBECONFIG file exists"
else
  echo "  ❌ KUBECONFIG file not found at $KUBECONFIG"
  READY=false
fi

CURRENT_CONTEXT=$(kubectl config current-context 2>/dev/null)
if [ -n "$CURRENT_CONTEXT" ]; then
  echo "  ✅ kubectl context: $CURRENT_CONTEXT"
else
  echo "  ❌ No kubectl context — check KUBECONFIG"
  READY=false
fi

if kubectl cluster-info > /dev/null 2>&1; then
  echo "  ✅ Cluster reachable"
else
  echo "  ❌ Cannot reach cluster — check KUBECONFIG and cluster status"
  READY=false
fi

echo ""
if [ "$READY" = true ]; then
  echo "✅ Environment ready. Proceed to Section I."
else
  echo "❌ Environment not ready. Fix the issues above before proceeding."
  exit 1
fi
```

---

## I. Pre-Upgrade Preparation

### 1. Resolve All Session Variables

> **Agent instruction:** Run this entire block as one unit. All subsequent sections depend on `openab-session-env.sh`. If the session file already exists from a previous run (e.g. backup was done earlier and upgrade is now resuming), source it instead of re-running this block.
>
> **Output:** `openab-session-env.sh` → sourced by all subsequent sections.

```bash
export KUBECONFIG=~/.kube/config

# If resuming a previous session, source the saved env and skip this block:
# source openab-session-env.sh && echo "✅ Session env loaded" && exit 0

# --- Resolve release and deployment names ---
RELEASE_NAME=$(helm list -o json | jq -r '.[] | select(.chart | startswith("openab-")) | .name' | head -1)
if [ -z "$RELEASE_NAME" ]; then
  echo "❌ No Helm release found. Is OpenAB installed?"
  exit 1
fi
DEPLOYMENT="${RELEASE_NAME}-kiro"
echo "Release: $RELEASE_NAME  |  Deployment: $DEPLOYMENT"
# Expected output contains: "Release: openab  |  Deployment: openab-kiro"

# --- Resolve current version ---
CURRENT_VERSION=$(helm list -f "$RELEASE_NAME" -o json | jq -r '.[0].chart' | sed 's/openab-//')
echo "Current chart version: $CURRENT_VERSION"

# --- Resolve target version (latest stable from OCI, no pre-release tags) ---
TARGET_VERSION=$(helm show chart oci://ghcr.io/openabdev/charts/openab 2>/dev/null \
  | grep '^version:' | awk '{print $2}')
if [ -z "$TARGET_VERSION" ]; then
  echo "❌ Could not resolve target version from OCI registry."
  echo "   Check network connectivity and registry access."
  exit 1
fi
echo "Target version (latest stable from OCI): $TARGET_VERSION"
# Expected output: "Target version (latest stable from OCI): 0.7.5"

# If you need to upgrade to a specific version instead of latest, override here:
# TARGET_VERSION="0.7.4"

# --- Check if upgrade is needed ---
if [ "$CURRENT_VERSION" = "$TARGET_VERSION" ]; then
  echo "ℹ️  Already on the latest version ($TARGET_VERSION). No upgrade needed."
  echo "   If you still want to proceed (e.g. force re-deploy), continue manually."
  exit 0
fi

# --- Check pre-release availability (determines Step 1 path) ---
if helm show chart oci://ghcr.io/openabdev/charts/openab \
     --version "${TARGET_VERSION}-beta.1" > /dev/null 2>&1; then
  PRERELEASE_VERSION="${TARGET_VERSION}-beta.1"
  echo "✅ Pre-release found: $PRERELEASE_VERSION"
else
  # Check if release notes explicitly mark this as pre-validated
  RELEASE_NOTES=$(gh api "repos/openabdev/openab/releases/tags/v${TARGET_VERSION}" \
    --jq '.body' 2>/dev/null || true)
  if echo "$RELEASE_NOTES" | grep -q 'pre-release-validated: true'; then
    PRERELEASE_VERSION=""
    echo "✅ Release notes contain pre-release-validated: true — Step 1 will be skipped"
  else
    echo "❌ STOP: ${TARGET_VERSION}-beta.1 not found in OCI registry."
    echo "   Release notes do not contain 'pre-release-validated: true'."
    echo "   Options:"
    echo "   1. Wait for the project to publish ${TARGET_VERSION}-beta.1"
    echo "   2. Check GitHub releases for an alternative pre-release tag:"
    echo "      gh release list --repo openabdev/openab"
    echo "   3. If a different pre-release tag is available (e.g. beta.2), set:"
    echo "      PRERELEASE_VERSION=\"${TARGET_VERSION}-beta.2\""
    echo "   Do NOT proceed until a pre-release is available or the release notes"
    echo "   explicitly contain 'pre-release-validated: true'."
    exit 1
  fi
fi

# --- Save session environment file ---
cat > openab-session-env.sh <<EOF
# OpenAB upgrade session — generated $(date)
export KUBECONFIG=~/.kube/config
export RELEASE_NAME="${RELEASE_NAME}"
export DEPLOYMENT="${DEPLOYMENT}"
export CURRENT_VERSION="${CURRENT_VERSION}"
export TARGET_VERSION="${TARGET_VERSION}"
export PRERELEASE_VERSION="${PRERELEASE_VERSION}"
# BACKUP_DIR will be appended by Section II
EOF
echo "✅ Session env saved to openab-session-env.sh"
echo "   Source it in subsequent sessions: source openab-session-env.sh"
```

### 2. Read the Release Notes

```bash
source openab-session-env.sh

echo "Release notes URL: https://github.com/openabdev/openab/releases/tag/v${TARGET_VERSION}"

# Print release notes to terminal for review
gh release view "v${TARGET_VERSION}" --repo openabdev/openab 2>/dev/null \
  || echo "⚠️ Could not fetch release notes via gh CLI — check the URL manually"
```

Pay special attention to:
- Breaking changes
- Helm Chart values changes
- Added or deprecated environment variables
- Any migration steps

> ⚠️ **`agentsMd` takes precedence over PVC files:** If you have `agentsMd` set in your Helm
> values, the ConfigMap volumeMount will shadow any existing file at the same path on the PVC
> (e.g. `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`). The PVC file is not deleted but becomes
> invisible to the agent. If you rely on PVC-managed agent instruction files, either leave
> `agentsMd` empty or migrate its content to the PVC file before upgrading.

### 3. Check Node Resources

```bash
source openab-session-env.sh

kubectl describe nodes | grep -A 5 "Allocatable:"
kubectl top nodes
```

> Skipping this step risks the new Pod getting stuck in `Pending` if the node lacks capacity.

### 4. Announce the Upgrade

> ⚠️ **Downtime is expected during every upgrade.** The deployment strategy is `Recreate` because the PVC is ReadWriteOnce, which does not support RollingUpdate. The old Pod is terminated before the new one starts — the Discord bot will be unavailable during this window, and this is expected behaviour.

```bash
source openab-session-env.sh

# Option A: Discord webhook notification (set DISCORD_WEBHOOK_URL in environment)
if [ -n "${DISCORD_WEBHOOK_URL:-}" ]; then
  curl -s -X POST "$DISCORD_WEBHOOK_URL" \
    -H "Content-Type: application/json" \
    -d "{\"content\": \"🔧 **Upgrade starting:** OpenAB is being upgraded from v${CURRENT_VERSION} to v${TARGET_VERSION}. The bot will be offline for approximately 1–3 minutes.\"}"
  echo "✅ Discord notification sent"
else
  echo "ℹ️  DISCORD_WEBHOOK_URL not set — skipping automated notification"
  echo "   Notify users manually: OpenAB upgrading v${CURRENT_VERSION} → v${TARGET_VERSION}, ~1–3 min downtime"
fi
```

---

## II. Backup

> **Agent instruction — dependency chain:**
> - `openab-session-env.sh` must exist (created in Section I)
> - Steps 0–7 must run in order
> - The Verification Gate must print `✅ GATE PASSED` before proceeding to Section III
> - `BACKUP_DIR` is appended to `openab-session-env.sh` after Step 0

### Agent-Executable Backup (Linear Sequence)

#### Step 0 — Resolve variables and create backup directory

> **Output:** `BACKUP_DIR` appended to `openab-session-env.sh` → used in Steps 1–7 and the Verification Gate.
>
> **Why `POD` is not saved to `openab-session-env.sh`:** The pod name changes after every `helm upgrade` or `kubectl rollout restart` (new pod is created, old one is terminated). Persisting the pod name would cause subsequent steps to target a pod that no longer exists. Each step re-resolves `POD` at runtime to ensure it always refers to the currently running pod.

```bash
source openab-session-env.sh

BACKUP_DIR="openab-backup-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP_DIR"
echo "Backup directory: $BACKUP_DIR"

POD=$(kubectl get pod \
  -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}" \
  -o jsonpath='{.items[0].metadata.name}')
echo "Pod: $POD"

if [ -z "$POD" ]; then
  echo "❌ Pod not found. Cannot proceed with backup."
  exit 1
fi

if ! kubectl exec "$POD" -- which tar > /dev/null 2>&1; then
  echo "❌ tar not found in container. kubectl cp of directories will fail. Aborting."
  exit 1
fi

# Append BACKUP_DIR to session env file
echo "export BACKUP_DIR=\"${BACKUP_DIR}\"" >> openab-session-env.sh
echo "✅ BACKUP_DIR saved to openab-session-env.sh"
```

#### Step 1 — Backup Helm values

> **Output:** `$BACKUP_DIR/values.yaml`
> **Expected:** file size > 0 bytes

```bash
source openab-session-env.sh
helm get values "$RELEASE_NAME" -o yaml > "$BACKUP_DIR/values.yaml"
echo "✅ Helm values backed up ($(wc -c < "$BACKUP_DIR/values.yaml") bytes)"
```

#### Step 2 — Backup agent config

> **Input:** `POD` (re-resolved) · **Output:** `$BACKUP_DIR/agents/`

```bash
source openab-session-env.sh
POD=$(kubectl get pod -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}" -o jsonpath='{.items[0].metadata.name}')
kubectl cp "$POD:/home/agent/.kiro/agents/" "$BACKUP_DIR/agents/"
echo "✅ Agent config backed up"
```

#### Step 3 — Backup steering files

> **Input:** `POD` (re-resolved) · **Output:** `$BACKUP_DIR/steering/`

```bash
source openab-session-env.sh
POD=$(kubectl get pod -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}" -o jsonpath='{.items[0].metadata.name}')
kubectl cp "$POD:/home/agent/.kiro/steering/" "$BACKUP_DIR/steering/"
echo "✅ Steering files backed up"
```

#### Step 4 — Backup skills (optional)

> **Input:** `POD` (re-resolved) · **Output:** `$BACKUP_DIR/skills/` (may be absent)

```bash
source openab-session-env.sh
POD=$(kubectl get pod -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}" -o jsonpath='{.items[0].metadata.name}')
if kubectl exec "$POD" -- test -d /home/agent/.kiro/skills 2>/dev/null; then
  kubectl cp "$POD:/home/agent/.kiro/skills/" "$BACKUP_DIR/skills/"
  echo "✅ Skills directory backed up"
else
  echo "⚠️ skills/ not found in container — skipping (normal if no custom skills are installed)"
fi
```

#### Step 5 — Backup GitHub CLI credentials and kiro-cli auth

> **Input:** `POD` (re-resolved) · **Output:** `$BACKUP_DIR/hosts.yml`, `$BACKUP_DIR/kiro-auth.sqlite3`

```bash
source openab-session-env.sh
POD=$(kubectl get pod -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}" -o jsonpath='{.items[0].metadata.name}')
kubectl cp "$POD:/home/agent/.config/gh/hosts.yml" "$BACKUP_DIR/hosts.yml"
echo "✅ hosts.yml backed up ($(wc -c < "$BACKUP_DIR/hosts.yml") bytes)"

kubectl cp "$POD:/home/agent/.local/share/kiro-cli/data.sqlite3" "$BACKUP_DIR/kiro-auth.sqlite3"
echo "✅ kiro-cli auth DB backed up ($(wc -c < "$BACKUP_DIR/kiro-auth.sqlite3") bytes)"
```

#### Step 6 — Backup Kubernetes Secret

> **Output:** `$BACKUP_DIR/secret.yaml` ⚠️ SENSITIVE

```bash
source openab-session-env.sh
kubectl get secret "${DEPLOYMENT}" -o yaml > "$BACKUP_DIR/secret.yaml"
echo "✅ Secret backed up ($(wc -c < "$BACKUP_DIR/secret.yaml") bytes)"
echo "🔐 SECURITY: secret.yaml contains credentials — do NOT commit. Encrypt before storing:"
echo "   gpg --symmetric $BACKUP_DIR/secret.yaml"
```

#### Step 7 — Backup Helm release history and full PVC snapshot

> **Input:** `POD` (re-resolved) · **Output:** `$BACKUP_DIR/helm-history.txt`, `$BACKUP_DIR/pvc-data/`
>
> **Note on PVC overlap:** `pvc-data/` copies the entire `/home/agent` directory, which includes paths already backed up individually in Steps 2–5 (agents/, steering/, hosts.yml, kiro-auth.sqlite3). This overlap is **intentional** — the full PVC snapshot is the last-resort restore path if the new version ran a data migration that corrupts the PVC. The individual backups in Steps 2–5 are for fast, targeted restores; `pvc-data/` is for full rollback of PVC state.
>
> **Size threshold:** If the PVC is larger than ~500 MB, `kubectl cp` may be slow or time out. In that case, use the VolumeSnapshot option below instead.

```bash
source openab-session-env.sh
POD=$(kubectl get pod -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}" -o jsonpath='{.items[0].metadata.name}')

helm history "$RELEASE_NAME" > "$BACKUP_DIR/helm-history.txt"
helm history "$RELEASE_NAME" --output json > "$BACKUP_DIR/helm-history.json"
echo "✅ Helm history backed up (text + JSON)"
# helm-history.json is the source of truth for PREV_REVISION used in Section IV rollback
# JSON format avoids column-shift parsing issues across Helm versions

PVC_SIZE_BYTES=$(kubectl exec "$POD" -- du -sb /home/agent 2>/dev/null | cut -f1)
PVC_SIZE_HUMAN=$(kubectl exec "$POD" -- du -sh /home/agent 2>/dev/null | cut -f1)
echo "PVC size: $PVC_SIZE_HUMAN"
if [ "${PVC_SIZE_BYTES:-0}" -gt 524288000 ]; then
  echo "⚠️ PVC exceeds 500 MB — kubectl cp may be slow or time out."
  echo "   Consider using the VolumeSnapshot option below instead."
fi
kubectl cp "$POD:/home/agent/" "$BACKUP_DIR/pvc-data/"
echo "✅ Full PVC snapshot backed up"
```

> **Advanced option — VolumeSnapshot (for large PVCs or CSI-enabled clusters):**
> ```bash
> source openab-session-env.sh
> PVC_NAME=$(kubectl get pod "$POD" \
>   -o jsonpath='{.spec.volumes[?(@.persistentVolumeClaim)].persistentVolumeClaim.claimName}')
> SNAPSHOT_CLASS=$(kubectl get volumesnapshotclass -o jsonpath='{.items[0].metadata.name}')
> echo "PVC: $PVC_NAME  |  SnapshotClass: $SNAPSHOT_CLASS"
> kubectl apply -f - <<EOF
> apiVersion: snapshot.storage.k8s.io/v1
> kind: VolumeSnapshot
> metadata:
>   name: openab-pvc-snapshot-$(date +%Y%m%d)
> spec:
>   volumeSnapshotClassName: ${SNAPSHOT_CLASS}
>   source:
>     persistentVolumeClaimName: ${PVC_NAME}
> EOF
> ```

#### Verification Gate — must pass before proceeding to upgrade

> **Agent instruction:** Run this gate after all backup steps. If output does not contain `✅ GATE PASSED`, **stop immediately** and do not proceed with the upgrade.

```bash
source openab-session-env.sh
echo "=== Backup Verification Gate ==="
GATE_PASS=true

check_file() {
  local path="$1"; local label="$2"
  if [ -s "$path" ]; then
    echo "  ✅ $label ($(wc -c < "$path") bytes)"
  else
    echo "  ❌ MISSING or EMPTY: $label ($path)"
    GATE_PASS=false
  fi
}

check_dir() {
  local path="$1"; local label="$2"
  if [ -d "$path" ] && [ -n "$(ls -A "$path" 2>/dev/null)" ]; then
    echo "  ✅ $label ($(ls "$path" | wc -l) files)"
  else
    echo "  ❌ MISSING or EMPTY: $label ($path)"
    GATE_PASS=false
  fi
}

check_file "$BACKUP_DIR/values.yaml"          "Helm values"
check_dir  "$BACKUP_DIR/agents/"              "Agent config"
check_dir  "$BACKUP_DIR/steering/"            "Steering files"
check_file "$BACKUP_DIR/hosts.yml"            "GitHub CLI credentials"
check_file "$BACKUP_DIR/kiro-auth.sqlite3"    "kiro-cli auth DB"
check_file "$BACKUP_DIR/secret.yaml"          "Kubernetes Secret"
check_file "$BACKUP_DIR/helm-history.txt"     "Helm history (text)"
check_file "$BACKUP_DIR/helm-history.json"    "Helm history (JSON — used for PREV_REVISION)"
check_dir  "$BACKUP_DIR/pvc-data/"            "PVC data"

echo ""
if [ "$GATE_PASS" = true ]; then
  echo "✅ GATE PASSED — all backup files present and non-empty. Safe to proceed with upgrade."
else
  echo "❌ GATE FAILED — one or more backup files are missing or empty."
  echo "   Do NOT proceed with the upgrade until all checks pass."
  exit 1
fi
```

---

## III. Upgrade Execution

> **Agent instruction — session continuity:**
> - Source `openab-session-env.sh` at the start of each step
> - If resuming after a gap (e.g. backup was done earlier), verify `BACKUP_DIR` matches the intended backup:
>   ```bash
>   source openab-session-env.sh
>   echo "BACKUP_DIR: $BACKUP_DIR"
>   echo "Backup time: $(echo "$BACKUP_DIR" | grep -oE '[0-9]{8}-[0-9]{6}')"
>   ls "$BACKUP_DIR/"
>   # Confirm this is the correct backup before proceeding
>   ```

### Step 1: Pre-release Validation

> ⚠️ Per project convention, **a stable release must be preceded by a validated pre-release**. Do not skip directly to Step 2.
>
> **Agent note — branch resolution:**
> - If `PRERELEASE_VERSION` is empty (set during Section I because `pre-release-validated: true` was found in release notes): **skip this entire step**, proceed directly to Step 2.
> - If `PRERELEASE_VERSION` is non-empty: run the full step below.
> - If this step fails (automated smoke test fails): run rollback (Section IV) and **stop** — do not proceed to Step 2.

```bash
source openab-session-env.sh

if [ -z "$PRERELEASE_VERSION" ]; then
  echo "ℹ️  PRERELEASE_VERSION is empty — pre-release step was skipped during env setup."
  echo "   Proceeding directly to Step 2."
  exit 0
fi

echo "Deploying pre-release: $PRERELEASE_VERSION"

# Dry-run first
helm upgrade "$RELEASE_NAME" oci://ghcr.io/openabdev/charts/openab \
  --version "$PRERELEASE_VERSION" \
  -f "$BACKUP_DIR/values.yaml" \
  --dry-run
# Expected output contains: "Release \"openab\" has been upgraded. Happy Helming!"

# Deploy pre-release
helm upgrade "$RELEASE_NAME" oci://ghcr.io/openabdev/charts/openab \
  --version "$PRERELEASE_VERSION" \
  -f "$BACKUP_DIR/values.yaml"

kubectl rollout status "deployment/${DEPLOYMENT}" --timeout=300s
# Expected output: "deployment/<DEPLOYMENT> successfully rolled out"

# --- Automated smoke test ---
# Estimated duration: 30–60 seconds
POD=$(kubectl get pod -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}" -o jsonpath='{.items[0].metadata.name}')
kubectl wait --for=condition=Ready "pod/${POD}" --timeout=120s
# Expected output: "pod/<POD> condition met"

kubectl exec "$POD" -- pgrep -x openab
# Expected output: a numeric PID (e.g. "42") — non-zero exit means process not running

PANIC_LINES=$(kubectl logs "deployment/${DEPLOYMENT}" --tail=100 | grep -icE "panic|fatal" || true)
if [ "$PANIC_LINES" -gt 0 ]; then
  echo "❌ Panic/fatal lines found in logs. Automated smoke test FAILED."
  echo "   Run rollback (Section IV) and do not proceed to Step 2."
  exit 1
fi
echo "✅ Automated smoke test passed."
```

**After automated smoke test — human Discord validation required:**

> **Agent note:** If running in a non-interactive shell (no stdin available), skip the `read` command below. Instead, report to the user that human confirmation is required and pause execution. Resume only after the user explicitly provides `CONFIRMED` or `ROLLBACK`.

```bash
# ⏸ HUMAN CONFIRMATION REQUIRED
# Estimated wait: 2–5 minutes
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "⏸  PAUSED — Human action required before continuing"
echo ""
echo "  1. Send a test message to the Discord channel"
echo "  2. Confirm the bot responds and basic conversation / tool calls work"
echo ""
echo "  When confirmed OK, type:  CONFIRMED"
echo "  To abort and rollback,    type:  ROLLBACK"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
read -t 600 -r HUMAN_INPUT || { echo "❌ Timeout: no human input received within 600s. Aborting."; exit 1; }
case "$HUMAN_INPUT" in
  CONFIRMED)
    echo "✅ Human confirmed — proceeding to Step 2"
    ;;
  ROLLBACK)
    echo "🔁 Rollback requested by human. Proceed to Section IV."
    exit 2
    ;;
  *)
    echo "❌ Unrecognized input ('$HUMAN_INPUT'). Aborting for safety."
    echo "   Run rollback (Section IV) if needed."
    exit 1
    ;;
esac
```

### Step 2: Promote to Stable

> **Agent instruction:** Only run this after Step 1 is fully complete (automated + human confirmation), or after confirming `PRERELEASE_VERSION` was empty.

```bash
source openab-session-env.sh

echo "Promoting to stable: $TARGET_VERSION"

# Dry-run
helm upgrade "$RELEASE_NAME" oci://ghcr.io/openabdev/charts/openab \
  --version "$TARGET_VERSION" \
  -f "$BACKUP_DIR/values.yaml" \
  --dry-run

# Deploy stable (short downtime expected due to Recreate strategy)
helm upgrade "$RELEASE_NAME" oci://ghcr.io/openabdev/charts/openab \
  --version "$TARGET_VERSION" \
  -f "$BACKUP_DIR/values.yaml"

kubectl rollout status "deployment/${DEPLOYMENT}" --timeout=300s
# Expected output: "deployment/<DEPLOYMENT> successfully rolled out"
# Estimated duration: 60–180 seconds
```

### Post-Upgrade Verification

> **Agent note — pass/fail criteria:**
> - **Pass:** All commands exit 0, deployed chart version equals `openab-${TARGET_VERSION}`, no panic/fatal in logs, PVC paths are present.
> - **Fail:** Any command exits non-zero, version mismatch, or panic/fatal in logs. → Proceed to Section IV Rollback immediately.

```bash
source openab-session-env.sh

POD=$(kubectl get pod \
  -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}" \
  -o jsonpath='{.items[0].metadata.name}')

# 1. Pod status
kubectl get pod -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}"
kubectl wait --for=condition=Ready "pod/${POD}" --timeout=120s
# Expected output: "pod/<POD> condition met"

# 2. Chart version
DEPLOYED=$(helm list -f "$RELEASE_NAME" -o json | jq -r '.[0].chart')
echo "Deployed: $DEPLOYED  |  Expected: openab-${TARGET_VERSION}"
if [ "$DEPLOYED" != "openab-${TARGET_VERSION}" ]; then
  echo "❌ Version mismatch. Investigate before proceeding."
  exit 1
fi

# 3. Image tag
kubectl get "deployment/${DEPLOYMENT}" \
  -o jsonpath='{.spec.template.spec.containers[0].image}{"\n"}'
# Expected output contains: TARGET_VERSION or its image SHA

# 4. Process check
kubectl exec "$POD" -- pgrep -x openab
# Expected output: a numeric PID

# 5. Log check
PANIC_LINES=$(kubectl logs "deployment/${DEPLOYMENT}" --tail=100 | grep -icE "panic|fatal" || true)
WARN_LINES=$(kubectl logs "deployment/${DEPLOYMENT}" --tail=100 | grep -icE "error|warn" || true)
echo "Panic/fatal: $PANIC_LINES  |  Error/warn: $WARN_LINES"
if [ "$PANIC_LINES" -gt 0 ]; then
  echo "❌ Panic/fatal found. Proceed to Section IV Rollback."
  exit 1
fi

# 6. PVC data integrity
kubectl exec "$POD" -- ls /home/agent/.kiro/steering/
# Expected output: at least one file listed (e.g. IDENTITY.md)
kubectl exec "$POD" -- cat /home/agent/.kiro/agents/default.json | head -5
# Expected output: first 5 lines of valid JSON

echo "✅ All automated checks passed."
```

**After automated checks — human Discord E2E confirmation:**

> **Agent note:** If running in a non-interactive shell (no stdin available), skip the `read` command below. Instead, report to the user that human confirmation is required and pause execution. Resume only after the user explicitly provides `CONFIRMED` or `ROLLBACK`.

```bash
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "⏸  PAUSED — Human E2E validation required"
echo ""
echo "  Send a test message in the Discord channel."
echo "  Confirm the bot responds and conversation works correctly."
echo ""
echo "  When confirmed OK, type:  CONFIRMED"
echo "  If issues found,   type:  ROLLBACK"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
read -t 600 -r HUMAN_INPUT || { echo "❌ Timeout: no human input received within 600s. Aborting."; exit 1; }
case "$HUMAN_INPUT" in
  CONFIRMED) echo "✅ Upgrade complete." ;;
  ROLLBACK)  echo "🔁 Rollback requested. Proceed to Section IV."; exit 2 ;;
  *)         echo "❌ Unrecognized input. Aborting."; exit 1 ;;
esac
```

### Completion Notice

```bash
source openab-session-env.sh

# Send completion notification via Discord webhook (if configured)
if [ -n "${DISCORD_WEBHOOK_URL:-}" ]; then
  curl -s -X POST "$DISCORD_WEBHOOK_URL" \
    -H "Content-Type: application/json" \
    -d "{\"content\": \"✅ **Upgrade complete:** OpenAB is now running v${TARGET_VERSION}. Service restored.\"}"
  echo "✅ Completion notice sent"
else
  echo "ℹ️  Notify users manually: OpenAB upgraded to v${TARGET_VERSION}, service restored."
fi
```

---

## IV. Rollback

> ⚠️ **`helm rollback` does NOT revert PVC data.** Helm only rolls back Kubernetes resources (Deployment, ConfigMap, Secret, etc.). The PVC and its contents remain as-is after rollback.
>
> If the new version ran a data migration on startup, the old version may not be compatible with the modified PVC data. In that case, restore PVC data from the Step 7 backup **before** running `helm rollback`:
> ```bash
> # Restore PVC data from backup first (see "Restore Custom Config" below)
> # Then run helm rollback
> ```

### Decision Table (Machine-Readable)

> **Agent instruction:** Evaluate conditions in order. Execute the action for the first matching row. Only one action should be taken per rollback event.

| Condition to check | How to check | Action |
|---|---|---|
| Pod phase is `CrashLoopBackOff` or `Pending` | `kubectl get pod ... -o jsonpath='{.items[0].status.phase}'` | `helm rollback` immediately |
| Pod is `Running` AND `pgrep -x openab` exits non-zero | `kubectl exec $POD -- pgrep -x openab; echo $?` | `helm rollback` |
| Pod is `Running`, process OK, logs contain `panic` or `fatal` | `kubectl logs ... \| grep -icE "panic\|fatal"` | `helm rollback` |
| Pod is `Running`, process OK, logs clean, no Discord response after 60s | Human reports no response | `kubectl rollout restart` first; if still no response after 60s → `helm rollback` |
| Pod is `Running`, process OK, bot responds, but config files missing | `kubectl exec $POD -- ls /home/agent/.kiro/steering/` | Restore from backup → `kubectl rollout restart` |
| Quick fix is clearly identified (e.g. known bad config key) | Human identifies root cause | Hotfix — escalate to human engineer |

### Helm Rollback

> **Agent instruction:** `PREV_REVISION` is resolved from `helm-history.json` saved during Step 7 (before any upgrade occurred). Using the JSON format avoids column-shift parsing issues across Helm versions. This also avoids the ambiguity of "second-to-last revision" when multiple `helm upgrade` calls were made (pre-release + stable).

```bash
source openab-session-env.sh

# Validate BACKUP_DIR is set and helm-history.txt exists
if [ -z "$BACKUP_DIR" ] || [ ! -f "$BACKUP_DIR/helm-history.json" ]; then
  echo "❌ BACKUP_DIR not set or helm-history.json missing."
  echo "   Resolve manually: helm history $RELEASE_NAME --output json | jq"
  exit 1
fi

echo "Using backup: $BACKUP_DIR"
echo "Backup timestamp: $(echo "$BACKUP_DIR" | grep -oE '[0-9]{8}-[0-9]{6}')"

# Resolve the pre-upgrade stable revision from the backup JSON
# (the last revision with status "deployed" at the time of backup)
# Uses JSON format saved during Step 7 — avoids column-shift parsing issues across Helm versions
PREV_REVISION=$(jq -r '[.[] | select(.status == "deployed")] | sort_by(.revision) | last | .revision' \
  "$BACKUP_DIR/helm-history.json" 2>/dev/null)
if [ -z "$PREV_REVISION" ] || [ "$PREV_REVISION" = "null" ]; then
  echo "❌ Could not resolve PREV_REVISION from helm-history.json."
  echo "   Contents of helm-history.json:"
  cat "$BACKUP_DIR/helm-history.json"
  echo ""
  echo "   Set PREV_REVISION manually and re-run: helm rollback $RELEASE_NAME <REVISION>"
  exit 1
fi
echo "Rolling back to revision: $PREV_REVISION (pre-upgrade stable)"

# Rollback
helm rollback "$RELEASE_NAME" "$PREV_REVISION"
# Expected output: "Rollback was a success! Happy Helming!"

kubectl rollout status "deployment/${DEPLOYMENT}" --timeout=300s
# Expected output: "deployment/<DEPLOYMENT> successfully rolled out"

# Confirm rollback
kubectl get pod -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}"
# Expected output: 1 pod in Running/Ready state

# Post-rollback verification
POD=$(kubectl get pod \
  -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}" \
  -o jsonpath='{.items[0].metadata.name}')
kubectl wait --for=condition=Ready "pod/${POD}" --timeout=120s
kubectl exec "$POD" -- pgrep -x openab
# Expected output: a numeric PID

PANIC_LINES=$(kubectl logs "deployment/${DEPLOYMENT}" --tail=100 | grep -icE "panic|fatal" || true)
echo "Panic/fatal after rollback: $PANIC_LINES"
if [ "$PANIC_LINES" -gt 0 ]; then
  echo "❌ Panic/fatal found even after rollback. Escalate to human engineer."
  exit 1
fi
echo "✅ Rollback complete. Send Discord test message to confirm bot is responsive."
```

### Restore Custom Config

```bash
source openab-session-env.sh

POD=$(kubectl get pod \
  -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/name=${DEPLOYMENT}" \
  -o jsonpath='{.items[0].metadata.name}')

echo "Restoring from: $BACKUP_DIR"

# Restore agent config
kubectl cp "$BACKUP_DIR/agents/default.json" "$POD:/home/agent/.kiro/agents/default.json"
echo "✅ Agent config restored"

# Restore steering files
# ⚠️ Use tar pipe to avoid nested directory issue (e.g. steering/steering/)
kubectl exec "$POD" -- mkdir -p /home/agent/.kiro/steering
tar c -C "$BACKUP_DIR/steering" . | kubectl exec -i "$POD" -- tar x -C /home/agent/.kiro/steering
echo "✅ Steering files restored"

# Restore GitHub CLI credentials
kubectl cp "$BACKUP_DIR/hosts.yml" "$POD:/home/agent/.config/gh/hosts.yml"
echo "✅ hosts.yml restored"

# Restore kiro-cli auth database
kubectl exec "$POD" -- mkdir -p /home/agent/.local/share/kiro-cli
kubectl cp "$BACKUP_DIR/kiro-auth.sqlite3" "$POD:/home/agent/.local/share/kiro-cli/data.sqlite3"
echo "✅ kiro-cli auth DB restored"

# Restart Pod to apply changes
kubectl rollout restart "deployment/${DEPLOYMENT}"
kubectl rollout status "deployment/${DEPLOYMENT}" --timeout=300s
echo "✅ Pod restarted with restored config"
```
