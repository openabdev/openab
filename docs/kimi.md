# Kimi Code CLI — Agent Backend Guide

[Kimi Code CLI](https://github.com/MoonshotAI/kimi-code) is Moonshot AI's coding agent. It supports the [Agent Client Protocol (ACP)](https://agentclientprotocol.com/) natively through `kimi acp`, so OpenAB can launch it directly over stdio without an adapter.

## Prerequisites

- Kimi Code CLI installed, or the OpenAB Kimi image
- A Kimi Code account or a Moonshot/Kimi Platform API key
- Persistent storage for Kimi credentials and configuration in containerized deployments

## Configuration

```toml
[agent]
command = "kimi"
args = ["acp"]
working_dir = "/home/node"
```

`kimi acp` communicates with OpenAB using JSON-RPC over stdin/stdout. Kimi's logs are written to stderr, keeping the ACP stream clean.

## Authentication

Kimi Code uses an interactive login flow. In a running container, start the CLI and enter `/login`:

```bash
kubectl exec -it deployment/openab-kimi -- kimi
# Select Kimi Code OAuth or Kimi Platform API key, then follow the prompts.
```

Credentials and configuration are stored under `~/.kimi-code/`. The Kimi image uses `/home/node` as `HOME`, so enable persistence for production deployments. For non-container usage, run `kimi` once in the desired home directory and complete `/login` before starting OpenAB.

The chart/image exposes the same helper used by other backends:

```bash
kubectl exec -it deployment/openab-kimi -- sh -c "$OPENAB_AGENT_AUTH_COMMAND"
```

Then enter `/login` in the interactive Kimi session.

## Docker

Build the standalone image:

```bash
docker build -f Dockerfile.kimi -t openab-kimi .
```

Build the unified image target:

```bash
docker build --target kimi -f Dockerfile.unified -t openab-kimi .
```

The image pins `@moonshot-ai/kimi-code` to a specific version. Update `KIMI_CODE_VERSION` in the Dockerfiles through a dedicated version-bump change.

## Helm

Use the Kimi image variant and provide the raw OpenAB configuration through `configToml` or `configUrl`:

```yaml
agents:
  kimi:
    enabled: true
    image: ghcr.io/openabdev/openab:beta-kimi
    workingDir: /home/node
    configToml: |
      [discord]
      allowed_channels = ["YOUR_CHANNEL_ID"]

      [agent]
      command = "kimi"
      args = ["acp"]
      working_dir = "/home/node"
```

For a complete deployment, inject `DISCORD_BOT_TOKEN` through a Secret and enable a persistent volume. Discord IDs should be passed with Helm's `--set-string` option when setting them on the command line.

## Model providers

Kimi Code can use its managed Kimi Code OAuth service or a Kimi/Moonshot API key. It also supports configuring Anthropic, OpenAI-compatible, OpenAI Responses, Google Gemini, and Vertex AI providers in `~/.kimi-code/config.toml`. See the [Kimi providers and models reference](https://moonshotai.github.io/kimi-code/en/configuration/providers).

Example Kimi provider configuration:

```toml
[providers.kimi]
type = "kimi"
base_url = "https://api.moonshot.ai/v1"

[providers.kimi.env]
KIMI_API_KEY = "sk-..."
```

Note that Kimi Code reads provider credentials only from `config.toml` — the provider's `api_key` field or its `[providers.<name>.env]` subtable — and does not fall back to shell environment variables. Supply the key through the agent's `configToml` (or the Kimi credential store) rather than relying on OpenAB's `[agent].inherit_env`/`[agent].env`, which control the broker's child-process environment and are not read by Kimi for provider credentials.

## ACP capabilities

Kimi Code's ACP adapter supports normal session creation/loading, prompts, tool calls, permission handling, image input, and MCP forwarding. Audio prompts are not currently supported by Kimi's ACP adapter.

## Troubleshooting

- **The process exits immediately:** run `kimi` interactively and complete `/login` first.
- **`kimi` cannot be found:** use the Kimi image or set `[agent].command` to the absolute path returned by `which kimi`.
- **Authentication disappears after restart:** enable the agent PVC and verify that `HOME=/home/node` is writable.
- **No response from ACP:** run `kimi acp` directly and verify that it waits for JSON-RPC input rather than printing an interactive banner.
