# Project Screening CronJob

This CronJob performs a one-shot screening pass every 30 minutes:

1. Query the OpenAB GitHub Project `Incoming` lane
2. Move the first matching item into `PR-Screening`
3. Generate a Codex screening report
4. Emit the report to the job logs

The job is intentionally stateless:

- GitHub auth comes from `GH_TOKEN`
- Codex auth comes from a mounted `auth.json` copied from `~/.codex/auth.json`
- Discord delivery uses `DISCORD_BOT_TOKEN` from the existing `openab-kiro-codex` secret
- scripts and prompt live in a mounted ConfigMap
- no shared agent PVC is required

## Why This Shape

Do not reuse the long-lived codex pod's home directory or PVC for screening jobs.

That pattern is fragile because:

- the running codex agent already owns its PVC
- CronJob pods should not depend on an interactive device-login state
- ephemeral jobs are easier to reason about when auth is secret-driven

This design follows the stronger parts of OpenClaw and Hermes:

- scheduler outside the model runtime
- isolated execution per run
- explicit credentials and prompt construction
- no always-on sleeper process

## Required Secrets

The GitHub token should include at least the scopes needed to read and update GitHub Projects:

- `project`
- `repo`
- `read:org`

The Codex auth file should be copied from a ChatGPT-authenticated Codex session:

```bash
cat ~/.codex/auth.json
```

The CronJob copies that file into its writable temp home before running `codex exec`.

If you want the report posted back to Discord as a new thread, make sure the existing `openab-kiro-codex` secret already contains `discord-bot-token`. The job will:

1. post a starter message in the target channel
2. create a thread from that message
3. send the screening report into that thread

This repo now sets an explicit override to the parent review channel `1494378525640097921`.

## Raw Kubernetes Apply

Apply these files:

```bash
kubectl apply -f k8s/project-screening-secret.yaml
kubectl apply -f k8s/project-screening-configmap.yaml
kubectl apply -f k8s/project-screening-cronjob.yaml
```

Inspect the most recent run:

```bash
kubectl get jobs --sort-by=.metadata.creationTimestamp
kubectl logs job/<latest-job-name>
```

## Helm

Enable with values like:

```yaml
projectScreening:
  enabled: true
  schedule: "*/30 * * * *"
  image: ghcr.io/openabdev/openab-codex:latest
  githubToken: "<token with project scope>"
  codexAuthJson: |
    PASTE_THE_CONTENTS_OF_YOUR__HOME__CODEX__AUTH_JSON_HERE
  discordReport:
    enabled: true
    secretName: "openab-kiro-codex"
    secretKey: "discord-bot-token"
    channelId: "1494378525640097921"
  senderContextJson: '{"schema":"openab.sender.v1","sender_id":"196299853884686336","sender_name":"mrshroom69","display_name":"mrshroom69","channel":"discord","channel_id":"1494398610173853816","is_bot":false}'
  project:
    owner: openabdev
    number: 1
    incomingStatus: Incoming
    screeningStatus: PR-Screening
```

## Current Limitation

This app pod cannot create the CronJob directly from inside the cluster right now because its service account lacks `get/create` permissions on `batch/cronjobs`.

The manifests in this repo are the recommended fix. Apply them from a cluster-admin context or through your normal Helm release pipeline.
