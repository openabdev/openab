# Configuration Reference

OpenAB is configured via a TOML file (default: `config.toml`). Environment variables can be interpolated using `${VAR_NAME}` syntax.

Configure at least one runnable surface: a Discord/Slack adapter, an enabled
Unified or Standalone Gateway platform, the ACP server endpoint, or `[mcp]`
facade-only mode.

## Loading Config

Specify the config source with `--config` / `-c`:

```bash
# Local file (default: config.toml when omitted)
openab run -c config.toml

# Remote URL via HTTPS (recommended)
openab run -c https://example.com/config.toml

# Remote URL via HTTP (warns — avoid in production; config contains secrets)
openab run -c http://internal.example.com/config.toml

# Amazon S3 (or S3-compatible) object
openab run -c s3://my-bucket/path/to/config.toml
```

Remote config is fetched via HTTP GET with a 10-second timeout and a 1 MiB response size limit. Environment variable expansion (`${VAR}`) works identically on both local and remote config content.

> **Security best practice:** Never hardcode secrets in remote config files. Use environment variable references like `bot_token = "${DISCORD_BOT_TOKEN}"` and inject the actual values via local environment variables or Kubernetes Secrets. For centralized secret management with rotation and audit, use `[secrets.refs]` with AWS Secrets Manager or an exec provider — see [secrets-management.md](secrets-management.md). OpenAB expands `${VAR}` identically for both local and remote config.

### `s3://` config source

`openab run -c s3://<bucket>/<key>` fetches the config object directly from Amazon S3
(requires a build with the `config-s3` feature, which is on by default). The same
1 MiB size cap, UTF-8 validation, and `${VAR}` expansion apply as for HTTP(S) sources.

**Credential & region resolution** uses the standard AWS provider chain — the same
mechanism as `aws-sm://` secret references:

