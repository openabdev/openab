# Hermes Agent

[Hermes Agent](https://github.com/NousResearch/hermes-agent) by Nous Research supports ACP natively via the `hermes acp` subcommand (or the `hermes-acp` binary).

Hermes acts as a multi-provider inference gateway — it handles OAuth token lifecycle, credential storage, and provider routing so OAB agents don't need to manage auth directly.

## Docker Image

```bash
docker build -f Dockerfile.hermes -t openab-hermes:latest .
```

The image installs Hermes Agent via the official install script.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.hermes.discord.enabled=true \
  --set agents.hermes.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.hermes.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.hermes.image=ghcr.io/openabdev/openab-hermes:latest \
  --set agents.hermes.command=hermes-acp \
  --set agents.hermes.workingDir=/home/agent
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

## Manual config.toml

```toml
[agent]
# command defaults from OPENAB_AGENT_COMMAND="hermes-acp"
# working_dir = "/home/agent"  # optional — defaults to $HOME
```

## Authentication

Hermes supports 30+ providers. Authenticate inside the pod:

```bash
kubectl exec -it <pod> -- hermes auth add xai-oauth    # xAI Grok (SuperGrok $30/mo)
kubectl exec -it <pod> -- hermes auth add nous         # Nous Portal
kubectl exec -it <pod> -- hermes model                 # Interactive provider picker
```

### xAI Grok OAuth (Recommended)

> ⚠️ **Requires an active [SuperGrok paid subscription](https://x.ai/grok) ($30/mo).** Auth will succeed without one, but the API silently returns empty responses — the bot appears to work but never replies.

xAI Grok OAuth uses a loopback redirect flow — the callback listener binds `127.0.0.1:56121` inside the pod/container.

#### Option A: Kubernetes (port-forward)

```bash
# Terminal 1: port-forward
kubectl port-forward deployment/<your-deployment> 56121:56121

# Terminal 2: run auth
kubectl exec -it deployment/<your-deployment> -- hermes auth add xai-oauth --no-browser
```

1. Copy the printed authorize URL → open in your local browser
2. Approve access on accounts.x.ai
3. Browser redirects to `127.0.0.1:56121/callback` → port-forward delivers it to the pod
4. Terminal shows `Added xai-oauth OAuth credential #1: "xai-oauth-oauth-1"`

#### Option B: ECS / Remote (curl-the-callback)

ECS Fargate doesn't support port-forward. Use two exec sessions instead:

```bash
# Terminal 1: start the auth listener
aws ecs execute-command --cluster openab --task <task-id> --container openab --interactive --command bash
hermes auth add xai-oauth --no-browser
# → prints authorize URL with &state=XXXXX in it
# → "Waiting for callback on http://127.0.0.1:56121/callback"
```

Open the authorize URL in your browser and approve. The browser will redirect to
`http://127.0.0.1:56121/callback?code=...` and fail ("Could not establish connection").
**Copy the `code` value** from the page or URL bar. The `state` value comes from the
authorize URL printed in Terminal 1.

```bash
# Terminal 2: exec into the SAME container
aws ecs execute-command --cluster openab --task <task-id> --container openab --interactive --command bash
curl "http://127.0.0.1:56121/callback?code=<THE_CODE>&state=<THE_STATE>"
```

Terminal 1 should print:
```
Added xai-oauth OAuth credential #1: "xai-oauth-oauth-1"
```

> ⚠️ The code expires in seconds — be fast. If you get `invalid_grant`, re-run `hermes auth add` and try again.

#### After auth: set the default model

```bash
hermes config set model.provider xai-oauth
hermes config set model.default grok-4.3
```

#### Fix file ownership (important for exec-based auth)

When running auth/config commands via `kubectl exec` or ECS exec (which runs as root),
fix ownership so the `agent` user can read the files:

```bash
chown -R agent:agent /home/agent/.hermes/
```

### Providers That Don't Need Port-Forward

| Provider | Auth Method |
|----------|-------------|
| Anthropic (Claude Pro/Max) | Paste-the-code flow |
| OpenAI Codex (ChatGPT Plus/Pro) | Device code flow |
| MiniMax, Nous Portal | Device code flow |
| xAI Grok, Spotify | Loopback OAuth (port-forward required) |

### Supported Providers (via OAuth)

| Provider | Auth Command | Cost Model |
|----------|-------------|------------|
| xAI Grok | `hermes auth add xai-oauth` | SuperGrok subscription ($30/mo) |
| OpenAI Codex | `hermes model` → OpenAI Codex | ChatGPT subscription |
| GitHub Copilot | `hermes model` → GitHub Copilot | Copilot subscription |
| Google Gemini | `hermes model` → Google Gemini (OAuth) | Free tier available |
| Anthropic | `hermes model` → Anthropic | Claude Max + extra credits |
| Nous Portal | `hermes auth add nous` | Nous subscription |

### Supported Providers (via API Key)

Any provider can also be configured with an API key via environment variables:

```toml
[agent]
# command defaults from OPENAB_AGENT_COMMAND="hermes-acp"
# working_dir = "/home/agent"  # optional — defaults to $HOME
env = { XAI_API_KEY = "${XAI_API_KEY}" }
```

## Provider Switching

Switch providers without restarting the pod:

```bash
kubectl exec -it <pod> -- hermes model
```

## Credential Persistence

Hermes stores OAuth tokens in `~/.hermes/`. The OpenAB Helm chart's default persistence covers this automatically (PVC mounted at `workingDir`).

If deploying manually (without the Helm chart), mount persistent storage at `/home/agent` or `/home/agent/.hermes`:

```yaml
volumes:
  - name: hermes-credentials
    persistentVolumeClaim:
      claimName: hermes-credentials-pvc
volumeMounts:
  - name: hermes-credentials
    mountPath: /home/agent/.hermes
```

## Advantages

- **Cost**: SuperGrok $30/mo flat rate vs pay-per-token API pricing
- **Multi-provider**: 30+ providers accessible through one agent
- **Zero auth complexity**: Hermes handles OAuth + token refresh
- **Multi-modal**: TTS, image gen, video gen via the same OAuth token
- **Fallback chains**: Auto-switch providers on failure
