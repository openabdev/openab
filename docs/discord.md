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

> 📖 Full config options with defaults: [docs/config-reference.md](config-reference.md#discord)

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
allowed_channels = ["123456789"]      # channel ID allowlist (empty = all)
allowed_users = ["987654321"]         # user ID allowlist (empty = all)
allow_bot_messages = "off"            # off | mentions | all
allow_user_messages = "multibot-mentions"      # multibot-mentions | involved | mentions
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
| `"involved"` | Respond in threads the bot owns or has participated in without @mention. Main channel always requires @mention. |
| `"mentions"` | Always require @mention, even in the bot's own threads. |
| `"multibot-mentions"` (default) | Same as `involved` in single-bot threads. In threads where other bots have also posted, requires @mention — prevents all bots from responding to every message. |

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

**Admission override:** A trusted bot that explicitly @mentions this bot bypasses the `allow_bot_messages` mode entirely — the mention is treated the same as a human @mention. This allows trusted bots to pull this bot into threads even when `allow_bot_messages = "off"`. Messages from trusted bots *without* @mention still follow normal gating.

### `allowed_role_ids`

Role IDs that trigger the bot, same as a direct @mention. This enables users to invoke multiple bots simultaneously with a single role mention (e.g. `@AllBots review this`).

```toml
allowed_role_ids = ["123456789012345678"]  # @mention this role = trigger the bot
```

Empty (default) = role mentions do not trigger the bot.

**Setup:**
1. Create a Discord role (e.g. `Bots` or `AllAgents`)
2. Assign the role to all bots you want to trigger together
3. Add the role's ID to each bot's `allowed_role_ids`
4. Users type `@RoleName <prompt>` to trigger all bots at once

> **Note:** If multiple bots share the same role, all will respond simultaneously. Use `multibot-mentions` mode if you want bots to require explicit @mention when other bots are already in the thread.

#### Interaction with `multibot-mentions` mode

When `allow_user_messages = "multibot-mentions"` is set alongside `allowed_role_ids`:

| Action | Result |
|--------|--------|
| `@Role review this` in a channel | All bots trigger (role mention = explicit mention) |
| Follow-up in the thread without @mention | Only the thread owner responds (multibot gate kicks in) |
| `@Role follow up` in the thread | All bots respond again |

This gives the best of both worlds: one role mention to summon all bots, but subsequent messages in the thread don't cause all bots to pile on.

### `peer_agent_role_ids`

Role IDs that explicitly target a **sibling / peer** agent bot in this deployment — i.e. role IDs belonging to *other* OpenAB bots that are configured to share the same Discord channels. This list is the inverse of `allowed_role_ids`:

| 列表 | 語意 |
|------|------|
| `discord.allowed_role_ids` | 觸發「這個 bot 自己」的角色 ID（即直接 @mention 之外的角色觸發方式）|
| `discord.peer_agent_role_ids` | 屬於「其他」已設定 OpenAB agent 的角色 ID（用於識別對等的 peer bot）|

> ⚠️ **重要的邊界條件（不是建議，是硬性規定）**
>
> - **「自己的」角色不可放入 `peer_agent_role_ids`**。如果你把本 bot 的觸發角色同時列進 peer 列表，這個 bot 會在結構化的角色提及中被誤判為「被對等 peer 鎖定」而提前拒收訊息。
> - **與本部署無關的 Discord 角色不可放入 `peer_agent_role_ids`**。這個列表是白名單，不是黑名單 — 任何錯誤列入的角色都會把這個 bot 在合法的角色提及場景裡提前 reject。
> - **不要從角色名稱、附件內文、引述內容、prompt 文字或 LLM 推論**。peer 列表 100% 由 operator 手動維護。OpenAB 不會嘗試自動猜測哪個角色屬於哪個 bot。

#### 這個機制在做什麼

Discord 對一則訊息提供兩種**伺服器端結構化**的提及欄位：

- `Message::mentions`：`@user-id` 提及（user 物件）
- `Message::mention_roles`：`@role-id` 提及（role ID）

OpenAB 的 Discord adapter 在判斷是否要處理一則訊息時，會先看這兩個欄位，並套用以下規則：

1. **結構化 `@user-id` 提及另一個 bot 帳號**（`mentions` 陣列中存在 `user.bot == true` 且 `user.id != this_bot.user.id`）→ **這個 bot 直接 reject**，不會落到 `involved` / MultibotMentions 旁路。
2. **結構化 `@role-id` 提及的 ID 出現在 `peer_agent_role_ids`，而且同一則訊息中沒有任何 `allowed_role_ids` 的角色**→ 同樣 reject，關閉「角色觸發後、multibot 快取尚未觀察到對等 bot」的時間差漏洞。

#### 安全語意（必須記住）

實際上 OpenAB 在判定 `is_mentioned` 時，會掃**三個**來源：

| 訊息內容來源 | 是否算 routing authority？ |
|--------------|----------------------------|
| 結構化 `@user-id` 提及（`Message::mentions`） | ✅ 是 |
| 結構化 `@role-id` 提及（`Message::mention_roles`，且 ID 落在 `allowed_role_ids`） | ✅ 是 |
| 原始 `Message::content` 中的 `<@BOT_ID>` 子字串（legacy / backward-compat fallback） | ✅ 是，但**比結構化欄位寬鬆** |
| 附件本文 / 抽取出的文字（attachment body / extracted text） | ❌ 否 |
| OCR / STT 內容 | ❌ 否 |
| LLM 生成的文字（prompt、reply、tool output） | ❌ 否 |

換言之：**Discord 自己填入的 `mentions` / `mention_roles` 是嚴格的 routing 權威；附件、OCR/STT 抽取文字、模型輸出都**不算**。**然而，**為了 backward compatibility，目前實作同時也對原始 `Message::content` 做一次 `<@BOT_ID>` 子字串掃描，這條 legacy fallback 比 Discord 結構化欄位寬鬆，詳見下一節。**

這個設計的理由：附件、OCR/STT 抽取文字、模型輸出都可以包含任意 `@user` 或 `@role` 字串，如果把它們當作 routing signal，等於把 routing 決策交給可被污染的文字內容。OpenAB 明確拒絕這條路。

#### 🔐 SECURITY NOTE：legacy `Message::content` 子字串 fallback

為了 backward compatibility，目前 `is_mentioned` 的計算還包含第三條路徑：

```text
is_mentioned =
    msg.mentions_user_id(bot_id)               // 結構化 mentions
 || msg.content.contains(format!("<@{}>", …))  // 原始 content 子字串掃描
 || (allowed_role_ids ∩ msg.mention_roles)     // 結構化 mention_roles
```

這條 `msg.content.contains("<@BOT_ID>")` fallback 的特性：

- 它是**字串層級的子字串搜尋**，掃描整個 `Message::content`，包含使用者**貼上 / 引述 / 程式碼區塊（`` ``` `` 或 `` ` ``）**裡出現的字面 ` <@BOT_ID> ` 字串。
- 它**不會**去掃描附件本體、OCR/STT 抽取出的文字、模型生成文字 — 那些管線不會回灌到 `Message::content`，所以本節才把它們列在 ❌。
- 但只要人類在 inline 文字（無論是手動輸入、貼上、引用、放在 code block 裡）放進字面的 `<@BOT_ID>` markup，bot 就會被視為「被提及」並通過 admission gate。

> ⚠️ **已知強化機會（NOT yet mitigated）**
>
> 這條 legacy fallback 是**已知的強化機會**，**不是**已經解決的問題。它的存在是有意的（向後相容），但它的比對範圍比 Discord 的結構化 `mentions` 欄位寬鬆，未來硬化方向包含：
>
> - 只接受 `msg.content` 中**結構化解析**出的 mention（與 `msg.mentions` 對齊）
>
> - 或明確排除在 fenced code block / blockquote 內出現的字面 mention markup
>
> 目前**未實作**上述硬化；operator 在評估 threat model 時，請把「任何 inline 文字位都可能包含可觸發 bot 的字面 mention markup」列入考量。

#### 向後相容

`peer_agent_role_ids` 省略、或顯式設定為 `[]` 時：

- role-peer 檢查**靜默**（不會 reject 任何訊息）
- 既有部署的行為**完全保留**
- 升級後不需要任何動作；只有在你部署了多個共用 Discord 角色的 OpenAB bot、且希望提前關閉「角色單觸發 multihop」的漏洞時，才需要填入這個欄位

#### 部署範例（deployment-specific，**非**通用預設值）

> ⚠️ 下面這組 ID 是特定部署的範例，**不是** OpenAB 的預設值。請依你自家 Discord 伺服器實際建立的 role ID 替換；切勿照抄。

在這個部署裡，三個 OpenAB bot 分別被綁定到三個 Discord role，operator 透過手動列舉的方式設定 peer 角色：

| Bot | 自身角色（放 `allowed_role_ids`）| 對等角色（放 `peer_agent_role_ids`）|
|-----|----------------------------------|-------------------------------------|
| Claude (`ArthurClaude`) | `1536737647253266445` | `1536737398191300661`（Codex）<br>`1536738445651615764`（Gemini）|
| Codex (`ArthurCodex`) | `1536737398191300661` | `1536737647253266445`（Claude）<br>`1536738445651615764`（Gemini）|
| Gemini (`ArthurGemini`) | `1536738445651615764` | `1536737647253266445`（Claude）<br>`1536737398191300661`（Codex）|

對應到 `config.toml`：

```toml
# ArthurClaude — peer list = Codex + Gemini
[discord]
allowed_role_ids    = [1536737647253266445]              # Claude's own role
peer_agent_role_ids  = [
  1536737398191300661, # Codex role   (peer)
  1536738445651615764, # Gemini role  (peer)
]
```

```toml
# ArthurCodex — peer list = Claude + Gemini
[discord]
allowed_role_ids    = [1536737398191300661]              # Codex's own role
peer_agent_role_ids  = [
  1536737647253266445, # Claude role  (peer)
  1536738445651615764, # Gemini role  (peer)
]
```

```toml
# ArthurGemini — peer list = Claude + Codex
[discord]
allowed_role_ids    = [1536738445651615764]              # Gemini's own role
peer_agent_role_ids  = [
  1536737647253266445, # Claude role  (peer)
  1536737398191300661, # Codex role   (peer)
]
```

> ⚠️ 注意上面三段設定只是**同一個三 bot 部署**裡的對稱填法範例。請勿把這些 ID 當作 OpenAB 預設值；不同部署、不同 Discord 伺服器，role ID 都不一樣。

#### 重新安裝時必須保留的事項

`peer_agent_role_ids` 不會從任何地方自動推導出來 — 它必須由 operator 在每次重新安裝 / 重新部署時手動重新填入。如果升級或重建時漏掉這個欄位，所有 bot 都會退回 legacy 行為（角色單觸發 multihop 漏洞重新打開）。建議把它和 `bot_token`、`allowed_channels`、`allowed_users` 一起納入部署 checklist。

---

## @Mention Behavior

The bot responds to:

1. **Direct @mention** (`@BotUser`) — always works
2. **Role mention** (`@RoleName`) — only if the role ID is in `allowed_role_ids`
3. **Thread reply** — depends on `allow_user_messages` mode (no @mention needed in `involved` mode)

```
✅ @AgentBroker hello           ← user mention, bot responds
✅ @AllBots hello               ← role mention, bot responds (if role in allowed_role_ids)
❌ @SomeOtherRole hello         ← role not in allowed_role_ids, bot ignores
```

The triggering role mention is stripped from the prompt sent to the agent (same as the bot's own user mention).

### User mention UIDs

When a user mentions another user (e.g. `@SomeUser`) in a message to the bot, the raw Discord mention `<@UID>` is preserved in the prompt sent to the LLM. This means:

- The LLM can copy `<@UID>` into its reply to produce a clickable Discord mention
- The bot's own mention is stripped (so the bot doesn't see itself being triggered)
- Triggering role mentions (in `allowed_role_ids`) are stripped
- Other role mentions are replaced with `@(role)` placeholder

To help the LLM know who each UID refers to, provide a UID→name mapping via system prompt or context entry (see [Multi-Bot Setup](#multi-bot-setup) below).

---

## Thread Behavior

When you @mention the bot in a channel, it creates a **thread** from your message and responds there. After that:

- **`multibot-mentions` mode (default):** just type in single-bot threads — no @mention needed; in multi-bot threads, @mention required
- **`involved` mode:** just type in the thread — no @mention needed
- **`mentions` mode:** @mention required for every message, even in threads

Each thread gets its own agent session. Sessions are cleaned up after `session_ttl_hours` (default: 24h).

---

## Ambient Mode

Ambient mode allows the bot to passively listen to configured channels and respond only when it has something valuable to add — without requiring @mentions. See [ambient.md](ambient.md) for full details.

```toml
[ambient]
enabled = true

[ambient.discord]
channels = ["1234567890"]   # Channel IDs to monitor (and their threads)
```

When enabled:
- Non-mention messages in listed channels are buffered and periodically sent to the LLM as a batch.
- If the LLM has nothing to add, it returns `[NO_REPLY]` (silently suppressed).
- **@mention always takes priority** — the ambient buffer is discarded and the mention gets an immediate response.

---

## Attachment Handling

OpenAB processes Discord file attachments and converts them into content blocks
for the agent. Supported types (checked in order):

| Type | Detection | Agent receives |
|------|-----------|----------------|
| Audio | MIME `audio/*` | Transcribed text via STT (if enabled) |
| Text files | Extension list (`.txt`, `.md`, `.json`, etc.) | File content inlined (up to 5 files, 1 MB total) |
| Images | MIME `image/*` or image extensions | Base64-encoded image block |
| Video | MIME `video/*` or extensions (`.mp4`, `.mov`, `.webm`, `.mkv`, `.m4v`, `.avi`) | Text block with filename, content type, size, and Discord CDN URL |

Unsupported attachment types are silently ignored.

### Video attachments

Video files are not downloaded or transcoded. The agent receives metadata and the
Discord CDN URL so it can fetch or inspect the file using tools like `ffprobe`.

```
[Video attachment]
filename: demo.mp4
content_type: video/mp4
size_bytes: 8421376
url: https://cdn.discordapp.com/attachments/.../demo.mp4
```

No configuration is needed — video forwarding is always enabled.

---

## Streaming

OpenAB uses **edit-streaming** on Discord — the bot sends a placeholder message and updates it every 1.5 seconds as tokens arrive, giving a live typing effect.

Streaming is decided **per-thread**, not globally:

| Thread state | Streaming |
|---|---|
| Single bot + human | ✅ ON — live edit updates |
| 2+ bots in thread | ❌ OFF — send-once to avoid edit interference |

When a second bot posts in a thread, streaming automatically switches off for that thread. This prevents multiple bots from editing placeholder messages simultaneously, which causes visual glitches on Discord.

No configuration needed — this is automatic based on multibot detection.

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
- **Thread ownership (involvement gate):** a bot only responds in threads it owns or has participated in. See the Involvement Gate section below for full details.

### Involvement Gate

In a multi-bot setup, every bot enforces an **involvement gate** before processing any message in a thread. This gate is evaluated before `allow_user_messages` or `allow_bot_messages` mode checks.

**Rule:** A bot must be **involved** (thread owner or has previously replied) before it will process any message in that thread.

**Key constraint:** Only a human @mention — or a @mention from a bot in `trusted_bot_ids` — can pull a bot into a thread for the first time. A @mention from an untrusted bot will be **silently dropped**.

```
Bot A's thread (Bot B not yet involved, Bot A NOT in Bot B's trusted_bot_ids):

  Bot A: "@Bot_B please review this"     → ❌ dropped (Bot B not involved, Bot A untrusted)
  Human: "@Bot_B please review this"     → ✅ Bot B replies, now involved
  Bot A: "@Bot_B any updates?"           → ✅ processed (Bot B is involved)

Bot A's thread (Bot B not yet involved, Bot A IS in Bot B's trusted_bot_ids):

  Bot A: "@Bot_B please review this"     → ✅ treated as human @mention, Bot B becomes involved
```

**Why:** This prevents untrusted bots from pulling other bots into arbitrary threads without human consent, protects session pool resources, and eliminates cross-thread chain reactions. Trusted bots are explicitly authorized by the admin.

**Workaround (without trusted_bot_ids):** Pre-involve all needed bots at thread creation by @mentioning them (or using a shared role via `allowed_role_ids`).

> 📖 Full design details: [docs/messaging.md — Involvement Gate](messaging.md#involvement-gate)

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

- **Soft limit** (`max_bot_turns`, default: 100) — total bot messages in a thread without human intervention. When reached, the bot sends a one-time warning and stops responding. A human message in the thread resets the counter.
- **Hard limit** (1000, not configurable) — cap on consecutive bot messages in `allow_bot_messages = "all"` mode. When reached, bot-to-bot conversation stops until a human replies.

Both limits count **all** bot messages in the thread, including the bot's own replies. In a two-bot ping-pong with `max_bot_turns = 100`, each bot sends ~50 messages before the limit triggers.

Warning messages are sent exactly once (on the exact threshold hit) to prevent warnings from ping-ponging between bots.

```toml
[discord]
max_bot_turns = 200               # default is 100
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
  --set agents.kiro.discord.allowUserMessages=involved \
  --set-string 'agents.kiro.discord.allowedRoleIds[0]=YOUR_ROLE_ID'
```

⚠️ Use `--set-string` for channel/user/role IDs to avoid float64 precision loss.

---

## Troubleshooting

### Bot doesn't respond

1. **Check channel ID** — make sure it's in `allowed_channels`
2. **Check permissions** — bot needs Send Messages, Create Public Threads, Read Message History in the channel
3. **Check intents** — Message Content Intent must be enabled in Developer Portal
4. **Check @mention type** — use user mention or a role in `allowed_role_ids`
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
kubectl exec -it deployment/openab-kiro -- sh -c "$OPENAB_AGENT_AUTH_COMMAND"
kubectl rollout restart deployment/openab-kiro
```
