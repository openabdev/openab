# Gemini CLI

Gemini CLI supports ACP natively via the `--acp` flag — no adapter needed.

## Docker Image

```bash
docker build -f Dockerfile.gemini -t openab-gemini:latest .
```

The image installs `@google/gemini-cli` globally via npm.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.gemini.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.gemini.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.gemini.image=ghcr.io/openabdev/openab-gemini:latest \
  --set agents.gemini.command=gemini \
  --set agents.gemini.args='{--acp}' \
  --set agents.gemini.workingDir=/home/node
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

## First-Run Workaround

Gemini CLI needs `~/.gemini/projects.json` to exist before the first ACP session starts. In ephemeral containers, the file may not be created yet, which can surface as `ENOENT` or `Unexpected end of JSON input` during the first run.

This is a workaround for the upstream Gemini CLI issue `google-gemini/gemini-cli#22583`.

The `openab-gemini` image now seeds this file automatically with the expected registry shape:

```json
{"projects":{}}
```

If you build a custom Gemini image or override the entrypoint, seed the directory before launching `openab`:

```bash
mkdir -p /home/node/.gemini && printf '{"projects":{}}\n' > /home/node/.gemini/projects.json && exec openab /etc/openab/config.toml
```

The `exec openab /etc/openab/config.toml` part assumes the Docker image default config path.

For Kubernetes or Helm deployments that use a custom Gemini entrypoint, use the same idea with an init container and a shared volume mounted at `/home/node/.gemini`.

## Manual config.toml

```toml
[agent]
command = "gemini"
args = ["--acp"]
working_dir = "/home/node"
env = { GEMINI_API_KEY = "${GEMINI_API_KEY}" }
```

## Authentication

Gemini supports Google OAuth or an API key:

- **API key**: Set `GEMINI_API_KEY` environment variable
- **OAuth**: Run Google OAuth flow inside the pod
