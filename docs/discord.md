# Discord Guide

Complete guide to setting up, configuring, and running OpenAB with Discord.

## Bot Setup

### 1. Create a Discord Application

1. Go to the [Discord Developer Portal](https://discord.com/developers/applications)
2. Click **New Application**
3. Give it a name (e.g. `AgentBroker`) and click **Create**

### 2. Enable Gateway Intents

1. In your application, go to the **Bot** tab (left sidebar)
2. Scroll down to **Privileged Gateway Intents**
3. Enable **Message Content Intent**
4. Enable **Server Members Intent** (recommended)
5. Click **Save Changes**

### 3. Get the Bot Token

1. Still on the **Bot** tab, click **Reset Token**
2. Copy the token — you'll need this for `DISCORD_BOT_TOKEN`
3. Keep this token secret. If it leaks, reset it immediately

### 4. Set Bot Permissions

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

### 5. Invite the Bot to Your Server

1. Open the URL from step 4 in your browser
2. Select the server you want to add the bot to
3. Click **Authorize**

### 6. Get the Channel ID

1. In Discord, go to **User Settings** → **Advanced** → enable **Developer Mode**
2. Right-click the channel where you want the bot to respond
3. Click **Copy Channel ID**
4. Use this ID in `allowed_channels` in your config

### 7. Get Your User ID (optional)

1. Make sure **Developer Mode** is enabled (see step 6)
2. Right-click your own username (in a message or the member list)
3. Click **Copy User ID**
4. Use this ID in `allowed_users` to restrict who can interact with the bot

---

## Configuration Reference

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
allowed_channels = ["123456789"]      # channel ID allowlist (empty = all)
allowed_users = ["987654321"]         # user ID allowlist (empty = all)
allow_bot_messages = "off"            # off | mentions | all
allow_user_messages = "involved"      # involved | mentions
trusted_bot_ids = []                  # bot user IDs allowed through (empty = any)
```

### `allowed_channels` / `allowed_users`

| `allowed_channels` | `allowed_users` | Result |
|---|---|---|
| empty | empty | All users, all channels (default) |
| set | empty | Only these channels, all users |
| empty | set | All channels, only these users |
| set | set | **AND** — must be in allowed channel AND allowed user |

- Empty `allowed_users` (default) = no user filtering
- Denied users get a 🚫 reaction and no reply

### `allow_bot_messages`

Controls whether the bot processes messages from other Discord bots.

| Value | Behavior | Loop risk |
|---|---|---|
| `"off"` (default) | Ignore all bot messages | None |
| `"mentions"` | Only process bot messages that @mention this bot | Very low |
| `"all"` | Process all bot messages (capped at 10 consecutive) | Mitigated by turn cap |

The bot's own messages are always ignored regardless of this setting.

### `allow_user_messages`

Controls whether the bot requires @mention in threads.

| Value | Behavior |
|---|---|
| `"involved"` (default) | Respond in threads the bot owns or has participated in without @mention. Main channel always requires @mention. |
| `"mentions"` | Always require @mention, even in the bot's own threads. |
| `"multibot-mentions"` | Same as `involved` in single-bot threads. In threads where other bots have also posted, requires @mention — prevents all bots from responding to every message. |

#### Comparison

| Scenario | `involved` | `mentions` | `multibot-mentions` |
|---|---|---|---|
| Main channel (no @mention) | ❌ | ❌ | ❌ |
| Main channel (with @mention) | ✅ | ✅ | ✅ |
| Single-bot thread (no @mention) | ✅ | ❌ | ✅ |
| Single-bot thread (with @mention) | ✅ | ✅ | ✅ |
| Multi-bot thread (no @mention) | ✅ | ❌ | ❌ |
| Multi-bot thread (with @mention) | ✅ | ✅ | ✅ |

#### When to use which

- **`involved`** — Single-bot setup, or you want all bots to respond freely in shared threads.
- **`mentions`** — Strict control. Every message must explicitly @mention the bot. Best for high-traffic channels where accidental triggers are a concern.
- **`multibot-mentions`** — Multi-bot setup. Natural conversation in single-bot threads, explicit @mention control in multi-bot threads. Recommended for most multi-bot deployments.

### `trusted_bot_ids`

When `allow_bot_messages` is `"mentions"` or `"all"`, you can restrict which bots are allowed through:

```toml
trusted_bot_ids = ["123456789012345678"]  # only this bot's messages pass through
```

Empty (default) = any bot can pass through (subject to the mode check).

---

## @Mention Behavior

**Always @mention the bot user, not the role.** Discord shows both in autocomplete — pick the one without the role icon.

```
✅ @AgentBroker hello     ← user mention, bot responds
❌ @AgentBroker hello     ← role mention (with role icon), bot ignores
```

Role mentions are ignored because they are shared across bots and cause false positives in multi-bot setups. This is intentional since v0.7.8-beta.3 (#420, #440).

### User mention UIDs

When a user mentions another user (e.g. `@SomeUser`) in a message to the bot, the raw Discord mention `<@UID>` is preserved in the prompt sent to the LLM. This means:

- The LLM can copy `<@UID>` into its reply to produce a clickable Discord mention
- The bot's own mention is stripped (so the bot doesn't see itself being triggered)
- Role mentions are replaced with `@(role)` placeholder

To help the LLM know who each UID refers to, provide a UID→name mapping via system prompt or context entry (see [Multi-Bot Setup](#multi-bot-setup) below).

---

## Thread Behavior

When you @mention the bot in a channel, it creates a **thread** from your message and responds there. After that:

- **`involved` mode (default):** just type in the thread — no @mention needed
- **`mentions` mode:** @mention required for every message, even in threads

Each thread gets its own agent session. Sessions are cleaned up after `session_ttl_hours` (default: 24h).

---

## Multi-Bot Setup

Multiple bots can share the same Discord channel. Each bot only responds to its own @mentions.

### Helm example

```bash
helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$BOT_A_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=CHANNEL_ID' \
  --set agents.dealer.discord.botToken="$BOT_B_TOKEN" \
  --set-string 'agents.dealer.discord.allowedChannels[0]=CHANNEL_ID' \
  --set agents.dealer.discord.enabled=true \
  --set agents.dealer.command=kiro-cli \
  --set 'agents.dealer.args={acp,--trust-all-tools}'
```

### Known limitations

- **One thread per message:** when you @mention both bots in a single message, only the first bot creates a thread. The second bot's thread creation fails and the message is dropped. Workaround: @mention each bot in separate messages.
- **Thread ownership:** a bot only responds in threads it owns or has participated in (`involved` mode). To have Bot B respond in Bot A's thread, use `mentions` mode and explicitly @mention Bot B.

### Recommended: `multibot-mentions` mode

In multi-bot channels, use `multibot-mentions` to get the best of both worlds:

```toml
[discord]
allow_user_messages = "multibot-mentions"
```

- **Single-bot threads:** natural conversation, no @mention needed (same as `involved`)
- **Multi-bot threads:** requires @mention so only the addressed bot responds

### Bot-to-bot communication

To enable bots to collaborate (e.g. code review → deploy handoff):

```toml
# Bot that receives bot messages
[discord]
allow_bot_messages = "mentions"
```

### Bot turn limits

To prevent runaway bot-to-bot loops, OpenAB enforces two layers of protection:

- **Soft limit** (`max_bot_turns`, default: 20) — consecutive bot turns without human intervention. When reached, the bot sends a warning and stops responding. A human message in the thread resets the counter.
- **Hard limit** (100, not configurable) — absolute cap on bot turns between human interventions. When reached, bot-to-bot conversation stops until a human replies.

```toml
[discord]
max_bot_turns = 30  # default is 20
```

### Ice-breaking: teaching bots who's in the room

Since user mentions are preserved as raw `<@UID>`, bots need a UID→name mapping to know who is who. Add an ice-breaking greeting to each bot's system prompt or context entry:

```
We have 3 participants in this room:

MY_NICIKNAME    <@MY_NAME>
BOT1_NICKNAME   <@BOT1>
BOT2_NICKNAME   <@BOT2>

Always use <@UID> format to mention someone in your messages.
```

This lets each bot build the mapping in its own context from the start and correctly mention others using `<@UID>`.

See [multi-agent.md](multi-agent.md) for detailed examples.

---

## Helm Values

```bash
helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.kiro.discord.allowBotMessages=off \
  --set agents.kiro.discord.allowUserMessages=involved
```

⚠️ Use `--set-string` for channel/user IDs to avoid float64 precision loss.

---

## Troubleshooting

### Bot doesn't respond

1. **Check channel ID** — make sure it's in `allowed_channels`
2. **Check permissions** — bot needs Send Messages, Create Public Threads, Read Message History in the channel
3. **Check intents** — Message Content Intent must be enabled in Developer Portal
4. **Check @mention type** — use user mention, not role mention
5. **Check if in a thread** — with `mentions` mode, @mention is required even in threads

### Bot stops receiving messages after restart

Discord Gateway may throttle event delivery after rapid reconnects. Use `scale 0 → wait 5s → scale 1` instead of `rollout restart`:

```bash
kubectl scale deployment/openab-kiro --replicas=0 && sleep 5 && kubectl scale deployment/openab-kiro --replicas=1
```

See [#455](https://github.com/openabdev/openab/issues/455) for details.

### "Failed to create thread"

Discord only allows one thread per message. If another bot already created a thread on the same message, this error appears. The message is dropped. This is a known limitation for multi-bot setups (#457).

### "Sent invalid authentication"

The bot token is wrong or expired. Reset it in the Developer Portal and redeploy.

### "Failed to start agent"

The agent CLI isn't authenticated. For kiro-cli:

```bash
kubectl exec -it deployment/openab-kiro -- kiro-cli login --use-device-flow
kubectl rollout restart deployment/openab-kiro
```