- environment variables (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_REGION`),
- shared config/credentials files (`~/.aws/...`),
- container/instance roles: **IRSA / EKS Pod Identity** (Kubernetes) or the **ECS task role** / EC2 instance role.

There is no `[s3]` config section for this: the credentials needed to *fetch* the
config cannot live *inside* the config you are fetching. Configure the bootstrap S3
access via the environment/role above.

**Minimum IAM policy** — scope the role to only the config prefix, never `Resource: "*"`:

```json
{
  "Effect": "Allow",
  "Action": ["s3:GetObject"],
  "Resource": "arn:aws:s3:::my-bucket/path/to/*"
}
```

> **Secrets still never belong in the config object.** The `s3://` loader does not
> resolve secrets — it only fetches text and expands `${VAR}`. Keep secrets out of the
> S3 object and inject them via env vars / `[secrets.refs]` as above.

> **S3-compatible stores (Cloudflare R2, MinIO):** R2 generally works by setting
> `AWS_ENDPOINT_URL_S3` (plus R2 keys and `AWS_REGION=auto`). MinIO and some others
> additionally require path-style addressing, which the standard AWS env vars do not
> cover yet — explicit endpoint / path-style support is tracked as a follow-up. Only
> point the endpoint at trusted hosts; a poisoned endpoint env var could redirect the
> fetch to a malicious server.

---

## Discord

Discord adapter. Requires a Discord bot token.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `bot_token` | string | *required* | Discord bot token. Use `${DISCORD_BOT_TOKEN}` for env var. |
| `allow_all_channels` | bool \| omit | auto-detect | `true` = all channels; `false` = only `allowed_channels`. Omitted = inferred from list (non-empty → false, empty → true). |
| `allowed_channels` | string[] | `[]` | Channel IDs to allow. Only checked when `allow_all_channels` resolves to false. |
| `allow_all_users` | bool \| omit | auto-detect | `true` = any user; `false` = only `allowed_users`. Omitted = inferred from list. |
| `allowed_users` | string[] | `[]` | User IDs to allow. Only checked when `allow_all_users` resolves to false. |
| `allow_bot_messages` | string | `"off"` | `"off"` — ignore all bot messages. `"mentions"` — only process bot messages that @mention this bot. `"all"` — process all bot messages (capped by `max_bot_turns`). |
| `trusted_bot_ids` | string[] | `[]` | When non-empty, only these bot IDs pass the bot gate. Empty = any bot (mode permitting). **Admission override:** a trusted bot that @mentions this bot bypasses `allow_bot_messages` mode entirely (treated as human @mention, can pull bot into threads). |
| `allow_user_messages` | string | `"multibot-mentions"` | `"multibot-mentions"` — like `"involved"`, but require @mention once another bot has posted in the thread (recommended for multi-bot deployments). `"involved"` — reply in threads bot has participated in without @mention; channel messages require @mention; DMs always process. `"mentions"` — always require @mention. |
| `allow_dm` | bool | `false` | `true` = respond to Discord DMs; `false` = ignore DMs. `allowed_users` still applies in DMs. Each DM user consumes one session slot. |
| `max_bot_turns` | u32 | `100` | Max consecutive bot turns per thread before throttling (soft limit). Human message resets the counter. A compiled-in hard cap of 1000 consecutive bot messages is always enforced. |
| `message_processing_mode` | string | `"per-message"` | Message dispatch mode: `"per-message"` (each message = own turn), `"per-thread"` (all messages in thread share one buffer), or `"per-lane"` (each sender gets own buffer). See [Message Dispatch Modes](message-dispatch-modes.md). |
| `max_buffered_messages` | u32 | `10` | Per-thread/lane mpsc channel capacity. Only applies to `per-thread` / `per-lane` modes. |
| `max_batch_tokens` | u32 | `24000` | Soft token cap per ACP turn. Only applies to `per-thread` / `per-lane` modes. |

---

## Slack

Slack adapter using Socket Mode. Requires both a Bot User OAuth Token and an App-Level Token.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `bot_token` | string | *required* | Bot User OAuth Token (`xoxb-...`). |
| `app_token` | string | *required* | App-Level Token (`xapp-...`) for Socket Mode. |
| `allow_all_channels` | bool \| omit | auto-detect | Same behavior as Discord. |
| `allowed_channels` | string[] | `[]` | Slack channel IDs (e.g. `C0123456789`). |
| `allow_all_users` | bool \| omit | auto-detect | Same behavior as Discord. |
| `allowed_users` | string[] | `[]` | Slack user IDs (e.g. `U0123456789`). |
| `allow_bot_messages` | string | `"off"` | Same as Discord. |
| `trusted_bot_ids` | string[] | `[]` | Slack Bot User IDs (`U...`) or Bot IDs (`B...`). `U...` matching resolves event Bot IDs via Slack `bots.info`, so the bot token needs `users:read`. |
| `allow_user_messages` | string | `"multibot-mentions"` | Same as Discord. |
| `max_bot_turns` | u32 | `100` | Same as Discord. |
| `message_processing_mode` | string | `"per-message"` | Same as Discord. See [Message Dispatch Modes](message-dispatch-modes.md). |
| `max_buffered_messages` | u32 | `10` | Same as Discord. |
| `max_batch_tokens` | u32 | `24000` | Same as Discord. |
| `assistant_mode` | bool | `true` | Use `assistant.threads.setStatus` for status indicators instead of emoji reactions, and native content streaming via `chat.startStream`/`appendStream`/`stopStream` instead of the post+edit loop. Native streaming is suppressed when another bot is present in the thread. Requires an AI-app Slack app with `assistant:write` — set to `false` for non-AI Slack apps to keep emoji-reaction status. When native streaming is active, the `reply_to` output directive is bypassed — the streamed message is itself the in-thread reply. |

---

## Gateway

Custom Gateway adapter for platforms like Telegram, LINE, Feishu/Lark, and Google Chat. Connects to the gateway via WebSocket.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `url` | string | *required* | WebSocket URL of the gateway (e.g. `ws://openab-gateway:8080/ws`). |
| `platform` | string | `"telegram"` | Platform name for session key namespacing (e.g. `"telegram"`, `"line"`, `"feishu"`, `"googlechat"`). |
| `token` | string | — | Shared token for WebSocket authentication (optional but recommended). |
| `bot_username` | string | — | Bot username for @mention gating in groups. |
| `allow_all_channels` | bool \| omit | auto-detect | `true` = all channels; `false` = only `allowed_channels`. Omitted = inferred from list (non-empty → false, empty → true). |
| `allowed_channels` | string[] | `[]` | Chat/group IDs to allow. Only checked when `allow_all_channels` resolves to false. |
| `allow_all_users` | bool \| omit | auto-detect | `true` = any user; `false` = only `allowed_users`. Omitted = inferred from list. |
| `allowed_users` | string[] | `[]` | User IDs to allow. Only checked when `allow_all_users` resolves to false. |
| `allow_bot_messages` | bool | `false` | Allow messages from bots. Unlike Discord/Slack (which use an enum with `"off"`/`"mentions"`/`"all"`), the gateway adapter uses a simple boolean: `true` = allow all bots, `false` = block (unless in `trusted_bot_ids`). |
| `trusted_bot_ids` | string[] | `[]` | Bot IDs that bypass the bot filter even when `allow_bot_messages = false`. |
| `streaming` | bool | `false` | Enable streaming (typewriter) mode — requires the gateway platform to support message editing. |
| `streaming_placeholder` | bool | `true` | Show "…" placeholder at streaming start. Set `false` for platforms using drafts (e.g. Telegram Rich Messages). |
| `gateway_ack_timeout_secs` | u64 | `12` | Maximum wait for an operation ACK explicitly advertised by a negotiated gateway. Missing ACKs from legacy peers remain fire-and-forget. Must be greater than 0, less than `pool.prompt_hard_timeout_secs`, and greater than 10 when `platform = "teams"`. |
| `message_processing_mode` | string | `"per-message"` | Same as Discord. See [Message Dispatch Modes](message-dispatch-modes.md). |
| `max_buffered_messages` | u32 | `10` | Same as Discord. |
| `max_batch_tokens` | u32 | `24000` | Same as Discord. |

---

## LINE

First-class LINE section — credentials, connection, and L3 identity trust (config-first parity, #1376). Replaces the uniform `GATEWAY_ALLOW_ALL_USERS` / `GATEWAY_ALLOWED_USERS` env vars for LINE trust — relying on those for LINE is deprecated and warns at startup.

> **Trust resolution:** applies in **both** deployment modes (see the note under `[wecom]` / `[googlechat]` / `[teams]` below — the same applies here).

Each field resolves: config value → `LINE_*` env var → default (deny-all).

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `channel_secret` | string | — | Channel secret for webhook HMAC-SHA256 validation (L1). Env fallback: `LINE_CHANNEL_SECRET`. |
| `channel_access_token` | string | — | Channel access token for the Reply/Push Message API and media downloads. Env fallback: `LINE_CHANNEL_ACCESS_TOKEN`. |
| `webhook_path` | string | `/webhook/line` | Webhook mount path. Env fallback: `LINE_WEBHOOK_PATH`. |
| `allow_all_users` | bool \| omit | `false` (deny-all) | `true` = any user may interact (bypasses `allowed_users` entirely); `false`/omitted = only `allowed_users`. Env fallback: `LINE_ALLOW_ALL_USERS`. |
| `allowed_users` | string[] | `[]` | LINE user IDs (`U…`, 33 chars) allowed to interact. Only checked when `allow_all_users` resolves to false. Env fallback: `LINE_ALLOWED_USERS` (comma-separated). |

```toml
[line]
allowed_users = ["U1234567890abcdef0123456789abcdef"]
# allow_all_users = true   # explicit opt-in only — any user can drive the agent
```

---

## LINE WORKS

First-class LINE WORKS section — bot credentials and service-account auth (config-first parity, #1375). Each field resolves: config → `LINEWORKS_*` env → default. The adapter is enabled only when `bot_id`, `bot_secret`, `client_id`, `client_secret`, `service_account`, and a private key (inline or file) all resolve to non-empty values; an incomplete section disables the adapter, matching env-only semantics.

LINE WORKS is webhook-only: register the callback URL (`https://<host><webhook_path>`) in the Developer Console — a CA-signed HTTPS certificate is required (no self-signed). Outbound messages authenticate via the OAuth 2.0 service-account JWT flow (RS256 key from the Console).

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `bot_id` | string | — | Bot ID, cross-checked against the `X-WORKS-BotId` callback header. Env: `LINEWORKS_BOT_ID`. |
| `bot_secret` | string | — | Bot Secret for webhook HMAC-SHA256 signature verification (L1). Env: `LINEWORKS_BOT_SECRET`. |
| `client_id` | string | — | App Client ID (JWT `iss`). Env: `LINEWORKS_CLIENT_ID`. |
| `client_secret` | string | — | App Client Secret. Env: `LINEWORKS_CLIENT_SECRET`. |
| `service_account` | string | — | Service account email (JWT `sub`). Env: `LINEWORKS_SERVICE_ACCOUNT`. |
| `private_key` | string | — | RS256 private key PEM (inline; takes precedence over `private_key_file`). Env: `LINEWORKS_PRIVATE_KEY`. |
| `private_key_file` | string | — | Path to the RS256 private key PEM. Env: `LINEWORKS_PRIVATE_KEY_FILE`. |
| `webhook_path` | string | `/webhook/lineworks` | Webhook mount path. Env: `LINEWORKS_WEBHOOK_PATH`. |
| `require_mention` | bool | `true` | Channel (group) messages must @-mention the bot; 1:1 always passes. Set `false` for ambient listening. Env: `LINEWORKS_REQUIRE_MENTION`. |
| `bot_name` | string | — | Bot display name for mention matching (plain-text match; the callback has no structured mention data). When unset, fetched from `GET /bots/{botId}` and cached. Env: `LINEWORKS_BOT_NAME`. |
| `rich_messages` | bool | `true` | Render markdown replies as flexible-template (flex) messages — headings, lists, inline bold/code, and shaded code blocks. Falls back to plain text when the reply has no markdown, exceeds flex size limits, or the API rejects the payload. Env: `LINEWORKS_RICH_MESSAGES`. |
| `ack_message` | string | — (disabled) | Short receipt message sent once a user message passes the mention/trust gates (e.g. `"🤔 處理中…"`). LINE WORKS has no reaction or typing-indicator API, so this is the only "working on it" signal. The webhook callback is acknowledged first; the ack send is then awaited inside the bounded post-ack worker — before attachment download and agent dispatch — so bursts cannot fan out unbounded outbound sends. Env: `LINEWORKS_ACK_MESSAGE`. |
| `allow_all_users` | bool \| omit | `false` (deny-all) | L3 identity trust: `true` = allow all senders. Overrides the uniform `GATEWAY_ALLOW_ALL_USERS` seed for this platform. Env: `LINEWORKS_ALLOW_ALL_USERS`. |
| `allowed_users` | string[] | `[]` | LINE WORKS userIds (UUIDs, as carried in callback events — a denied sender's request-access echo shows their ID). Only checked when `allow_all_users` is `false`. Env: `LINEWORKS_ALLOWED_USERS` (comma-separated). |

Platform limits: no message edit/delete (no streaming), no reactions, no threads, plain-text messages up to 10,000 chars (longer replies are split). Inbound attachments are downloaded and processed: images feed the LLM (vision), audio is stored for STT, text files pass an extension whitelist; binaries/video/location/sticker are rejected or ignored with a reason the agent can see.

```toml
[lineworks]
bot_id           = "${LINEWORKS_BOT_ID}"
bot_secret       = "${LINEWORKS_BOT_SECRET}"
client_id        = "${LINEWORKS_CLIENT_ID}"
client_secret    = "${LINEWORKS_CLIENT_SECRET}"
service_account  = "bot@example.serviceaccount"
private_key_file = "/etc/openab/lineworks_private_key.pem"
```

---

## WeCom

Full first-class WeCom section (config-first parity, #1378) — credentials, connection, and L3 identity trust. Each field resolves: config → `WECOM_*` env → default. The adapter requires all five credentials (`corp_id`, `secret`, `token`, `encoding_aes_key`, `agent_id`); an incomplete section (after env fallback) disables the adapter, matching env-only semantics.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `corp_id` | string | — | Corp ID. Env: `WECOM_CORP_ID`. |
| `secret` | string | — | App secret. Env: `WECOM_SECRET`. |
| `token` | string | — | Callback token (L1 signature). Env: `WECOM_TOKEN`. |
| `encoding_aes_key` | string | — | 43-char callback AES key (L1). Env: `WECOM_ENCODING_AES_KEY`. |
| `agent_id` | string | — | Numeric agent id. Env: `WECOM_AGENT_ID`. |
| `webhook_path` | string | `/webhook/wecom` | Env: `WECOM_WEBHOOK_PATH`. |
| `streaming_enabled` | bool | `false` | Recall+resend streaming opt-in. Env: `WECOM_STREAMING_ENABLED`. |
| `debounce_secs` | u64 | `3` | Debounce window. Env: `WECOM_DEBOUNCE_SECS`. |
| `allow_all_users` | bool \| omit | `false` (deny-all) | Env: `WECOM_ALLOW_ALL_USERS`. |
| `allowed_users` | string[] | `[]` | WeCom UserIDs. Env: `WECOM_ALLOWED_USERS` (comma-separated). |

---

## Google Chat

Full first-class Google Chat section (config-first parity, #1379) — credentials, connection, and L3 identity trust. Each field resolves: config → `GOOGLE_CHAT_*` env → default.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Enable the adapter. Env: `GOOGLE_CHAT_ENABLED`. |
| `sa_key_json` | string | — | Inline service-account key JSON (wins over `sa_key_file`). Env: `GOOGLE_CHAT_SA_KEY_JSON`. |
| `sa_key_file` | string | — | Path to a service-account key file. Env: `GOOGLE_CHAT_SA_KEY_FILE`. |
| `access_token` | string | — | Static access token alternative. Env: `GOOGLE_CHAT_ACCESS_TOKEN`. |
| `audience` | string | — | JWT audience — enables webhook JWT verification (L1). Env: `GOOGLE_CHAT_AUDIENCE`. |
| `webhook_path` | string | `/webhook/googlechat` | Env: `GOOGLE_CHAT_WEBHOOK_PATH`. |
| `allow_all_users` | bool \| omit | `false` (deny-all) | Env: `GOOGLE_CHAT_ALLOW_ALL_USERS`. |
| `allowed_users` | string[] | `[]` | User resource names (`users/<id>`). Env: `GOOGLE_CHAT_ALLOWED_USERS`. |

---

## Teams

Full first-class Teams section (config-first parity, #1380) — credentials, connection, typed L2 scope, and L3 identity trust. Each field resolves: config → `TEAMS_*` env → default. `app_id` + `app_secret` are mandatory (after env fallback); an incomplete section disables the embedded adapter.

> ⚠️ **Commercial-public-cloud restriction:** Teams transport supports Microsoft commercial public cloud only. The adapter rejects non-HTTPS endpoints, userinfo, non-standard ports, sovereign-cloud hosts, custom proxy hosts, and service URLs outside `smba.trafficmanager.net`. Existing sovereign-cloud or proxy deployments must remain on an earlier release until an explicit cloud profile is available.
>
> Teams outbound replies require the bounded authenticated `event_id` route. A restart, route expiry, or capacity eviction causes a fail-closed `route_not_found`; the user must send a new activity. New Standalone peers advertise required send ACK and return the real Bot Framework activity ID, using `[gateway].gateway_ack_timeout_secs` as the Core wait budget. Edit/delete are permitted only for IDs in the process-local bot-owned index; restart or ownership expiry makes an older message immutable through OpenAB.
>
> `reactions_enabled` is an explicit opt-in to Microsoft's public-preview Bot Connector reaction endpoints. It requires no Graph/RSC permission, but the bot must be installed in the target scope. Disabled mode preserves the legacy reaction no-op.
>
> `processing_indicator = "message"` opts into one turn-local Bot Connector status message. It reuses negotiated real-ID send plus bot-owned edit/delete, remains separate from content streaming, and defaults to `off`. If reaction preview is also enabled, permanent queued receipts remain independent from the processing message.
>
> `streaming = true` opts into a separate progressive content placeholder. It is enabled only after Standalone hello (or the Unified Teams adapter) proves real-ID send plus bot-owned edit/delete with required ACKs. The generic `[gateway].streaming` and Telegram settings never enable Teams. Unknown write outcomes suppress recovery sends to avoid duplicates; no Graph/RSC grant is used. Microsoft 365 Personal live evidence covers ordinary progressive delivery, balanced long fenced-code overflow, explicit-reply correlation, restart/expiry rejection, and stop-on-Unknown for a middle overflow ACK loss. GroupChat, channel, cleanup-failure, live-`429`, and the remaining recovery branches stay open in the [live-validation tracker](msteams-live-validation.md).
>
> New Teams peers use a fixed 80,000 UTF-16-byte final-text budget based on Microsoft's conservative 80 KB recommendation; this is not a user setting. Old Gateway, no-hello, and unavailable Unified paths retain the 4,096-character fallback. Final chunks are sent sequentially under required ACK and stop at the first rejected or unknown outcome. Teams text-only messages do not render Markdown tables, so keep the default `[markdown].tables = "code"` or select `"bullets"`; `"off"` is an explicit raw-pipe bypass.
>
> `inbound_attachments = true` enables metadata-first Teams image/text ingress. Gateway publishes only bounded metadata and an opaque process-local reference; Core requests bytes only after structural, typed L2, and L3 identity admission. URLs, query strings, and Microsoft credentials stay in Gateway. Inline images work in all scopes; Personal `file.download.info` image and UTF-8 text files additionally require a separate manifest profile with `supportsFiles: true`. Group-chat/channel paperclip files remain unsupported without Graph. Standalone requires the opt-in on both processes plus a valid Gateway hello advertising the additive materialization capability; malformed switches, old peers, route expiry, restart, scope mismatch, and oversized data fail closed without retry.
>
> `conversation_registry_path` explicitly opts Gateway into restart-persistent Teams conversation references. Relative paths resolve beneath `$HOME/.openab/`; absence means no registry file, filesystem access, or advertised capability. Core sends only origin correlation after structural, typed L2, and L3 Allow; Gateway derives the record from its authenticated ephemeral route, and `serviceUrl` never enters Core or ACP. The versioned JSON file is capped at 16 MiB, written atomically, and protected as mode `0600` on Unix. Standalone operators must mount the selected Gateway path on durable storage themselves. The registry does not send by itself; an operator-owned Teams baseline `[[cron.jobs]]` entry may select one exact active record as documented below.
>
> Teams Personal, group-chat, and channel scope is derived from the authenticated Bot Framework activity. Presence of any of `allowed_teams`, `allowed_channels`, `allow_personal`, or `allow_group_chats` (or its environment variable) opts into typed L2 policy. With neither list populated, all Team channels are admitted; otherwise a Team **or** channel ID match admits the channel. Personal and group chats use their booleans. L3 user trust is still evaluated independently. The two boolean environment variables accept `true`/`false` or `1`/`0`; any other explicitly present value resolves to `false` (fail closed).
>
> If none of the typed fields is present, Core preserves the legacy untyped `[gateway].allowed_channels` / `GATEWAY_ALLOWED_CHANNELS` conversation-ID behavior for rolling upgrades. This fallback is logged. `ChannelInfo.id` remains the outbound conversation ID; typed scope never changes routing or session keys.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `app_id` | string | — | Azure AD app (bot) ID. Env: `TEAMS_APP_ID`. |
| `app_secret` | string | — | App client secret. Env: `TEAMS_APP_SECRET`. |
| `allowed_tenants` | string[] | `[]` (all) | Restrict to tenant IDs. Env: `TEAMS_ALLOWED_TENANTS`. |
| `oauth_endpoint` | string | Bot Framework | HTTPS endpoint on `login.microsoftonline.com`; use a tenant-specific path for single-tenant bots. Env: `TEAMS_OAUTH_ENDPOINT`. |
| `openid_metadata` | string | Bot Framework | HTTPS metadata endpoint on `login.botframework.com`. Env: `TEAMS_OPENID_METADATA`. |
| `webhook_path` | string | `/webhook/teams` | Env: `TEAMS_WEBHOOK_PATH`. |
| `dedupe_ttl_secs` | u64 | `600` | Process-local accepted-activity dedupe window. Must be greater than zero. Env: `TEAMS_DEDUPE_TTL_SECS`. |
| `route_ttl_secs` | u64 | `3600` | Gateway-local authenticated ingress route lifetime. Must be greater than zero. Env: `TEAMS_ROUTE_TTL_SECS`. |
| `max_route_entries` | usize | `10000` | Capacity bound applied independently to route, dedupe, and bot-owned outbound activity caches. Must be greater than zero. Env: `TEAMS_MAX_ROUTE_ENTRIES`. |
| `reactions_enabled` | bool | `false` | Enable public-preview add/remove reactions and advertise reaction availability. Env: `TEAMS_REACTIONS_ENABLED`. |
| `processing_indicator` | `off` \| `message` | `off` | Opt in to one processing message per admitted turn. Requires negotiated send/edit/delete ACK and real target support; malformed env values fail closed to `off`. Env: `TEAMS_PROCESSING_INDICATOR`. |
| `streaming` | bool | `false` | Opt in to progressive bot-owned content edits. Requires valid hello plus send/edit/delete ACK, real target IDs, and placeholder support; malformed env values fail closed to `false`. Env: `TEAMS_STREAMING`. |
| `inbound_attachments` | bool | `false` | Opt in to post-trust opaque-reference materialization for bounded inline images and Personal image/UTF-8 text files. Requires a valid negotiated capability; malformed env values fail closed. Env: `TEAMS_INBOUND_ATTACHMENTS`. |
| `conversation_registry_path` | string \| omit | — (disabled) | Opt in to the Gateway-local versioned registry. Relative paths resolve beneath `$HOME/.openab/`; unsafe traversal/symlink paths fail closed. Env: `TEAMS_CONVERSATION_REGISTRY_PATH`. |
| `conversation_registry_max_entries` | usize | `1000` | Independent persistent record cap, accepted range `1..=10000`. Env: `TEAMS_CONVERSATION_REGISTRY_MAX_ENTRIES`. |
| `conversation_registry_ttl_secs` | u64 | `31536000` | Active/disabled record retention; revoked tombstones remain bounded by capacity. Env: `TEAMS_CONVERSATION_REGISTRY_TTL_SECS`. |
| `allowed_teams` | string[] \| omit | `[]` (all Team channels when both lists are empty) | Team IDs admitted for channel conversations. If either scope list is non-empty, Team **or** channel match admits. Env: `TEAMS_ALLOWED_TEAMS` (comma-separated). |
| `allowed_channels` | string[] \| omit | `[]` (all Team channels when both lists are empty) | Teams channel IDs admitted for channel conversations. Env: `TEAMS_ALLOWED_CHANNELS` (comma-separated). |
| `allow_personal` | bool \| omit | `true` | Admit Personal conversations under typed policy. Env: `TEAMS_ALLOW_PERSONAL`. |
| `allow_group_chats` | bool \| omit | `true` | Admit group chats under typed policy. Env: `TEAMS_ALLOW_GROUP_CHATS`. |
| `allow_all_users` | bool \| omit | `false` (deny-all) | Independent L3 identity gate. Env: `TEAMS_ALLOW_ALL_USERS`. |
| `allowed_users` | string[] | `[]` | `activity.from.id` values (`29:…`). Env: `TEAMS_ALLOWED_USERS`. |

---

## Feishu/Lark

Full first-class Feishu/Lark section (config-first parity, #1377) — credentials, connection, behavior, and L3 identity trust. Each field resolves: config → `FEISHU_*` env → default. `app_id` + `app_secret` are mandatory (after env fallback); an incomplete section disables the adapter. The gateway adapter's parser remains the single source of truth — the section renders into the same form the env vars use, so defaults and enum rules cannot diverge.

| Key | Type | Default | Env |
|-----|------|---------|-----|
| `app_id` / `app_secret` | string | — (mandatory) | `FEISHU_APP_ID` / `FEISHU_APP_SECRET` |
| `verification_token` | string | — | `FEISHU_VERIFICATION_TOKEN` |
| `encrypt_key` | string | — (enables webhook signature, L1) | `FEISHU_ENCRYPT_KEY` |
| `domain` | string | `feishu` (`feishu`\|`lark`) | `FEISHU_DOMAIN` |
| `connection_mode` | string | `websocket` (`websocket`\|`webhook`) | `FEISHU_CONNECTION_MODE` |
| `webhook_path` | string | `/webhook/feishu` | `FEISHU_WEBHOOK_PATH` |
| `allowed_users` | string[] | `[]` (open_ids — per-app!) | `FEISHU_ALLOWED_USERS` |
| `allowed_groups` | string[] | `[]` | `FEISHU_ALLOWED_GROUPS` |
| `trusted_bot_ids` | string[] | `[]` | `FEISHU_TRUSTED_BOT_IDS` |
| `require_mention` | bool | `true` | `FEISHU_REQUIRE_MENTION` |
| `allow_bots` | string | `off` (`off`\|`mentions`\|`all`) | `FEISHU_ALLOW_BOTS` |
| `allow_user_messages` | string | `multibot_mentions` (`multibot_mentions`\|`mentions`\|`involved`) | `FEISHU_ALLOW_USER_MESSAGES` |
| `max_bot_turns` | u32 | `20` | `FEISHU_MAX_BOT_TURNS` |
| `dedupe_ttl_secs` | u64 | `300` | `FEISHU_DEDUPE_TTL_SECS` |
| `message_limit` | u64 | `4000` | `FEISHU_MESSAGE_LIMIT` |
| `session_ttl_hours` | u64 | `24` (`0` disables) | `FEISHU_SESSION_TTL_HOURS` |
| `card_streaming_mode` | string | `auto` (`auto`\|`post`\|`card`) | `FEISHU_CARD_STREAMING_MODE` |
| `card_fallback_to_post` | bool | `true` | `FEISHU_CARD_FALLBACK_TO_POST` |
| `card_promote_bytes` | u64 | `4000` | `FEISHU_CARD_PROMOTE_BYTES` |
| `card_idle_finalize_ms` | u64 | `3000` | `FEISHU_CARD_IDLE_FINALIZE_MS` |
| `allow_all_users` | bool \| omit | `false` (deny-all at the shared L3 gate) | `FEISHU_ALLOW_ALL_USERS` |

> The `[feishu]` section also feeds the shared trust registry (feishu was the last platform on the uniform `GATEWAY_*` seed). The gateway-side `allowed_users`/`allowed_groups` double-gate elimination is tracked on #1357.

---

## Agent

The AI agent subprocess that OpenAB spawns to handle messages via ACP.

> **This entire section is optional.** If omitted, `command` and `args` default from `$OPENAB_AGENT_COMMAND` (e.g. `"opencode acp"` — first token is command, rest are args). Each Docker image sets this env var so you typically don't need an `[agent]` block unless you want to override `env` or `args`.

**Resolution priority:** config `[agent].command`/`args` > `$OPENAB_AGENT_COMMAND` > `"openab-agent"`

> **Partial override rule:** Setting `command` without `args` resets args to `[]`. This prevents a custom command from inheriting the env var's args. To keep env-var args with a custom command, set both fields explicitly.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `command` | string | from `$OPENAB_AGENT_COMMAND` or `"openab-agent"` | Agent binary. Optional — defaults from image env var. |
| `args` | string[] | from `$OPENAB_AGENT_COMMAND` or `[]` | CLI arguments. Defaults to env var args only when `command` is also defaulted. |
| `working_dir` | string | `$HOME` | Working directory for the agent process. Optional — defaults to container's `$HOME`. |
| `env` | map | `{}` | Extra environment variables (e.g. `{ OPENAI_API_KEY = "${OPENAI_API_KEY}" }`). |
| `inherit_env` | string[] | `[]` | Env var names to inherit from the OAB process (e.g. vars injected via K8s `envFrom`). Keys in `env` take precedence. |

> **Default inherited vars:** After `env_clear()`, the agent always receives `HOME`, `PATH`, and `USER` (on Windows: `USERPROFILE`, `USERNAME`, `PATH`, `SystemRoot`, `SystemDrive`). Use `inherit_env` to pass additional vars beyond this baseline.

### Authentication

Each image sets `OPENAB_AGENT_AUTH_COMMAND` with the correct auth command. To authenticate any agent:

```bash
kubectl exec -it deployment/openab-<name> -- sh -c "$OPENAB_AGENT_AUTH_COMMAND"
```

This works for all agents regardless of backend — no need to remember the specific auth command.

### Agent examples

```toml
# Kiro CLI
[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"

# Claude Code
[agent]
command = "claude-agent-acp"
args = []
working_dir = "/home/node"
# Auth: kubectl exec -it deploy/openab-claude -- claude auth login
# Credentials persist in HOME PVC across restarts. See docs/claude-code.md.

# Codex
[agent]
command = "codex-acp"
working_dir = "/home/node"
env = { OPENAI_API_KEY = "${OPENAI_API_KEY}" }

# Recommended for containerized OpenAB deployments: the outer container is the
# security boundary; Codex's inner sandbox needs user namespaces containers
# typically don't grant. See docs/codex.md §ACP Modes and Migration.
[pool]
default_config_options = { mode = "agent-full-access" }

# Gemini CLI
[agent]
command = "gemini"
args = ["--acp"]
working_dir = "/home/node"
env = { GEMINI_API_KEY = "${GEMINI_API_KEY}" }

# GitHub Copilot
[agent]
command = "copilot"
args = ["--acp", "--stdio"]
working_dir = "/home/node"

# opencode
[agent]
command = "opencode"
args = ["acp"]
working_dir = "/home/node"

# Kimi Code CLI
[agent]
command = "kimi"
args = ["acp"]
working_dir = "/home/node"

# Pi Agent
[agent]
command = "pi-acp"
working_dir = "/home/node"

# Cursor Agent
[agent]
command = "cursor-agent"
args = ["acp", "--model", "auto", "--workspace", "/home/agent"]
working_dir = "/home/agent"

# Hermes Agent
[agent]
command = "hermes-acp"
working_dir = "/home/agent"
```

---

## Pool

Session pool settings for managing concurrent agent sessions.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `max_sessions` | usize | `10` | Maximum number of concurrent agent sessions. When full, the oldest idle session is suspended (recoverable); if all sessions are busy, new requests are rejected. |
| `session_ttl_hours` | u64 | `4` | Session time-to-live in hours. Idle sessions are reclaimed after this period. The example config uses `24`. |
| `hung_grace_secs` | u64 | `120` | Grace period after `prompt_hard_timeout_secs` before a session stuck with its connection mutex held (in-flight prompt) is force-evicted from the pool. Eviction threshold: `prompt_hard_timeout_secs + hung_grace_secs`. |
| `default_config_options` | map | `{}` | Config options to set automatically after session creation. Keys are config option IDs (e.g. `mode`, `model`), values are the desired values (e.g. `bypass`, `swe-1-6`). Sent via ACP `session/set_config_option` after each `session/new`. |

**Example** — force Devin to bypass mode and use a specific model:

```toml
[pool]
max_sessions = 3
session_ttl_hours = 1
default_config_options = { mode = "bypass", model = "swe-1-6" }
```

---

## Hooks

Lifecycle hooks that run at specific points during the container lifecycle. See [hooks.md](hooks.md) for full documentation and examples.

### hooks.pre_seed

Downloads and extracts archives from S3 before `pre_boot`. Seeds the agent environment with configs, tools, and shared memory without requiring AWS CLI in the image.

> `pre-seed` is enabled by default. No feature flag needed.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `sources` | string[] | `[]` | S3 URIs of archives (`.zip`, `.tar.gz`, `.tgz`). Max 5. Extracted in order; later layers overwrite earlier ones. |
| `target` | string | `$HOME` | Extraction target directory. |
| `max_bytes` | u64 | `104857600` | Max compressed archive size in bytes (100 MiB). Rejects downloads exceeding this. |
| `timeout_seconds` | u64 | `300` | Per-source download+extract timeout in seconds. |
| `on_failure` | string | `"abort"` | `"abort"` exits openab; `"warn"` logs and continues. |
| `region` | string | — | Override AWS region for S3 access. |
| `endpoint_url` | string | — | Override S3 endpoint URL (for LocalStack, VPC endpoints). |

**Credential resolution** uses the standard AWS provider chain (same as `config-s3` and `secrets-aws`):
environment variables, shared credentials, IRSA / EKS Pod Identity, ECS task role.

**Integrity verification:** If S3 objects are uploaded with `--checksum-algorithm SHA256`, OpenAB automatically verifies the checksum on download. No config needed — see [hooks.md](hooks.md) for details.

```toml
[hooks.pre_seed]
sources = [
  "s3://my-bucket/base-env.tar.gz",
  "s3://my-bucket/shared-memory.zip",
  "s3://my-bucket/agent-overrides.tgz",
]
timeout_seconds = 300
on_failure = "abort"
```

### hooks.pre_boot

Runs **before** agent pool creation. Use for bootstrapping files, syncing from S3, installing CLIs.

### hooks.pre_shutdown

Runs **after** pool shutdown on SIGTERM. Use for backing up state, syncing to S3.

Both hooks share the same fields:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `script` | string | — | Absolute path to an executable script. |
| `inline` | string | — | Script content (written to temp file and executed). |
| `url` | string | — | Remote script URL (max 1 MiB). |
| `sha256` | string | — | Required with `url` — hex-encoded SHA-256 checksum. |
| `timeout_seconds` | u64 | `60` | Max wall-clock seconds before the script is killed. |
| `on_failure` | string | `"abort"` | `"abort"` exits openab; `"warn"` logs and continues. |

> Exactly one of `script`, `inline`, or `url` must be set. `script` must be an absolute path. `url` requires `sha256`.

```toml
[hooks.pre_boot]
inline = '''
#!/bin/sh
set -e
aws s3 sync "$BOOTSTRAP_URI" "$HOME/"
'''
timeout_seconds = 120
on_failure = "abort"

[hooks.pre_shutdown]
inline = '''
#!/bin/sh
aws s3 sync "$HOME/" "s3://$STATE_BUCKET/$TASK_FAMILY/" \
  --exclude "aws-cli/*" --quiet
'''
timeout_seconds = 30
on_failure = "warn"
```

---

## Secrets

External secrets management. Secrets are resolved at boot time (after `pre_boot` hooks) and held in memory only — never written to disk. See [secrets-management.md](secrets-management.md) for full documentation.

### secrets.refs

Secret references. Each key maps to a provider URI. Resolved values are available as `${secrets.<key>}` in other config fields.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `<name>` | string | — | URI referencing an external secret. Supported schemes: `aws-sm://`, `exec://`. |

**URI formats:**

- `aws-sm://<secret-id>#<json-key>` — fetch from AWS Secrets Manager, extract JSON field
- `exec://<absolute-script-path> <key> <attribute>` — run script with two arguments, read stdout

### secrets.aws

AWS Secrets Manager provider configuration (optional).

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `region` | string | auto | Override AWS region. Defaults to SDK credential chain (env/IMDS/IRSA). |
| `endpoint_url` | string | — | Override endpoint URL (for LocalStack or VPC endpoints). |

### secrets.exec

Exec provider configuration (optional).

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `timeout_seconds` | u64 | `10` | Max seconds per script invocation before kill. |

```toml
[secrets.refs]
discord_token = "aws-sm://openab/prod#discord_bot_token"
openai_key    = "aws-sm://openab/prod#openai_api_key"
github_pat    = "exec:///home/agent/.local/bin/get-secret.sh vault/openab github_pat"

[secrets.aws]
region = "ap-northeast-1"

[secrets.exec]
timeout_seconds = 15

[discord]
bot_token = "${secrets.discord_token}"
```

---

## Reactions

Emoji reaction feedback on messages to show agent processing status.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Enable/disable reaction feedback. |
| `remove_after_reply` | bool | `false` | Remove the status reaction after the agent replies. |
| `tool_display` | string | `"full"` | How tool calls are rendered: `"full"` (complete title), `"compact"` (count summary, e.g. `✅ 3 · 🔧 1 tool(s)`), or `"none"` (hidden). |

### reactions.emojis

Customize the emoji for each processing stage.

| Key | Default | Description |
|-----|---------|-------------|
| `queued` | 👀 | Message received, queued for processing. |
| `thinking` | 🤔 | Agent is thinking / generating. |
| `tool` | 🔥 | Agent is calling a tool. |
| `coding` | 👨‍💻 | Agent is writing code. |
| `web` | ⚡ | Agent is doing web operations. |
| `done` | 🆗 | Agent finished successfully. |
| `error` | 😱 | Agent encountered an error. |

### reactions.timing

Fine-tune reaction timing behavior (milliseconds).

| Key | Default | Description |
|-----|---------|-------------|
| `debounce_ms` | `700` | Debounce interval before updating the reaction emoji. |
| `stall_soft_ms` | `10000` | Soft stall threshold — warn if no progress. |
| `stall_hard_ms` | `30000` | Hard stall threshold — consider the agent stuck. |
| `done_hold_ms` | `1500` | How long to show the done emoji before removing (if `remove_after_reply`). |
| `error_hold_ms` | `2500` | How long to show the error emoji before removing. |

### reactions.mapping

Map emoji reactions to text commands. When a user reacts with a configured emoji on any message in a monitored channel, the bot treats it as if the user sent the corresponding text message.

Keys can be unicode emoji or Discord/GitHub shortcodes (e.g. `:thumbsup:`). Shortcodes are resolved to unicode at config load time.

```toml
[reactions.mapping]
"👍" = "OK"
":thumbsdown:" = "不行"
":arrows_counterclockwise:" = "重新 review"
":white_check_mark:" = "approve"
```

**Requirements:**

- Enable the `GUILD_MESSAGE_REACTIONS` intent in the Discord Developer Portal.
- Only unicode emoji are supported (custom server emoji are ignored).
- The bot's own reactions are always ignored (prevents feedback loops).
- Channel/thread access control still applies — reactions in non-monitored channels are ignored.

---

## STT

Speech-to-text transcription for voice messages. Uses an OpenAI-compatible `/audio/transcriptions` endpoint.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Enable voice message transcription. |
| `api_key` | string | `""` | API key for the STT service. When empty and `base_url` contains `groq.com`, the `GROQ_API_KEY` environment variable is used automatically. For local servers, use `api_key = "not-needed"`. |
| `model` | string | `"whisper-large-v3-turbo"` | Model name to use for transcription. |
| `base_url` | string | `"https://api.groq.com/openai/v1"` | Base URL of the STT API. Any OpenAI-compatible `/audio/transcriptions` endpoint works. |
| `echo_transcript` | bool | `false` | When set to `true` and STT runs, post a `> 🎤 <transcript>` message to the thread before the agent reply so users can verify what was heard. Failures show `(transcription failed)` and add a ⚠️ reaction to the original message. |

---

## Workspace

Workspace aliases for [Control Directives](adr/control-directives.md). Users specify `[[ws:@alias]]` in their first message to set the session's working directory.

```toml
[workspace.aliases]
openab = "~/projects/openab"
infra  = "~/projects/infra-cdk"
web    = "~/projects/frontend"
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `aliases` | map | `{}` | Key-value map of alias name → path. Users reference with `@` prefix: `[[ws:@openab]]`. Paths starting with `~` expand to `$HOME`. All paths must be within the bot's home directory (security boundary). |

**Security:**

- Relative paths are rejected
- `~` expands to bot home (`$HOME`)
- Paths are canonicalized and must be within bot home subtree
- Symlink escapes are caught by canonicalization
- Target must be an existing directory (not a file)

---

## Ambient

Passive channel listening with batch flush. See [ambient.md](ambient.md) for full guide.

```toml
[ambient]
enabled = false                   # Master switch
flush_interval_seconds = 60       # Time trigger (±20% jitter)
flush_max_messages = 10           # Count trigger
flush_hard_cap = 50               # Max buffer size
max_concurrent_flushes = 3        # Global LLM concurrency limit
flush_timeout_seconds = 120       # Safety timeout per flush
context_window = 20               # (v2, not yet implemented)

[ambient.pool]                    # (v2, not yet enforced)
max_sessions = 5
session_ttl_minutes = 60
context_flushes = 3

[ambient.discord]
channels = []                     # Channel ID allowlist — and their threads (required)
allow_bot_messages = true
```

---

## Filestore

Optional S3/R2-compatible object store for handling file attachments.

When configured, text files exceeding the 512 KB inline limit are uploaded to the
object store and a presigned GET URL is returned to the agent. This eliminates
the silent-drop behavior for large files and works with any agent that can perform
HTTP GET (no platform auth tokens required).

```toml
[filestore]
bucket = "my-oab-files"
region = "us-west-2"
# endpoint = "https://<account_id>.r2.cloudflarestorage.com"  # Cloudflare R2
# endpoint = "http://localhost:9000"                           # MinIO
prefix = "incoming/"       # object key prefix (default: "incoming/")
presigned_ttl = 3600       # URL expiry in seconds (default: 3600 = 1 hour)
# max_file_size_mb = 250   # max upload size in MB (default: 250, max: 500)
# access_key_id = "${secrets.filestore_key}"         # recommended: use secret refs
# secret_access_key = "${secrets.filestore_secret}"  # recommended: use secret refs
```

> **Credentials best practice:** For R2 and explicit S3 credentials, always use
> `[secrets.refs]` to resolve credentials from AWS Secrets Manager or an exec
> provider. Avoid hardcoding credentials or relying solely on env vars in production.
> For AWS S3 with IRSA/Pod Identity/instance roles, omit both fields entirely.

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `bucket` | ✅ | — | S3 bucket name |
| `region` | ✅ | — | AWS region (use `"auto"` for Cloudflare R2) |
| `endpoint` | ❌ | AWS default | Custom S3-compatible endpoint URL (R2, MinIO, etc.) |
| `prefix` | ❌ | `"incoming/"` | Object key prefix for uploaded files |
| `presigned_ttl` | ❌ | `3600` | Presigned URL expiry in seconds |
| `max_file_size_mb` | ❌ | `250` | Maximum file size for upload in MB (hard cap: 500) |
| `access_key_id` | ❌ | provider chain | Explicit access key (falls back to IRSA/env/config) |
| `secret_access_key` | ❌ | provider chain | Explicit secret key |

**Behavior when configured:**

- Text files ≤ 512 KB: inlined into the prompt as before (unchanged)
- Text files > 512 KB: downloaded by OAB, uploaded to S3/R2, presigned URL returned
- PDF, ZIP, binary, and other unsupported formats (Discord/Slack): uploaded to S3/R2, presigned URL returned
- The presigned URL requires no authentication — any HTTP GET works
- File count cap (5 files) still applies
- Aggregate 1 MB cap only applies to inlined files; filestore uploads bypass it

**Behavior when NOT configured (default):**

- Text files > 512 KB and unsupported formats are silently dropped (existing behavior)

**Supported backends:**

- AWS S3
- Cloudflare R2 (S3-compatible, zero egress fees)
- MinIO
- Any S3-compatible object store

**Build requirement:** The filestore feature is enabled by default in standard builds. When built without it (e.g. `--no-default-features`), the `[filestore]` config section is ignored and all behavior is unchanged.

**Minimum IAM policy:**

```json
{
  "Effect": "Allow",
  "Action": [
    "s3:PutObject",
    "s3:GetObject",
    "s3:AbortMultipartUpload",
    "s3:ListMultipartUploadParts"
  ],
  "Resource": "arn:aws:s3:::my-oab-files/incoming/*"
}
```

For Cloudflare R2, use the equivalent R2 API token with Object Read & Write
permissions scoped to the bucket.

---

## Cron

Everything cron-related lives under `[cron]`.

```toml
[cron]
usercron_enabled = true                      # enable hot-reload (default: false)
usercron_path = "cronjob.toml"               # relative to $HOME/.openab/, or absolute

[[cron.jobs]]
enabled = true                               # optional, default: true
schedule = "0 9 * * 1-5"                    # cron expression (5-field POSIX)
channel = "123456789"                        # target channel/thread ID
message = "summarize yesterday's merged PRs" # message sent to agent
platform = "discord"                         # optional, default: "discord"
sender_name = "DailyOps"                     # optional, default: "openab-cron"
timezone = "America/New_York"                # optional, default: "UTC"
thread_id = ""                               # optional, post to existing thread

[[cron.jobs]]
schedule = "0 0 * * 0"
channel = "123456789"
message = "generate weekly status report"
platform = "discord"
timezone = "UTC"

# Teams is operator-baseline-only and resolves through the Gateway registry.
[[cron.jobs]]
schedule = "0 9 * * 1-5"
platform = "teams"
channel = "<teams-conversation-id>"
teams_tenant_id = "<tenant-id>"
message = "summarize yesterday's merged work"
sender_name = "DailyOps"
timezone = "Asia/Taipei"
```

### Cron fields

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `usercron_enabled` | bool | `false` | Enable usercron hot-reload. Must be explicitly set to `true`. |
| `usercron_path` | string | — | Path to the external `cronjob.toml`. Relative paths resolve from `$HOME/.openab/`. |

### Baseline cron.jobs fields

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Set `false` to disable without removing the entry. |
| `schedule` | string | *required* | Cron expression (minute, hour, day-of-month, month, day-of-week). |
| `channel` | string | *required* | Platform destination. For Teams this is the exact trusted conversation ID, bounded to 2,048 bytes. |
| `teams_tenant_id` | string | *required for Teams* | Exact Teams tenant ID, bounded to 256 bytes. Invalid on every other platform. |
| `message` | string | *required* | Message sent to the agent as a prompt. |
| `platform` | string | `"discord"` | `"discord"`, `"slack"`, `"telegram"`, `"googlechat"`, `"lineworks"`, or `"teams"`. |
| `sender_name` | string | `"openab-cron"` | Sender attribution shown in the prompt context. |
| `timezone` | string | `"UTC"` | IANA timezone for schedule evaluation (e.g. `"America/New_York"`, `"Europe/Berlin"`). |
| `thread_id` | string | `""` | Optional existing thread for supported platforms. Teams rejects this field and never creates a scheduler thread. |

Teams jobs require an available
[trusted persistent conversation registry](adr/teams-trusted-persistent-conversation-registry.md)
and a Gateway peer that advertises persistent-send support. Gateway combines
configured app identity with `teams_tenant_id`, the fixed `msteams` transport,
and `channel`; only an exact active, non-expired record is eligible. The visible
trigger must return a real Bot Framework activity ID before OpenAB starts or
reuses an ACP session. `serviceUrl` and the stored record remain Gateway-local.

The external `cronjob.toml` uses `[[jobs]]` for non-Teams jobs. Every usercron entry with `platform = "teams"` or `teams_tenant_id` is rejected before lookup or ACP work. See [Usercron docs](cronjob.md#usercron--hot-reload-with-cronjobtoml) for details.

### Usercron-only `[[jobs]]` fields

These fields are valid only in the external usercron file, for example `$HOME/.openab/cronjob.toml`. They are rejected in baseline `[[cron.jobs]]` because OpenAB only writes state back to the user-managed cron file.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `id` | string | *required with `disable_on_success`* | Stable job ID used when the scheduler writes `enabled = false` or `thread_id` back to `cronjob.toml`. |
| `disable_on_success` | string | — | Command to run before sending the scheduled prompt. |
| `disable_on_success_match` | string | *required with `disable_on_success`* | Marker that must appear in stdout or stderr, in addition to exit code `0`, before the job is considered complete. |
| `disable_on_success_timeout_secs` | integer | `60` | Timeout for the completion check command. |
| `disable_on_success_working_dir` | string | — | Working directory for the completion check command. |

Example:

```toml
[[jobs]]
id = "fix-unit-tests"
enabled = true
schedule = "*/10 * * * *"
channel = "123456789"
message = "Unit tests are still failing. Continue fixing them."
disable_on_success = "npm test && echo OPENAB_GOAL_SUCCESS"
disable_on_success_match = "OPENAB_GOAL_SUCCESS"
disable_on_success_timeout_secs = 120
disable_on_success_working_dir = "/workspace/my-project"
```

**Cron expression format:**

```
┌───────────── minute (0-59)
│ ┌───────────── hour (0-23)
│ │ ┌───────────── day of month (1-31)
│ │ │ ┌───────────── month (1-12)
│ │ │ │ ┌───────────── day of week (0-7, 0 and 7 = Sunday)
│ │ │ │ │
* * * * *
```

**Behaviors:**

- Scheduler evaluates expressions once per minute
- If a previous execution is still running, the next tick is skipped (no overlap)
- Failed executions are logged but do not block other jobs or chat traffic
- Stateless — no scheduler persistence or catch-up; schedules are re-evaluated from config on restart
- Teams sends are threadless, required-ACK, and never retried after an ambiguous outcome

> **Helm:** the chart schema in this source tree no longer renders
> `agents.<name>.cronjobs`. Put the complete baseline block in
> `agents.<name>.configToml`, or provide it through `configUrl`. This is
> intentional: Teams tenant/conversation authority has no second Helm values
> surface.

---

## Customizing via Helm

The chart schema in this source tree treats `config.toml` as an authoritative
raw document. Supply it through `agents.<name>.configToml`, load it with
`--set-file`, or use `agents.<name>.configUrl`; the chart no longer maps
adapter, pool, or cron fields into TOML.

```yaml
agents:
  kiro:
    configToml: |
      [discord]
      bot_token = "${DISCORD_BOT_TOKEN}"
      allowed_channels = ["1234567890"]

      [agent]
      command = "kiro-cli"
      args = ["acp", "--trust-all-tools"]

      [[cron.jobs]]
      schedule = "0 9 * * 1-5"
      channel = "1234567890"
      message = "summarize yesterday's merged PRs"
```

For a standalone file:

```bash
helm upgrade --install mybot charts/openab \
  --set-file agents.kiro.configToml=./config.toml
```

This raw contract is especially important for Teams: `teams_tenant_id` and the conversation ID exist only in baseline `[[cron.jobs]]`; Gateway credentials, registry path, and persisted `serviceUrl` stay on the Gateway side. See [`migrate-to-configtoml.md`](migrate-to-configtoml.md) and `charts/openab/values.yaml` for deployment-only values such as persistence, images, resources, and volume mounts.

---

## Environment variable interpolation

Any value can reference environment variables with `${VAR_NAME}`:

```toml
bot_token = "${DISCORD_BOT_TOKEN}"
```

Undefined variables resolve to an empty string.

---

## Unified Mode Environment Variables

When running with `BUILD_MODE=unified`, the binary embeds a webhook server for gateway platforms. These env vars control its behavior:

### Server

| Variable | Default | Description |
|----------|---------|-------------|
| `GATEWAY_LISTEN` | `0.0.0.0:8080` | Bind address for the embedded webhook server |

### Security Gating

| Variable | Default | Description |
|----------|---------|-------------|
| `GATEWAY_ALLOW_ALL_CHANNELS` | `true` | Accept events from any channel. **Set to `false` in production** and use `GATEWAY_ALLOWED_CHANNELS`. |
| `GATEWAY_ALLOWED_CHANNELS` | *(empty)* | Comma-separated channel IDs to allow (when `GATEWAY_ALLOW_ALL_CHANNELS=false`) |
| `GATEWAY_ALLOW_ALL_USERS` | `true` | Accept events from any user. **Set to `false` in production** and use `GATEWAY_ALLOWED_USERS`. |
| `GATEWAY_ALLOWED_USERS` | *(empty)* | Comma-separated user IDs to allow (when `GATEWAY_ALLOW_ALL_USERS=false`) |
| `GATEWAY_ALLOW_BOT_MESSAGES` | `false` | Allow messages from all bots (for multi-agent scenarios) |
| `GATEWAY_TRUSTED_BOT_IDS` | *(empty)* | Comma-separated bot IDs to allow even when `GATEWAY_ALLOW_BOT_MESSAGES=false` |
| `GATEWAY_BOT_USERNAME` | *(empty)* | Bot's username for @mention detection in groups |

### Platform Adapters

Each platform is auto-enabled when its env vars are present:

| Platform | Required Env Var | Optional |
|----------|-----------------|----------|
| Telegram | `TELEGRAM_BOT_TOKEN` | `TELEGRAM_SECRET_TOKEN`, `TELEGRAM_WEBHOOK_PATH`, `TELEGRAM_RICH_MESSAGES` |
| LINE | `LINE_CHANNEL_SECRET` | `LINE_CHANNEL_ACCESS_TOKEN` |
| Feishu | `FEISHU_APP_ID` | `FEISHU_WEBHOOK_PATH` |
| Google Chat | `GOOGLE_CHAT_ENABLED=true` | `GOOGLE_CHAT_SA_KEY_JSON`, `GOOGLE_CHAT_SA_KEY_FILE`, `GOOGLE_CHAT_ACCESS_TOKEN`, `GOOGLE_CHAT_AUDIENCE`, `GOOGLE_CHAT_WEBHOOK_PATH` |
| WeCom | `WECOM_CORP_ID` | *(see wecom config)* |
| Teams | `TEAMS_APP_ID` | `TEAMS_WEBHOOK_PATH` |

> ⚠️ **Production checklist**: Set `GATEWAY_ALLOW_ALL_CHANNELS=false` and `GATEWAY_ALLOW_ALL_USERS=false` with explicit allowlists. The defaults are permissive for development convenience.
>
> ⚠️ **Google Chat JWT**: When `GOOGLE_CHAT_AUDIENCE` is unset, webhook requests are **not** verified via JWT. Set this to your Google Chat app's project number or service account email in production to enable request authentication. If `GOOGLE_CHAT_SA_KEY_FILE` is set but the file cannot be read, the adapter starts without token authentication (warn logged).

---

## Maintaining This Reference

- **Trigger:** any change to config structs, environment fallbacks, defaults,
  validation rules, supported platforms, or Helm's raw-config contract.
- **Action:** update the affected table from source and run:

  ```bash
  cargo test -p openab-core config
  cargo test -p openab-gateway schema
  cargo test --manifest-path crates/platform-schema/Cargo.toml
  ```

- **Why:** source parsers and validators are authoritative; this manually curated
  reference must not preserve superseded config phases or release assumptions.
