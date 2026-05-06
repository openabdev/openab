# OpenAB Install & Upgrade Guide

> **[AI Instruction]** Follow this guide to install or upgrade an OpenAB Helm deployment. Execute each section in order. If any step fails, follow the ROLLBACK section. Do not skip the backup (for upgrades).

## Goal

Enable an AI agent to fresh-install or upgrade OpenAB on Kubernetes. For upgrades: back up everything first, upgrade, validate, and automatically roll back if anything goes wrong.

## How to Use

Tell your AI agent to follow this guide. Example prompts:

```
# Fresh install with kiro on local k8s
fresh install https://github.com/openabdev/openab v0.7.7 with kiro on my local k8s with all credentials in .env

# Upgrade to latest stable
upgrade to latest stable for my local openab k8s deployment per https://github.com/openabdev/openab/blob/main/docs/ai-install-upgrade.md

# Upgrade to a specific version
upgrade to v0.7.7 for my local openab k8s deployment per https://github.com/openabdev/openab/blob/main/docs/ai-install-upgrade.md

# Upgrade to a beta
upgrade to v0.7.7-beta.1 for my local openab k8s deployment per https://github.com/openabdev/openab/blob/main/docs/ai-install-upgrade.md

# Rollback after a bad upgrade
rollback openab per the upgrade SOP — the upgrade to v0.7.7 failed
```

---

## Flow

```
  ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
  │  1. RESOLVE  │────►│  2. BACKUP  │────►│  3. UPGRADE │
  │   versions   │     │   3 items   │     │ helm upgrade│
  └─────────────┘     └──────┬──────┘     └──────┬──────┘
                             │                    │
                        fail │               ┌────┴────┐
                             │             pass      fail
                             │               │         │
                             ▼               ▼         ▼
                        ┌─────────┐    ┌──────────┐ ┌──────────┐
                        │  ABORT  │    │6. CLEANUP│ │5. ROLLBACK│
                        │         │    │  (ask    │ │          │
                        └─────────┘    │  user)   │ │ uninstall│
                                       └────┬─────┘ │ reinstall│
                                            ▼       │ restore  │
                                       ┌─────────┐  └──────────┘
                                       │  DONE ✅ │
                                       └─────────┘
```

**Invariant:** At every point, the system is either running the current version, running the target version, or being restored to the current version. No data is lost.

---

## 1. Resolve Versions

**Goal:** Determine current version, target version, and release name. If the user specifies a target (e.g. `0.7.7-beta.1`), use it. Otherwise resolve latest stable from the Helm repo.

```
  Helm Release          OCI / Helm Repo         User Override
  ┌────────────┐       ┌────────────────┐      ┌────────────┐
  │ CURRENT    │       │ LATEST STABLE  │      │ TARGET     │
  │ = helm list│       │ = helm show    │  or  │ = user     │
  │   chart ver│       │   chart version│      │   specified│
  └─────┬──────┘       └───────┬────────┘      └─────┬──────┘
        │                      │                      │
        └──────────┬───────────┘──────────────────────┘
                   ▼
          CURRENT == TARGET? ──yes──► exit (nothing to do)
                   │ no
                   ▼
            save to env file
```

**Success:** `RELEASE`, `CURRENT`, and `TARGET` are resolved and saved.
**If same version:** Exit — no upgrade needed.

---

## 2. Backup

**Goal:** Capture everything needed to fully restore the current deployment.

```
  Current Cluster                           Local Disk
  ┌──────────────┐    helm get values      ┌──────────────┐
  │ Helm Release  │ ──────────────────►    │ values.yaml  │
  ├──────────────┤    kubectl get secret   ├──────────────┤
  │ K8s Secret    │ ──────────────────►    │ secret.yaml  │
  ├──────────────┤    kubectl cp $HOME     ├──────────────┤
  │ Pod /home/    │ ──────────────────►    │ home/        │
  │    agent/     │                        │  (full snap) │
  └──────────────┘                         └──────────────┘
```

**Success:** All 3 items exist and are non-empty.
**Failure:** Do NOT proceed to upgrade.

> **Pod label selector:** `app.kubernetes.io/instance=$RELEASE,app.kubernetes.io/component=kiro`

> **Gateway config migration (one-time, if applicable):** If you previously enabled a custom gateway by manually patching the ConfigMap (e.g. adding `[gateway]` to `config.toml` by hand), that block is not captured by `helm get values`. Before upgrading, copy the gateway settings into your `values.yaml` under `agents.<name>.gateway` and set `enabled: true` so they are preserved on every subsequent `helm upgrade`. See chart `values.yaml` for the field reference (`enabled`, `url`, `platform`, `token`, `botUsername`). After migrating, do not manually edit the ConfigMap again — manage gateway config through `values.yaml` only.

> **Usercron path migration (v0.8.2+):** The usercron `cronjob.toml` path resolution changed from `$HOME/` to `$HOME/.openab/`. If you have an existing `cronjob.toml` in the agent's home directory, move it before upgrading:
> ```
> mkdir -p $HOME/.openab
> mv $HOME/cronjob.toml $HOME/.openab/cronjob.toml
> ```
> The scheduler will not pick up the file from the old location. Hot-reload (polling every 60s) will detect the file once it is in the correct path.

