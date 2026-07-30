# LINE WORKS Setup

Run OpenAB as a LINE WORKS bot — workplace messaging with 1:1 talks and group
channels. Webhook-only platform: LINE WORKS POSTs signed events to a public
HTTPS callback; outbound messages go through a REST API authenticated with an
OAuth 2.0 service-account JWT.

For the machine-readable capability/feature matrix see
[`docs/platforms/schema/lineworks.toml`](platforms/schema/lineworks.toml).

## What you need

| Credential | Where it comes from | Config key |
|---|---|---|
| Bot ID | Developer Console → Bot | `bot_id` / `LINEWORKS_BOT_ID` |
| Bot Secret | Developer Console → Bot | `bot_secret` / `LINEWORKS_BOT_SECRET` |
| Client ID | Developer Console → API 2.0 App | `client_id` / `LINEWORKS_CLIENT_ID` |
| Client Secret | Developer Console → API 2.0 App | `client_secret` / `LINEWORKS_CLIENT_SECRET` |
| Service Account | Developer Console → App (issue it) | `service_account` / `LINEWORKS_SERVICE_ACCOUNT` |
| Private Key (PEM) | Developer Console → App (downloadable once) | `private_key_file` / `LINEWORKS_PRIVATE_KEY_FILE` |

## 1. Create the API 2.0 App

In the [LINE WORKS Developer Console](https://dev.worksmobile.com/) (admin
account required):

1. **API 2.0 → App → Add**: note the **Client ID** and **Client Secret**.
2. **Service Account**: issue one — an email-form account like
   `xxx@yourtenant.serviceaccount` (`service_account`).
3. **Private Key**: issue and download the PEM (single download — store it
   safely; this signs the RS256 JWT for token exchange).
4. **OAuth Scopes**: grant **`bot.message`** (message send) and **`bot.read`**
   (read-only bot details, used for mention-gating name lookup). The adapter
   requests exactly `bot.message,bot.read` — the broad read/write `bot` scope
   is not needed.

## 2. Create the Bot

1. **Bot → Add**: note the **Bot ID** (numeric) and **Bot Secret**.
2. Set the **Callback URL** to `https://<your-host><webhook_path>` (default
   path `/webhook/lineworks`) and enable it.
   ⚠️ The callback endpoint must present a **CA-signed certificate** —
   self-signed certificates are rejected. For local development use a tunnel
   (e.g. `cloudflared`).
3. In the **Admin Console → Services → Bot**, add the bot and assign its
   usage scope — members outside the scope cannot see or message it.

## 3. Configure OpenAB

```toml
[lineworks]
bot_id           = "${LINEWORKS_BOT_ID}"
bot_secret       = "${LINEWORKS_BOT_SECRET}"
client_id        = "${LINEWORKS_CLIENT_ID}"
client_secret    = "${LINEWORKS_CLIENT_SECRET}"
service_account  = "bot@yourtenant.serviceaccount"
private_key_file = "/etc/openab/lineworks_key.pem"
# webhook_path    = "/webhook/lineworks"
# require_mention = true          # channel messages need @BotName; 1:1 always answers
# bot_name        = "My Bot"      # override mention matching (else fetched from the API)
# rich_messages   = true          # markdown → flexible-template rendering
# ack_message     = "🤔 Got it, working on it…"  # receipt signal (no reactions/typing API)
```

Every key falls back to its `LINEWORKS_*` env var (config wins). The adapter
activates only when bot id, bot secret, and the full auth material resolve.

See [`docs/config-reference.md`](config-reference.md#lineworks) for the full
field table.

## Behavior notes

- **No streaming, ever** — the platform has no message-edit API, so replies
  arrive as single messages. `ack_message` fills the silence (there is no
  reaction or typing-indicator API either).
- **Mentions are plain text.** Callbacks carry no structured mention data;
  gating matches the literal `@BotDisplayName` string (auto-resolved from
  `GET /bots/{botId}`, cached). Renaming the bot in the Console needs a
  restart or a `bot_name` override.
- **Markdown renders as flexible templates** (headings, lists, shaded code
  boxes, inline bold/code) with automatic plain-text fallback. Long replies
  split at the 10,000-char text cap.
- **Attachments**: inbound images feed the LLM (vision), audio is stored for
  STT, text files pass a whitelist; binaries are rejected with a reason the
  agent can see. Video/location/sticker events are ignored.
- **Cron targeting**: group jobs use the channel UUID (copyable from the
  app); 1:1 jobs can use `channel = "user:<loginId>"` — the send API accepts
  the email-form loginId in place of the UUID, so no directory lookup or
  extra scope is needed.

```toml
[[cron.jobs]]
schedule = "0 9 * * 1-5"
platform = "lineworks"
channel  = "user:can@yourtenant"
message  = "Summarize yesterday's merged PRs"
```

## Trust

Identity trust (L3) is first-class on the `[lineworks]` section:

```toml
[lineworks]
# allow_all_users = false            # default: deny-all (identity-trust-none ADR)
allowed_users = ["userId-uuid-1"]    # env fallback: LINEWORKS_ALLOWED_USERS (comma-separated)
```

Default is deny-all: unlisted senders get a throttled "request access" echo
carrying their userId, which is also the easiest way to discover a user's
UUID. When neither `[lineworks]` trust fields nor `LINEWORKS_ALLOW_ALL_USERS`
/ `LINEWORKS_ALLOWED_USERS` env vars are set, trust falls back to the
deprecated uniform `GATEWAY_ALLOW_ALL_USERS` / `GATEWAY_ALLOWED_USERS` seed
(a Phase-1 deprecation warning is logged; the fallback becomes a startup
error in Phase 2, #1356).
