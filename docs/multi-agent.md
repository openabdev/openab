# Multi-Agent Setup

You can run multiple agents in a single Helm release. Each agent key in the `agents` map creates its own Deployment, ConfigMap, Secret, and PVC.

## Example: Kiro + Claude Code

```bash
helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$KIRO_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=KIRO_CHANNEL_ID' \
  --set agents.claude.discord.botToken="$CLAUDE_BOT_TOKEN" \
  --set-string 'agents.claude.discord.allowedChannels[0]=CLAUDE_CHANNEL_ID' \
  --set agents.claude.image=ghcr.io/openabdev/openab-claude:latest \
  --set agents.claude.command=claude-agent-acp \
  --set agents.claude.workingDir=/home/node
```

## How It Works

- Each `agents.<name>` entry creates an independent set of Kubernetes resources (Deployment, ConfigMap, Secret, PVC)
- Each agent gets its own Discord bot token and allowed channels
- Agents run in separate pods and don't share state
- Set `agents.<name>.enabled: false` to skip creating resources for an agent

## Example: All Four Agents

```bash
helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$KIRO_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=KIRO_CHANNEL_ID' \
  --set agents.claude.discord.botToken="$CLAUDE_BOT_TOKEN" \
  --set-string 'agents.claude.discord.allowedChannels[0]=CLAUDE_CHANNEL_ID' \
  --set agents.claude.image=ghcr.io/openabdev/openab-claude:latest \
  --set agents.claude.command=claude-agent-acp \
  --set agents.claude.workingDir=/home/node \
  --set agents.codex.discord.botToken="$CODEX_BOT_TOKEN" \
  --set-string 'agents.codex.discord.allowedChannels[0]=CODEX_CHANNEL_ID' \
  --set agents.codex.image=ghcr.io/openabdev/openab-codex:latest \
  --set agents.codex.command=codex-acp \
  --set agents.codex.workingDir=/home/node \
  --set agents.gemini.discord.botToken="$GEMINI_BOT_TOKEN" \
  --set-string 'agents.gemini.discord.allowedChannels[0]=GEMINI_CHANNEL_ID' \
  --set agents.gemini.image=ghcr.io/openabdev/openab-gemini:latest \
  --set agents.gemini.command=gemini \
  --set agents.gemini.args='{--acp}' \
  --set agents.gemini.workingDir=/home/node
```

See individual agent docs for authentication steps:
- [Kiro CLI](kiro.md)
- [Claude Code](claude-code.md)
- [Codex](codex.md)
- [Gemini](gemini.md)