---

## 3. Upgrade

**Goal:** Deploy the target version using the backed-up values.

```
  Local Disk                    Helm Repo                  Cluster
  ┌──────────────┐             ┌──────────┐              ┌──────────┐
  │ values.yaml  │──-f────────►│ helm     │──upgrade────►│ New Pod  │
  └──────────────┘             │ upgrade  │              │ (TARGET) │
                               │ --version│              └──────────┘
                               │  $TARGET │
                               └──────────┘
```

> **Important:** Use `-f values.yaml` (not `--reuse-values`) so new chart defaults are merged correctly.

---

## 4. Smoke Test

**Goal:** Verify the upgraded deployment is healthy.

```
  ┌─────────────────────────────────────────────────┐
  │                  SMOKE TEST                      │
  │                                                  │
  │  ✓ deployment rolled out successfully            │
  │  ✓ pod is Ready                                  │
  │  ✓ openab process alive (pgrep)                  │
  │  ✓ no panic/fatal in logs                        │
  │  ✓ "bot connected" in logs                       │
  │  ✓ helm chart version matches TARGET             │
  │  ✓ (if gateway enabled) no gateway disconnect    │
  │    errors in logs; verify Cloudflare tunnel URL  │
  │    is still reachable and update values.yaml if  │
  │    the URL has rotated                           │
  │  ✓ (if usercron enabled) cronjob.toml loaded     │
  │    — check for "loaded usercron jobs" in logs;   │
  │    if you see "no cronjobs yet" instead, verify  │
  │    file is at $HOME/.openab/cronjob.toml         │
  │                                                  │
  │  ALL PASS ──► proceed to 6. CLEANUP            │
  │  ANY FAIL ──► proceed to 5. ROLLBACK             │
  └─────────────────────────────────────────────────┘
```

---

## 5. Rollback

**Goal:** Restore the previous working state — uninstall, fresh install, restore data.

```
  Step ①  Uninstall failed deployment
  ┌──────────┐
  │ helm     │──► release gone
  │ uninstall│──► chart PVCs are retained
  └────┬─────┘
       ▼
  Step ②  Reinstall previous version
  ┌──────────┐    ┌──────────────┐
  │ helm     │◄───│ values.yaml  │
  │ install  │    └──────────────┘
  │ $CURRENT │──► new empty pod running
  └────┬─────┘
       ▼
  Step ③  Restore data
  ┌──────────────┐    kubectl cp     ┌──────────┐
  │ backup/home/ │ ─────────────────►│ Pod $HOME│
  ├──────────────┤    kubectl apply  ├──────────┤
  │ secret.yaml  │ ─────────────────►│ K8s      │
  └──────────────┘                   │ Secret   │
                                     └────┬─────┘
       ▼                                  │
  Step ④  Restart + verify                │
  ┌──────────────────────────────────────┘
  │ rollout restart → wait Ready → pgrep openab
  │
  │ ✅ Rollback complete
  └──────────────────────────────────────────────
```

> **PVC retention:** OpenAB chart-created PVCs are kept on `helm uninstall` to protect auth/session data. Delete a retained PVC manually only when you intentionally want to discard that state. `persistence.existingClaim` PVCs are owned outside the chart and are never created or deleted by OpenAB.
>
> **Upgrade path:** Existing installations will gain the `helm.sh/resource-policy: keep` annotation on their PVCs upon the next `helm upgrade`. This is an additive-only change — it does not alter runtime behavior and only takes effect on a subsequent `helm uninstall`.

---

## 6. Cleanup (post-upgrade)

**Goal:** After a successful upgrade, evaluate old backups and let the user decide what to keep.

> **[AI Instruction]** This phase is advisory. List what you find and ask the user — do NOT auto-delete anything.

```
  Local Disk
  ┌──────────────────────────────────────────────┐
  │  ~/openab-backups/                           │
  │  ├── 0.7.5_20260401/  (1.2 GB)              │
  │  ├── 0.7.6_20260415/  (1.4 GB)              │
  │  └── 0.7.7_20260501/  (1.5 GB)  ◄── current │
  └──────────────────────────────────────────────┘

  Step ①  List all backup directories with size and date
  Step ②  Show total disk usage
  Step ③  Ask the user which backups to remove (if any)
```

**What to report:**

| Item | Command |
|------|---------|
| Backup dirs | `du -sh ~/openab-backups/*/` |
| Total size | `du -sh ~/openab-backups/` |

**Then ask the user:**
- Which old backups to delete (if any)
- Whether to keep the most recent N backups as a policy going forward
- Recommend keeping at least the latest stable backup as a safety net

**Do NOT** delete anything without explicit user confirmation.

---

## Quick Reference

| Action | Key info |
|--------|----------|
| Release name | `helm list \| grep openab` |
| Pod selector | `app.kubernetes.io/instance=$RELEASE,app.kubernetes.io/component=kiro` |
| Check logs | `kubectl logs deployment/${RELEASE}-kiro --tail=50` |
| Restart pod | `kubectl rollout restart deployment/${RELEASE}-kiro` |
| Auth kiro-cli | `kubectl exec -it deployment/${RELEASE}-kiro -- kiro-cli login --use-device-flow` |
