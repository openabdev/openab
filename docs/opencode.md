# OpenCode

OpenCode supports ACP natively via the `acp` subcommand — no adapter needed.

OpenCode supports [75+ LLM providers](https://opencode.ai/docs/providers/) via the AI SDK, making it the most flexible backend for OpenAB. Users bring their own provider — no separate API keys per backend needed.

```
┌──────────┐  Discord  ┌────────┐ ACP stdio ┌──────────┐   ┌───────────────────┐
│ Discord  │◄────────► │ OpenAB │◄────────► │ OpenCode │──►│  LLM Providers    │
│ Users    │ Gateway   │ (Rust) │ JSON-RPC  │  (ACP)   │   │                   │
└──────────┘           └────────┘           └──────────┘   │ ┌───────────────┐ │
                                                 │         │ │ Ollama Cloud  │ │
                                       opencode.json       │ │ OpenAI        │ │
                                       sets model          │ │ Anthropic     │ │
                                                           │ │ AWS Bedrock   │ │
                                                           │ │ GitHub Copilot│ │
                                                           │ │ Groq          │ │
                                                           │ │ OpenRouter    │ │
                                                           │ │ Ollama (local)│ │
                                                           │ │ 75+ more...   │ │
                                                           │ └───────────────┘ │
                                                           └───────────────────┘
```

## Docker Image

```bash
docker build -f Dockerfile.opencode -t openab-opencode:latest .
```

The image installs `opencode-ai` globally via npm on `node:22-bookworm-slim`.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.opencode.enabled=true \
  --set agents.opencode.command=opencode \
  --set 'agents.opencode.args={acp}' \
  --set agents.opencode.image=ghcr.io/openabdev/openab-opencode:latest \
  --set agents.opencode.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.opencode.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.opencode.workingDir=/home/node \
  --set agents.opencode.pool.maxSessions=3
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

## Manual config.toml

```toml
[agent]
# command and args default from OPENAB_AGENT_COMMAND="opencode acp"
# Only override if you need non-default behavior
```

## Authentication

```bash
kubectl exec -it deployment/openab-opencode -- sh -c "$OPENAB_AGENT_AUTH_COMMAND"
```

Follow the browser OAuth flow, then restart the pod:

```bash
kubectl rollout restart deployment/openab-opencode
```

## Providers

OpenCode supports multiple providers. Add any of them via `opencode auth login`:

- **Ollama Cloud** — free tier available, models like `gemini-3-flash-preview`, `qwen3-coder-next`, `deepseek-v3.2`
- **OpenCode Zen / Go** — tested and verified models provided by the OpenCode team (e.g. `opencode/big-pickle`, `opencode/gpt-5-nano`)
- **OpenAI, Anthropic, AWS Bedrock, GitHub Copilot, Groq, OpenRouter** — and [75+ more](https://opencode.ai/docs/providers/)

To list all available models across configured providers:

```bash
kubectl exec deployment/openab-opencode -- opencode models
```

## Local OpenAI-Compatible Vision Models

OpenAB can pass inbound image attachments to OpenCode as ACP image content blocks, but OpenCode must also select a model whose metadata declares image input support. For custom providers, that means `modalities.input: ["text", "image"]` in `opencode.json`.

See [Local OpenAI-Compatible Vision Models](local-vision-models.md#opencode-configuration) for the `llama-server` setup, `opencode.json` example, and local vision pitfalls.

## Example: Ollama Cloud with gemini-3-flash-preview

### 1. Authenticate Ollama Cloud

```bash
kubectl exec -it deployment/openab-opencode -- opencode auth login -p "ollama cloud"
```

### 2. Set default model

Create `opencode.json` in the working directory (`/home/node`). OpenCode reads it as project-level config:

```bash
kubectl exec deployment/openab-opencode -- sh -c \
  'echo "{\"model\": \"ollama-cloud/gemini-3-flash-preview\"}" > /home/node/opencode.json'
```

This file is on the PVC and persists across restarts.

### 3. Restart to pick up config

```bash
kubectl rollout restart deployment/openab-opencode
```

### 4. Verify

```bash
kubectl logs deployment/openab-opencode --tail=5
# Should show: discord bot connected
```

`@mention` the bot in your Discord channel to start chatting.

## Example: xAI Grok with SuperGrok OAuth

### 1. Create the auth directory

OpenCode stores credentials at `~/.local/share/opencode/auth.json`. The directory must exist before login:

```bash
kubectl exec deployment/openab-opencode -- mkdir -p /home/node/.local/share/opencode
```

### 2. Authenticate xAI (device-code flow)

```bash
kubectl exec -it deployment/openab-opencode -- opencode auth login -p xai
```

Select **"xAI Grok OAuth (Headless / Remote / VPS)"**. The CLI prints a URL and a short code:

```
Open https://x.ai/device on any device and enter code: ABCD-1234
```

Open the URL on any device with a browser, enter the code, and approve.

### 3. Verify auth file was created

```bash
kubectl exec deployment/openab-opencode -- cat /home/node/.local/share/opencode/auth.json
```

You should see a JSON object with `xai` credentials.

### 4. Set default model

Create `opencode.json` in the working directory (`/home/node`):

```bash
kubectl exec -it deployment/openab-opencode -- bash -c 'cat > /home/node/opencode.json << "EOF"
{
  "$schema": "https://opencode.ai/config.json",
  "model": "xai/grok-4.3"
}
EOF'
```

### 5. Restart to pick up config

```bash
kubectl rollout restart deployment/openab-opencode
```

> **Important:** Do NOT set a custom `baseURL` or provider override for xAI. The built-in xAI provider handles routing correctly. A stale `~/.config/opencode/opencode.json` with `baseURL: "http://localhost:9090/v1"` (from xai-proxy setups) will break xAI — delete it if present.

## Notes

- **Tool authorization**: OpenCode handles tool authorization internally and never emits `session/request_permission` — all tools run without user confirmation, equivalent to `--trust-all-tools` on other backends.
- **Model selection**: Set the default model via `opencode.json` in the working directory using the `provider/model` format (e.g. `ollama-cloud/gemini-3-flash-preview`).
- **Frequent releases**: OpenCode releases very frequently (often daily). The pinned version in `Dockerfile.opencode` should be bumped via a dedicated PR when an update is needed.
