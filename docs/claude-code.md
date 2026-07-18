# Claude Code

Claude Code uses the [@agentclientprotocol/claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp) adapter for ACP support.

## Docker Image

```bash
docker build -f Dockerfile.claude -t openab-claude:latest .
```

The image installs `@agentclientprotocol/claude-agent-acp` and `@anthropic-ai/claude-code` globally via npm.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.claude.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.claude.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.claude.command=claude-agent-acp \
  --set agents.claude.workingDir=/home/node \
  --set image.tag=beta
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

### Image Tag

Use `--set image.tag=<version>` to set the image version globally.
The chart auto-appends `-<agent>` to produce the final tag (see [image-tags.md](image-tags.md) for full details).

| Tag | Resolves to | Description |
|-----|-------------|-------------|
| `beta` | `beta-claude` | Floating beta channel (latest pre-release) |
| `0.9.0-beta.2` | `0.9.0-beta.2-claude` | Pinned to exact version |
| `0.9` | `0.9-claude` | Latest patch in minor (floating) |
| `stable` | `stable-claude` | Floating stable channel |

To override a single agent's image instead of the global tag:
```bash
--set agents.claude.image=ghcr.io/openabdev/openab:beta-claude
```

> ⚠️ There is no `latest` tag. Use `beta` or `stable`, or pin to an exact version.

## Manual config.toml

```toml
[agent]
# command defaults from OPENAB_AGENT_COMMAND="claude-agent-acp"
# Only override if you need non-default behavior
```

## Authentication

Sign in interactively using the OAuth device flow. Credentials are stored on disk (persisted via PVC across pod restarts):

```bash
kubectl exec -it deployment/openab-claude -- sh -c "$OPENAB_AGENT_AUTH_COMMAND"
```

After authenticating, restart the pod so the bot process loads the new credentials:

```bash
kubectl rollout restart deployment/openab-claude
```

> **Note:** `claude setup-token` is a different command — it generates a long-lived token for CI/scripts and prints it without saving locally. For container-based deployments, `claude auth login` is the correct approach as it persists credentials to the filesystem.

## Troubleshooting

### `Login failed: Request failed with status code 400` at "Paste code here if prompted"

The `Paste code here if prompted >` prompt is Claude Code's manual OAuth flow — the
claude-agent-acp adapter delegates authentication to the underlying `claude` binary,
so the 400 comes from the token exchange with Anthropic's OAuth server, not from the
ACP adapter or OpenAB.

Check these in order:

1. **Stale or partial code (most common).** The auth code is single-use and
   short-lived. Get a fresh code and paste the **entire** string, including
   everything after the `#` (the format is `code#state`).
2. **Old Claude Code version.** Claude Code v2.1.105–v2.1.107 had a bracketed-paste
   regression that truncated the pasted code, causing the token exchange to fail
   with 400. Fixed in v2.1.108. Upgrade inside the container:
   ```bash
   npm install -g @anthropic-ai/claude-code@latest @agentclientprotocol/claude-agent-acp@latest
   ```
   Or use a newer image tag (see [Image Tag](#image-tag)).
3. **Anthropic OAuth outage.** Waves of OAuth 400/500 errors have been server-side
   incidents (see [anthropics/claude-code#10719](https://github.com/anthropics/claude-code/issues/10719)).
   Check [status.claude.com](https://status.claude.com) before debugging further.

Always authenticate interactively via `kubectl exec` (as shown above) rather than
through the ACP stdio session, then restart the pod to load the new credentials.
