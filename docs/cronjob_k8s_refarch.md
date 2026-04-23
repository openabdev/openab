# Kubernetes CronJob Reference Architecture

This document is a reference architecture for how we set up the project-screening CronJob around `codex exec`, GitHub Projects, and Discord delivery.

It is not meant to be framed as the one official OpenAB recommendation. The intent is narrower: when someone asks how to do scheduled screening work in Kubernetes, we can hand this document to their Kiro or Codex-style agent as a concrete starting point and let that agent adapt the pattern to their environment.

## ASCII Flow

```text
GitHub Project Board
  Incoming
     |
     v
Kubernetes CronJob
  schedule: every 30 minutes
  concurrencyPolicy: Forbid
     |
     v
Ephemeral Job Pod
  image: ghcr.io/openabdev/openab-codex:latest
  command: bash /opt/openab-project-screening/screen_once.sh
     |
     +--> read GitHub Project state via gh
     +--> claim first Incoming item
     +--> build prompt from PR/issue metadata
     +--> run codex exec
     +--> post summary to Discord
     +--> create Discord thread
     +--> post full report
     |
     v
Project Board
  PR-Screening
     |
     v
Human or agent follow-up
```

## What This Document Covers

We deliberately chose a Kubernetes `CronJob` instead of:

- installing `cron` inside the app container
- running an always-on sleep loop in the main pod
- reusing a long-lived ACP session for scheduled screening

This shape fits Kubernetes better:

- the scheduler is owned by the cluster
- each run gets a fresh pod
- failures are isolated per run
- logs are attached to each job
- `concurrencyPolicy: Forbid` prevents overlapping claimers

## Credential Model

The job is intentionally stateless.

- `GH_TOKEN` comes from the `openab-project-screening` Secret
- `auth.json` comes from the same Secret and is copied into `$HOME/.codex/auth.json`
- `DISCORD_BOT_TOKEN` comes from the existing `openab-kiro-codex` Secret
- the script and prompt are mounted from a ConfigMap
- the pod uses `/tmp` via `emptyDir`
- no shared PVC is required

This avoids coupling the scheduled workflow to a long-lived interactive pod.

If another team wants the same behavior, they should treat the specific secret names, project names, and channel IDs in this document as implementation examples and swap in their own values.

## CronJob Manifest

The CronJob shape we use looks like this:

```yaml
apiVersion: batch/v1
kind: CronJob
metadata:
  name: openab-project-screening
spec:
  schedule: "*/30 * * * *"
  concurrencyPolicy: Forbid
  jobTemplate:
    spec:
      # No retries — each run is one-shot. A failure should surface in job
      # logs rather than silently re-claiming the same item.
      backoffLimit: 0
      template:
        spec:
          restartPolicy: Never
          containers:
            - name: project-screening
              # Pin to a specific tag in production (e.g. :0.8.0) to ensure
              # reproducible runs. :latest is used here for illustration only.
              image: ghcr.io/openabdev/openab-codex:latest
              command:
                - bash
                - /opt/openab-project-screening/screen_once.sh
              env:
                - name: GH_TOKEN
                  valueFrom:
                    secretKeyRef:
                      name: openab-project-screening
                      key: gh-token
                - name: CODEX_AUTH_JSON_SOURCE
                  value: /opt/openab-project-screening-auth/auth.json
                - name: DISCORD_BOT_TOKEN
                  valueFrom:
                    secretKeyRef:
                      name: openab-kiro-codex
                      key: discord-bot-token
                - name: DISCORD_REPORT_CHANNEL_ID
                  value: "<your_channel_id>"
```

Security settings were kept tight on purpose:

```yaml
securityContext:
  runAsNonRoot: true
  runAsUser: 1000
  runAsGroup: 1000
  fsGroup: 1000
  seccompProfile:
    type: RuntimeDefault
```

Container hardening:

```yaml
securityContext:
  allowPrivilegeEscalation: false
  readOnlyRootFilesystem: true
  capabilities:
    drop:
      - ALL
```

## ConfigMap And Script

The mounted ConfigMap carries:

- `screen_once.sh`
- `screening_prompt.md`

The core runtime flow in `screen_once.sh` is:

```bash
item_id="$(incoming_item_jq '.items[0].id // empty')"

if [[ -z "$item_id" ]]; then
  log "no Incoming items found"
  exit 0
fi

gh project item-edit \
  --id "$item_id" \
  --project-id "$project_id" \
  --field-id "$status_field_id" \
  --single-select-option-id "$screening_option_id" >/dev/null

generate_report "$prompt_file" "$report_file"
post_report_to_discord "$item_number" "$item_title" "$item_url" "$report_file"
```

That gives us the exact one-shot behavior we want:

