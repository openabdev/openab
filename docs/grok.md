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
  --set agents.grok.image=ghcr.io/openabdev/openab-grok:latest \
  --set agents.grok.command=grok \
  --set-string 'agents.grok.args[0]=agent' \
  --set-string 'agents.grok.args[1]=stdio' \
  --set agents.grok.workingDir=/home/agent
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

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

## Comparison with Hermes

| Property | `Dockerfile.grok` | `Dockerfile.hermes` |
|----------|-------------------|---------------------|
| Provider | xAI Grok only | xAI + 30 others via Nous gateway |
| ACP | Native (`grok agent stdio`) | Via `hermes-acp` wrapper |
| Headless auth | API key env or device-code | Loopback OAuth (needs port-forward / ECS curl trick) |
| Supply chain | xAI only | xAI + Nous Research install script |
| Image size | Smaller (single static binary, no Python venv) | Larger (Python + uv + ffmpeg) |

Pick `Dockerfile.grok` if Grok is the only model you need. Pick `Dockerfile.hermes` if you want multi-provider switching or fallback chains.
