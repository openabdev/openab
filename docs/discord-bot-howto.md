# Discord Bot Setup Guide

Step-by-step guide to create and configure a Discord bot for openab.

## 1. Create a Discord Application

1. Go to the [Discord Developer Portal](https://discord.com/developers/applications)
2. Click **New Application**
3. Give it a name (e.g. `AgentBroker`) and click **Create**

## 2. Enable Message Content Intent

1. In your application, go to the **Bot** tab (left sidebar)
2. Scroll down to **Privileged Gateway Intents**
3. Enable **Message Content Intent**
4. Click **Save Changes**

## 3. Get the Bot Token

1. Still on the **Bot** tab, click **Reset Token**
2. Copy the token — you'll need this for `DISCORD_BOT_TOKEN`
3. Keep this token secret. If it leaks, reset it immediately

## 4. Set Bot Permissions

1. Go to **OAuth2** → **URL Generator** (left sidebar)
2. Under **Scopes**, check `bot`
3. Under **Bot Permissions**, check:
   - Send Messages
   - Send Messages in Threads
   - Create Public Threads
   - Read Message History
   - Add Reactions
   - Manage Messages
4. Copy the generated URL at the bottom

## 5. Invite the Bot to Your Server

1. Open the URL from step 4 in your browser
2. Select the server you want to add the bot to
3. Click **Authorize**

## 6. Get the Channel ID

1. In Discord, go to **User Settings** → **Advanced** → enable **Developer Mode**
2. Right-click the channel where you want the bot to respond
3. Click **Copy Channel ID**
4. Use this ID in `allowed_channels` in your config

## 7. Get Your User ID (optional)

1. Make sure **Developer Mode** is enabled (see step 6)
2. Right-click your own username (in a message or the member list)
3. Click **Copy User ID**
4. Use this ID in `allowed_users` to restrict who can interact with the bot

## 8. Configure openab

Set the bot token and channel ID:

```bash
# Local
export DISCORD_BOT_TOKEN="your-token-from-step-3"
```

In `config.toml`:
```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
allowed_channels = ["your-channel-id-from-step-6"]
# allowed_users = ["your-user-id-from-step-7"]  # optional: restrict who can use the bot
```

### Access control behavior

| `allowed_channels` | `allowed_users` | Result |
|---|---|---|
| empty | empty | All users, all channels (default) |
| set | empty | Only these channels, all users |
| empty | set | All channels, only these users |
| set | set | **AND** — must be in allowed channel AND allowed user |

- Empty `allowed_users` (default) = no user filtering, fully backward compatible
- Denied users get a 🚫 reaction and no reply

For Kubernetes:
```bash
kubectl create secret generic openab-secret \
  --from-literal=discord-bot-token="your-token-from-step-3"
```

## 9. Test

In the allowed channel, mention the bot:

```
@AgentBroker hello
```

The bot should create a thread and respond. After that, just type in the thread — no @mention needed.

## Troubleshooting

- **Bot doesn't respond** — check that the channel ID is correct and the bot has permissions in that channel
- **"Sent invalid authentication"** — the bot token is wrong or expired, reset it in the Developer Portal
- **"Failed to start agent"** — kiro-cli isn't authenticated, run `kiro-cli login --use-device-flow` inside the container
- **`gh` commands fail with 401** — the agent needs GitHub CLI authentication. See [gh auth device flow guide](gh-auth-device-flow.md) for how to authenticate in a headless container
