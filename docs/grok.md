# Grok Build (xAI)

[Grok Build](https://x.ai/news/grok-build-cli) is xAI's official coding agent CLI. It speaks ACP natively via `grok agent stdio` — no wrapper required.

## Docker Image

```bash
docker build -f Dockerfile.grok -t openab-grok:latest .
```

The image pulls a pinned `grok` binary from xAI's public artifacts bucket and verifies its SHA256 checksum. Bump `GROK_VERSION`, `GROK_SHA256_AMD64`, and `GROK_SHA256_ARM64` in `Dockerfile.grok` to upgrade.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.grok.discord.enabled=true \
  --set agents.grok.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.grok.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.grok.command=grok \
  --set-string 'agents.grok.args[0]=agent' \
  --set-string 'agents.grok.args[1]=stdio' \
  --set agents.grok.workingDir=/home/agent \
  --set image.tag=beta
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

### Image Tag

Use `--set image.tag=<version>` to set the image version globally.
The chart auto-appends `-<agent>` to produce the final tag (see [image-tags.md](image-tags.md) for full details).

| Tag | Resolves to | Description |
|-----|-------------|-------------|
| `beta` | `beta-grok` | Floating beta channel (latest pre-release) |
| `0.9.0-beta.2` | `0.9.0-beta.2-grok` | Pinned to exact version |
| `0.9` | `0.9-grok` | Latest patch in minor (floating) |
| `stable` | `stable-grok` | Floating stable channel |

To override a single agent's image instead of the global tag:
```bash
--set agents.grok.image=ghcr.io/openabdev/openab:beta-grok
```

> ⚠️ There is no `latest` tag. Use `beta` or `stable`, or pin to an exact version.

## Manual config.toml

```toml
[agent]
# command and args default from OPENAB_AGENT_COMMAND="grok agent stdio"
# Only override if you need non-default behavior:
# args = ["agent", "stdio", "--model", "grok-4.3"]
```

## Authentication

Grok Build supports three credential sources. Pick whichever fits your deployment.

### Option A: API key (simplest, recommended for CI / bot deployments)

Set the environment variable in the pod / task definition:

```bash
export GROK_CODE_XAI_API_KEY="xai-..."
```

Get a key from <https://console.x.ai/>. No interactive login needed.

> ⚠️ **Security**: env vars listed under `[agent].env` are visible to the agent and can be leaked via prompt injection. Prefer mounting them via the platform's secret manager.

### Option B: Device-code OAuth (for SuperGrok subscriptions)

If you want to use a SuperGrok subscription instead of pay-per-token API billing:

```bash
kubectl exec -it <pod> -- sh -c "$OPENAB_AGENT_AUTH_COMMAND"
```

The CLI prints a short code and URL — open the URL on any device, enter the code, approve. The token is stored at `~/.grok/auth.json` inside the container.

This works in any headless environment (K8s exec, ECS exec, plain SSH) **without port-forwarding** — unlike loopback OAuth flows.

### Option C: Enterprise deployment key

```bash
export GROK_DEPLOYMENT_KEY="..."
```

A deployment key takes precedence over `auth.json`. The CLI fetches managed config from `cli-chat-proxy.grok.com/v1/deployment/config` on startup. Available to xAI enterprise customers; contact xAI sales for details.

## Credential Persistence

`grok login` stores OAuth credentials at `~/.grok/auth.json` and runtime config at `~/.grok/config.toml`. The OpenAB Helm chart's default persistence covers `workingDir` automatically (PVC mounted at `/home/agent`).

If deploying manually, mount persistent storage at `/home/agent/.grok`:

```yaml
volumes:
  - name: grok-credentials
    persistentVolumeClaim:
      claimName: grok-credentials-pvc
volumeMounts:
  - name: grok-credentials
    mountPath: /home/agent/.grok
```

API-key-only deployments don't need persistence.

## Model Selection

The default model is whichever Grok Build CLI selects (currently `grok-code-fast-1` for the free tier; `grok-4.3` family for SuperGrok). To override:

```toml
[agent]
# Override args to select a specific model (command defaults from OPENAB_AGENT_COMMAND)
args = ["agent", "stdio", "--model", "grok-4.3"]
```

List available models inside the pod:

```bash
kubectl exec -it <pod> -- grok models
```

## Updating

```bash
# Inside the container (one-shot upgrade):
kubectl exec -it <pod> -- grok update

# Or rebuild the image with a new pinned version:
docker build -f Dockerfile.grok \
  --build-arg GROK_VERSION=0.1.220 \
  --build-arg GROK_SHA256_AMD64=... \
  --build-arg GROK_SHA256_ARM64=... \
  -t openab-grok:latest .
```
