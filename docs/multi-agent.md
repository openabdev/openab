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

## Bot-to-Bot Communication

> 📖 Full config options: [docs/config-reference.md](config-reference.md)

By default, each agent ignores messages from other bots. To enable multi-agent collaboration in the same channel (e.g. a code review bot handing off to a deploy bot), configure `allow_bot_messages` in each agent's `config.toml`:

```toml
[discord]
allow_bot_messages = "mentions"  # recommended
```

### Modes

| Value | Behavior | Loop risk |
|---|---|---|
| `"off"` (default) | Ignore all bot messages | None |
| `"mentions"` | Only respond to bot messages that @mention this bot | Very low — bots must explicitly @mention each other |
| `"all"` | Respond to all bot messages | Mitigated by turn cap (10 consecutive bot messages) |

### Which mode should I use?

**`"mentions"` is recommended for most setups.** It enables collaboration while acting as a natural loop breaker — Bot A only processes Bot B's message if Bot B explicitly @mentions Bot A. Two bots won't accidentally ping-pong.

Use `"all"` only when bots need to react to each other's messages without explicit mentions (e.g. monitoring bots). A hard cap of 10 consecutive bot-to-bot turns prevents infinite loops.

### Example: Code Review → Deploy handoff

```
┌──────────────────────────────────────────────────────────┐
│ Discord Channel #dev                                     │
│                                                          │
│  👤 User: "Review this PR and deploy if it looks good"   │
│       │                                                  │
│       ▼                                                  │
│  🤖 Kiro (allow_bot_messages = "off"):                   │
│       "LGTM — tests pass, no security issues.            │
│        @DeployBot please deploy to staging."             │
│       │                                                  │
│       ▼                                                  │
│  🤖 Deploy Bot (allow_bot_messages = "mentions"):        │
│       "Deploying to staging... ✅ Done."                  │
└──────────────────────────────────────────────────────────┘
```

Note: the review bot doesn't need `allow_bot_messages` enabled — only the bot that needs to *receive* bot messages does.

### Helm values

```bash
helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$KIRO_BOT_TOKEN" \
  --set agents.kiro.discord.allowBotMessages="off" \
  --set agents.deploy.discord.botToken="$DEPLOY_BOT_TOKEN" \
  --set agents.deploy.discord.allowBotMessages="mentions"
```

### Safety

- The bot's own messages are **always** ignored, regardless of setting
- `"mentions"` mode is a natural loop breaker — no rate limiter needed
- `"all"` mode has a hard cap of 10 consecutive bot-to-bot turns per channel
- Channel and user allowlists still apply to bot messages
- `trusted_bot_ids` further restricts which bots are allowed through

### Restricting to specific bots

If you only want to accept messages from specific bots (e.g. your own deploy bot), add their Discord user IDs:

```toml
[discord]
allow_bot_messages = "mentions"
trusted_bot_ids = ["123456789012345678"]  # only this bot's messages pass through
```

When `trusted_bot_ids` is empty (default), any bot can pass through (subject to the mode check). When set, only listed bots are accepted — all others are silently ignored.