1. no-op when `Incoming` is empty
2. claim the first item when work exists
3. generate the report once
4. deliver it once
5. exit

## Codex Execution

The report is generated with `codex exec`, not with a long-lived ACP daemon:

```bash
codex exec \
  --skip-git-repo-check \
  --cd "$WORK_DIR" \
  --sandbox read-only \
  --ephemeral \
  --color never \
  --output-last-message "$report_file" \
  - <"$prompt_file" >/dev/null
```

Why `codex exec`:

- this workflow is scheduled and one-shot
- each run should start clean
- we do not need a persistent interactive session
- job logs map naturally to one execution

## Discord Delivery

After the report is generated, the script posts a summary message, creates a thread on that message, and then sends the full report into the thread.

Summary message:

```text
PR Screening - #<number>
<title>
Status: moved to PR-Screening
```

Actual implementation:

```bash
starter_content="🔍 **PR Screening** — [#${item_number}](${item_url})
${item_title}
Status: moved to ${SCREENING_STATUS_NAME}"
```

Thread naming (Node.js helper used by the script):

```javascript
const base = `Screening: #${number}${title ? ` ${title}` : ""}`.trim();
process.stdout.write(base.slice(0, 100) || `Screening: #${number}`);
```

Discord API flow:

```bash
# 1. post summary message
POST /channels/{channel_id}/messages

# 2. create thread on that message
POST /channels/{channel_id}/messages/{message_id}/threads

# 3. post report chunks
POST /channels/{thread_id}/messages
```

The script also retries on Discord `429` rate limits before continuing.

## Secrets

The screening job secret contains:

```yaml
stringData:
  gh-token: "REPLACE_WITH_GITHUB_TOKEN_WITH_PROJECT_SCOPE"
  auth.json: |
    REPLACE_WITH_CONTENTS_OF_CODEX_AUTH_JSON
```

Discord is intentionally not duplicated there. The CronJob reads the bot token from the existing:

```text
Secret name: openab-kiro-codex
Key: discord-bot-token
```

## Raw Kubernetes Install

Create or update the screening secret:

```bash
kubectl -n default create secret generic openab-project-screening \
  --from-literal=gh-token='YOUR_GITHUB_TOKEN_WITH_PROJECT_SCOPE' \
  --from-file=auth.json="$HOME/.codex/auth.json" \
  --dry-run=client -o yaml | kubectl apply -f -
```

Verify the shared Discord token secret exists:

```bash
kubectl -n default get secret openab-kiro-codex
kubectl -n default get secret openab-kiro-codex -o jsonpath='{.data.discord-bot-token}' | grep -q .
```

Apply the ConfigMap and CronJob manifests:

```bash
kubectl -n default apply -f project-screening-configmap.yaml
kubectl -n default apply -f project-screening-cronjob.yaml
```

Run one manual test:

```bash
kubectl -n default create job \
  --from=cronjob/openab-project-screening \
  openab-project-screening-manual-$(date +%s)
```

Inspect the logs:

```bash
LATEST_JOB=$(kubectl -n default get jobs \
  --sort-by=.metadata.creationTimestamp \
  -o jsonpath='{.items[-1:].metadata.name}')

kubectl -n default logs -f job/"$LATEST_JOB"
```

## Helm Values

A Helm chart can wire this under `projectScreening` values like:

> **⚠️ Security note:** `githubToken` and `codexAuthJson` below are shown inline for illustration.
> In practice, supply these via `--set` flags, environment variables, or an external secret manager
> (e.g. Sealed Secrets, External Secrets Operator). **Do not commit credentials to version control.**

```yaml
projectScreening:
  enabled: true
  schedule: "*/30 * * * *"
  # Pin to a specific tag in production (e.g. :0.8.0)
  image: ghcr.io/openabdev/openab-codex:latest
  githubToken: "<token with project scope>"
  codexAuthJson: |
    <contents of ~/.codex/auth.json>
  discordReport:
    enabled: true
    secretName: "openab-kiro-codex"
    secretKey: "discord-bot-token"
    channelId: "<your_channel_id>"
```

## Operational Notes

- This pod cannot install the CronJob from inside itself without broader RBAC.
- The correct install path is a cluster-admin shell or CI/CD pipeline.
- Once the CronJob is live, stop any older in-pod watcher so only one claimer remains.

## Design Summary

The elegant part of this setup is that each concern is separated cleanly:

- Kubernetes owns the schedule
- GitHub Projects remains the source of truth
- `codex exec` is used as a disposable analysis engine
- Discord is only the reporting surface
- the handoff queue is `PR-Screening`

That separation is why this design works well in Kubernetes.

The more opinionated design discussion, including alternatives we considered and why we ultimately chose this route, should live in a separate architecture note. This document is intentionally the operational reference version.
