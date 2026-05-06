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
command = "opencode"
args = ["acp"]
working_dir = "/home/node"
```

## Authentication

```bash
kubectl exec -it deployment/openab-opencode -- opencode auth login
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

## Notes

- **Tool authorization**: OpenCode handles tool authorization internally and never emits `session/request_permission` — all tools run without user confirmation, equivalent to `--trust-all-tools` on other backends.
- **Model selection**: Set the default model via `opencode.json` in the working directory using the `provider/model` format (e.g. `ollama-cloud/gemini-3-flash-preview`).
- **Frequent releases**: OpenCode releases very frequently (often daily). The pinned version in `Dockerfile.opencode` should be bumped via a dedicated PR when an update is needed.
