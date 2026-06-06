# Pi Coding Agent

OpenAB supports the [Pi coding agent](https://github.com/earendil-works/pi-coding-agent) via the `pi-acp` adapter — a Node.js bridge that translates ACP JSON-RPC into Pi CLI invocations.

## Advantages Over Other Native Coding Agents

Pi is a native coding agent that supports subscription-based authentication (like Codex, Cloud Code, and GitHub Copilot). Key advantages:

### No Auth Proxy Required

Pi natively supports Anthropic (Claude Pro/Max) and ChatGPT Plus/Pro subscriptions via OAuth. Unlike agents that require an `openab-auth-proxy` sidecar for subscription forwarding, Pi handles subscription auth directly — reducing deployment complexity and eliminating a moving part.

| Agent | Subscription Auth | Auth Proxy Needed? |
|-------|------------------|--------------------|
| Pi | Native OAuth (`pi /login`) | ❌ No |
| Codex | Native device flow | ❌ No |
| GitHub Copilot | Native device flow | ❌ No |
| Claude Code | Native OAuth | ❌ No |
| Kiro | Native OAuth | ❌ No |

### Minimal Tool Surface (Maximum Context Window)

Pi exposes only 4 core tools: `read`, `write`, `edit`, `bash`. Combined with a tiny system prompt, this drastically reduces prompt overhead and maximizes the available context window for actual project source files.

| Agent | Tool Count | System Prompt Size |
|-------|-----------|-------------------|
| Pi | 4 | Minimal |
| Claude Code | 10+ | Large |
| Codex | 8+ | Medium |
| Copilot | 10+ | Large |

### Multi-Model Support

Pi is model-agnostic and supports 15+ LLM providers. Developers can switch models mid-session without restarting the agent or changing configuration.

Supported providers include:
- Anthropic (Claude) — via subscription or API key
- OpenAI (GPT/Codex) — via subscription or API key
- Google (Gemini) — via API key
- Any OpenAI-compatible endpoint

### Branching Session Trees

Pi saves session history as trees, enabling clean branching of code exploration. This allows developers to explore multiple approaches from a single decision point without losing context.

## Configuration

```toml
[agent]
# command defaults from OPENAB_AGENT_COMMAND="openab-agent"
# working_dir = "/home/node"  # optional — defaults to $HOME
```

## Docker

```bash
docker build -f Dockerfile.pi -t openab-pi:latest .
```

## Helm

```yaml
agents:
  pi:
    discord:
      enabled: true
      allowedChannels:
        - "YOUR_CHANNEL_ID"
    command: pi-acp
    workingDir: /home/node
    image: "ghcr.io/openabdev/openab-pi:latest"
```

## Authentication

```bash
kubectl exec -it deployment/openab-pi -- pi
# Once inside the interactive interface, type /login to authenticate
```

Supported authentication methods:

| Provider | Auth Method | Subscription |
|----------|-------------|-------------|
| Anthropic (Claude Pro/Max) | OAuth via `pi /login` | Claude subscription |
| ChatGPT Plus/Pro | OAuth via `pi /login` | ChatGPT subscription |
| Any API key provider | `env = { OPENAI_API_KEY = "..." }` | Pay-per-token |

## Local OpenAI-Compatible Vision Models

OpenAB can pass inbound image attachments to Pi as ACP image content blocks, but Pi must also select a model declared as image-capable. For custom OpenAI-compatible providers, add `input: ["text", "image"]` to the model entry in `~/.pi/agent/models.json`.

See [Local OpenAI-Compatible Vision Models](local-vision-models.md#pi-configuration) for the `llama-server` setup, `models.json` example, and local vision pitfalls.

## Steering Files

Pi reads steering files in this order:

1. `.pi/SYSTEM.md` — replaces the default system prompt entirely
2. `.pi/APPEND_SYSTEM.md` — appends to the default system prompt
3. `AGENTS.md` — loaded hierarchically (project root → global) for context injection

Place your steering instructions in `/home/node/AGENTS.md` or `/home/node/.pi/APPEND_SYSTEM.md`.

## Persisted Paths (PVC)

| Path | Contents |
|------|----------|
| `/home/node/.pi/` | Pi configuration and auth tokens |
| `/home/node/.pi/sessions/` | Session history trees |

## Limitations

- **No streaming**: `pi-acp` returns the full response at once; streamed output is sent as a single `agent_message_chunk` notification.
- **Cancel is best-effort**: Pi CLI runs to completion; `session/cancel` may not interrupt mid-generation.
