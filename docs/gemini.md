# Gemini CLI

Gemini CLI supports ACP natively via the `--acp` flag — no adapter needed.

> **Migration notice:** Since June 18, 2026, Gemini CLI no longer serves Google AI
> Pro, Google AI Ultra, or free-tier individual accounts. Google recommends
> migrating those users to Antigravity CLI. Enterprise users with Gemini Code
> Assist licenses and API-key authentication remain supported. See Google's
> [transition announcement](https://github.com/google-gemini/gemini-cli/discussions/27274)
> and [June 18 update](https://github.com/google-gemini/gemini-cli/discussions/28017).

## Docker Image

```bash
docker build -f Dockerfile.gemini -t openab-gemini:latest .
```

The image installs `@google/gemini-cli` globally via npm.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.gemini.discord.enabled=true \
  --set agents.gemini.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.gemini.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.gemini.command=gemini \
  --set agents.gemini.args='{--acp}' \
  --set agents.gemini.workingDir=/home/node \
  --set image.tag=beta
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.
> 
> (Optional) `agents.gemini.args='{--acp}'` could be modified as `{--model,gemini-3-pro-preview,--acp}` if specific model is required. Otherwise, the default value will be 'Auto (Gemini 3)'.

### Image Tag

Use `--set image.tag=<version>` to set the image version globally.
The chart auto-appends `-<agent>` to produce the final tag (see [image-tags.md](image-tags.md) for full details).

| Tag | Resolves to | Description |
|-----|-------------|-------------|
| `beta` | `beta-gemini` | Floating beta channel (latest pre-release) |
| `0.9.0-beta.2` | `0.9.0-beta.2-gemini` | Pinned to exact version |
| `0.9` | `0.9-gemini` | Latest patch in minor (floating) |
| `stable` | `stable-gemini` | Floating stable channel |

To override a single agent's image instead of the global tag:
```bash
--set agents.gemini.image=ghcr.io/openabdev/openab:beta-gemini
```

> ⚠️ There is no `latest` tag. Use `beta` or `stable`, or pin to an exact version.

## Manual config.toml

```toml
[agent]
# command and args default from OPENAB_AGENT_COMMAND="gemini --acp"
# Only override if you need non-default behavior
env = { GEMINI_API_KEY = "${GEMINI_API_KEY}" }
```

## Authentication

Gemini CLI authentication depends on the account type:

- **API key**: Set the `GEMINI_API_KEY` environment variable.
- **Enterprise or Google Cloud OAuth**: Run the Google OAuth flow inside the pod
  using an organizational account with supported Gemini Code Assist or Google
  Cloud access.
- **Individual OAuth**: Google AI Pro, Google AI Ultra, and free-tier individual
  accounts are no longer served by Gemini CLI. Migrate these users to
  Antigravity CLI.
