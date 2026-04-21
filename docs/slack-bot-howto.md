# Slack Bot Setup Guide

Step-by-step guide to create and configure a Slack bot for openab.

## 1. Create a Slack App

1. Go to https://api.slack.com/apps
2. Click **Create New App** → **From scratch**
3. Enter an app name (e.g. "OpenAB") and select your workspace
4. Click **Create App**

## 2. Enable Socket Mode

Socket Mode uses a persistent WebSocket connection — no public URL or ingress needed.

1. In the left sidebar, click **Socket Mode**
2. Toggle **Enable Socket Mode** to ON
3. You'll be prompted to generate an **App-Level Token**:
   - Token name: `openab-socket` (or any name)
   - Scope: `connections:write`
   - Click **Generate**
4. Copy the token (`xapp-...`) — this is your `SLACK_APP_TOKEN`

## 3. Subscribe to Events

1. In the left sidebar, click **Event Subscriptions**
2. Toggle **Enable Events** to ON
3. Under **Subscribe to bot events**, add:
   - `app_mention` — triggers when someone @mentions the bot
   - `message.channels` — receives messages in public channels (for thread follow-ups)
   - `message.groups` — receives messages in private channels (for thread follow-ups)
4. Click **Save Changes**

## 4. Add Bot Token Scopes

1. In the left sidebar, click **OAuth & Permissions**
2. Under **Bot Token Scopes**, add:

| Scope | Purpose |
|-------|---------|
| `app_mentions:read` | Receive @mention events |
| `chat:write` | Send and edit messages |
| `channels:history` | Read public channel messages (for thread context) |
| `groups:history` | Read private channel messages (for thread context) |
| `channels:read` | List public channels |
| `groups:read` | List private channels |
| `reactions:write` | Add/remove emoji reactions |
| `files:read` | Download file attachments (images, audio) |
| `users:read` | Resolve user display names |

## 5. Install to Workspace

1. In the left sidebar, click **Install App**
2. Click **Install to Workspace** (or **Reinstall** if you've changed scopes)
3. Authorize the requested permissions
4. Copy the **Bot User OAuth Token** (`xoxb-...`) — this is your `SLACK_BOT_TOKEN`

## 6. Configure openab

Add the `[slack]` section to your `config.toml`:

```toml
[slack]
bot_token = "${SLACK_BOT_TOKEN}"
app_token = "${SLACK_APP_TOKEN}"
allowed_channels = []                # empty = allow all channels
# allowed_users = ["U0123456789"]    # empty = allow all users
```

Set the environment variables:

```bash
export SLACK_BOT_TOKEN="xoxb-..."
export SLACK_APP_TOKEN="xapp-..."
```

## 7. Invite the Bot

In each Slack channel where you want to use the bot:

```
/invite @OpenAB
```

## 8. Test

In a channel where the bot is invited:

```
@OpenAB explain this code
```

The bot will reply in a thread. After that, just type in the thread — no @mention needed for follow-ups.

## Finding Channel and User IDs

- **Channel ID**: Right-click the channel name → **View channel details** → ID at the bottom (starts with `C` for public, `G` for private)
- **User ID**: Click a user's profile → **...** menu → **Copy member ID** (starts with `U`)

## Troubleshooting

### Bot doesn't respond to @mentions

1. Verify Socket Mode is enabled in your app settings
2. Check that `app_mention` is subscribed under **bot events** (not user events)
3. Ensure the app is reinstalled after adding new event subscriptions
4. Check the bot is invited to the channel (`/invite @YourSlackAppName`)
5. Run with `RUST_LOG=openab=debug cargo run` to see incoming events

### Bot doesn't respond to thread follow-ups

1. Verify `message.channels` (and `message.groups` for private channels) are subscribed under **bot events**
2. Reinstall the app after adding these events

### "not_authed" or "invalid_auth" errors

1. Verify your `SLACK_BOT_TOKEN` starts with `xoxb-`
2. Verify your `SLACK_APP_TOKEN` starts with `xapp-`
3. Check the tokens haven't been revoked in your app settings

### Reactions not showing

1. Verify `reactions:write` scope is added
2. Reinstall the app after adding the scope
