# Discord — platform notes

**Schema version:** 2026-07-04

Engineering-facing capability & quirks reference for the Discord adapter. For operator setup see `docs/discord.md`. Follows the schemas in [`README.md`](./README.md).

## 1. Platform capability (`platform-capability`)

| Field | Value | Source |
|---|---|---|
| transport | WebSocket Gateway (persistent WSS `wss://gateway.discord.gg/`). The bot opens a persistent gateway connection and receives events; outbound actions use the REST HTTP API. | [Gateway](https://docs.discord.com/developers/topics/gateway) |
| inbound_auth | Gateway handshake via Identify (opcode 2) carrying the bot token + intents. REST calls use the `Authorization: Bot <token>` header. Gateway is a bot-initiated persistent socket, so there is no per-event inbound signature to verify (unlike webhook platforms). | [Gateway](https://docs.discord.com/developers/topics/gateway) · [Reference](https://docs.discord.com/developers/reference) |
| threads | native. ANNOUNCEMENT_THREAD (10) / PUBLIC_THREAD (11) / PRIVATE_THREAD (12) — temporary sub-channels of a text/forum/announcement channel (types available in API v9+). Threads are themselves channels (own `channel_id`, `thread_metadata`, `parent_id`). | [Channel](https://docs.discord.com/developers/resources/channel) |
| slash_commands | supported. Application commands of type CHAT_INPUT (1), registered over HTTP: global `POST /applications/{id}/commands` or per-guild `POST /applications/{id}/guilds/{gid}/commands`. Invocations delivered as `INTERACTION_CREATE` with an interaction token for the response. | [Application Commands](https://docs.discord.com/developers/interactions/application-commands) |
| mentions | Bot detects being addressed via user mention `<@user_id>` (and legacy nick form `<@!id>`) or role mention `<@&role_id>` in `mention_roles`. Gateway also flags `mentions` / `mention_everyone`. | [Message](https://docs.discord.com/developers/resources/message) |
| emoji_reactions | Bot **add**: `PUT` Create Reaction; **remove**: Delete Own Reaction (own) or delete others' with MANAGE_MESSAGES. **Receives**: `MESSAGE_REACTION_ADD` / `MESSAGE_REACTION_REMOVE` (needs GUILD_MESSAGE_REACTIONS intent `1 << 10`; DIRECT_MESSAGE_REACTIONS for DMs). | [Message](https://docs.discord.com/developers/resources/message) · [Gateway](https://docs.discord.com/developers/topics/gateway) |
| edit_message | Yes — a bot may edit its own messages via Edit Message. | [Message](https://docs.discord.com/developers/resources/message) |
| delete_message | Own: always. Others': requires MANAGE_MESSAGES permission. | [Message](https://docs.discord.com/developers/resources/message) |
| rich_content | Markdown, embeds, and message components (buttons, string/select menus, action rows). | [Message](https://docs.discord.com/developers/resources/message) |
| attachments | Inbound & outbound arbitrary file types. Default upload cap **10 MiB per file** (raised by uploader Nitro status or server Boost tier). | [Reference](https://docs.discord.com/developers/reference) |
| message_length_limit | 2000 characters per message content. | [Channel](https://docs.discord.com/developers/resources/channel) |
| dm_support | Yes — 1:1 DM channels (private channels). | [Channel](https://docs.discord.com/developers/resources/channel) |
| group_model | Guild → channels (GUILD_TEXT, forum, announcement, voice) → threads. Plus DM / group-DM private channels. Threads are channels with a `parent_id`. | [Channel](https://docs.discord.com/developers/resources/channel) |
| group_sender_identity | Yes — stable per-user snowflake `author.id` on every message; not consent-gated. Requires the MESSAGE_CONTENT intent to also read message text. | [Message](https://docs.discord.com/developers/resources/message) · [Gateway](https://docs.discord.com/developers/topics/gateway) |
| send_model | push. No reply-window/TTL — a bot with channel access may send at any time via REST. Replies are opt-in via `message_reference{message_id}`. | [Message](https://docs.discord.com/developers/resources/message) |
| proactive_push | Yes — unsolicited sends allowed within permissions. Global cap **50 requests/sec/bot**; per-route buckets (`X-RateLimit-Bucket`); invalid-request cap 10,000/10min. | [Rate Limits](https://docs.discord.com/developers/topics/rate-limits) |
| bot_to_bot | Yes — the gateway delivers other bots' messages; the `author.bot` flag distinguishes them (the bot's own messages arrive too and must be self-filtered). | [Gateway](https://docs.discord.com/developers/topics/gateway) |
| typing_indicator | Supported — `POST /channels/{id}/typing` (Trigger Typing); inbound `TYPING_START` event. | [Gateway](https://docs.discord.com/developers/topics/gateway) |

## 2. OpenAB feature support (`openab-feature-support`)

| Feature | Status | Note | Ref |
|---|---|---|---|
| send_message | implemented | `ChannelId::say`; `resolve_channel` prefers `thread_id` over `channel_id`; `message_limit()` = 2000. | `discord.rs:70`, `discord.rs:55`, `discord.rs:66` |
| message_split/chunking | implemented | Router reads `message_limit()` (2000) then splits: `split_delivery` handles directive/body, `format::split_message` chunks the body, mentions are propagated to each chunk. | `adapter.rs:686`, `adapter.rs:1105`, `adapter.rs:149` |
| streaming | implemented | Post-then-edit, not native. Discord uses the edit-loop: `use_streaming` returns true only when no other bot is present (`!other_bot_present`); the router consults it at dispatch. `uses_native_streaming` stays default false, so the trait's native stream methods only hit their edit-based fallbacks. | `discord.rs:136`, `adapter.rs:687`, `adapter.rs:361`, `adapter.rs:380` |
| reply/quote | implemented | `send_message_with_reply` sets `reference_message`; falls back to plain send on invalid id (parses to 0) or reply failure (unknown/cross-channel message). | `discord.rs:83` |
| edit_message | implemented | Native `EditMessage.content` (overrides the trait default that returns "edit_message not supported"). | `discord.rs:123`, `adapter.rs:330` |
| delete_message | implemented | Native `http.delete_message` (overrides trait default which edits to a zero-width space `\u{200b}`). | `discord.rs:114`, `adapter.rs:353` |
| emoji_reactions | implemented | `add_reaction` = `create_reaction`, `remove_reaction` = `delete_reaction_me`. Unicode emoji only. | `discord.rs:164`, `discord.rs:177` |
| threads/topics | implemented | `create_thread` builds a thread from the trigger message via serenity `create_thread_from_message` (1-day auto-archive); auto-thread on first channel message via `get_or_create_thread`. Not the gateway `create_topic` path — the native adapter creates threads directly. | `discord.rs:140`, `discord.rs:2725` |
| media_inbound | implemented | Attachments processed inline in the per-attachment loop: images encoded (`download_and_encode_image`), text files (≤1 MB total, ≤5 files), video passed as a URL block; non-image files warned to the user. | `discord.rs:850`, `discord.rs:911` |
| voice_stt | implemented | Audio attachments transcribed via `media::download_and_transcribe` when `stt_config.enabled`; transcript injected + echoed; 🎤 reaction when STT disabled. | `discord.rs:852`, `discord.rs:855`, `discord.rs:883` |
| trust_gate | implemented | Two layers: adapter-level channel/user allowlist (`allowed_channels`, `is_denied_user`) + shared L3 identity gate `router.gate_incoming` (humans only; bots bypass via `l3_gate_applies`). | `discord.rs:803`, `discord.rs:1048`, `discord.rs:2922` |
| deny_echo | partial | On a denied user the bot reacts 🚫 on the offending message and drops it — no text reply (Discord L3 denies drop silently apart from the reaction). | `discord.rs:803` |
| mention_gating | implemented | `AllowUsers` modes: Mentions (always require @), Involved (skip @ if bot owns/participated in thread), MultibotMentions (require @ when other bots present). DMs treated as an implicit mention. | `discord.rs:759`, `discord.rs:456` |
| slash_commands | implemented | Global commands registered on `ready` via `set_global_commands`: /models, /agents, /cancel, /cancel-all, /reset, /remind, /auth, /export-thread; dispatched via `interaction_create`. | `discord.rs:1334`, `discord.rs:1386`, `discord.rs:1423` |
| multibot | implemented | Early other-bot detection cached (disk-persisted, irreversible); disables streaming, gates via MultibotMentions, enforces bot-turn limits; `trusted_bot_ids` + @mention admits handoff regardless of `allow_bot_messages`. | `discord.rs:331`, `discord.rs:593`, `discord.rs:2947` |
| group_routing | implemented | Per-thread dispatch keyed by `dispatcher.key("discord", channel_id, sender_id)`; thread↔parent allowlist via `detect_thread`; ambient mode buffers passive-channel messages. | `discord.rs:1066`, `discord.rs:2901`, `discord.rs:530` |

## 3. Platform quirks (`platform-quirks`)

### Threads are channels
A Discord thread has its own `channel_id`; the adapter resolves outbound targets via `thread_id.unwrap_or(channel_id)` (`resolve_channel`, `discord.rs:55`). Thread identity is `thread_metadata.is_some()` — `parent_id` alone is NOT reliable (category children also carry `parent_id`), so `detect_thread` returns early unless `has_thread_metadata`, and only uses `parent_id` for the allowlist check (`discord.rs:2901`).

### Self-echo and bot-loop control
The bot receives its own messages over the gateway and must self-filter (`msg.author.id == bot_id`, `discord.rs:443`). Because multiple bots can ping-pong, there are layered guards: a hard consecutive-bot cap (`MAX_CONSECUTIVE_BOT_TURNS = 1000`), a configurable soft per-thread `max_bot_turns` reset by any human message, and `BotTurnTracker`. Bot-turn counting deliberately runs *before* the self-check (`discord.rs:341`) so all bot messages count, but warning posts respect the channel allowlist + prior participation to avoid uninvolved bots spamming (`discord.rs:402`).

### Multibot detection is irreversible & disk-cached
Once any other bot posts in a channel/thread, that thread is permanently "multibot": cached in-memory and persisted to `MultibotCache` on disk (survives restarts), since bot messages don't disappear (`discord.rs:331`, `discord.rs:227`). This flips streaming off (the edit-loop interferes across bots) and can require @mention under `MultibotMentions`.

### Streaming is post-then-edit, not native
Unlike Slack, Discord has no native streaming API; OpenAB streams by editing a placeholder message. `use_streaming` disables this whenever another bot is present, to avoid edit interference (`discord.rs:136`, #534). `uses_native_streaming` stays false (`adapter.rs:361`), so the trait's native stream methods only ever hit their edit-based fallbacks.

### create_topic vs create_thread
The shared gateway layer has a `create_topic` command (`gateway.rs:513`) for gateway-protocol adapters. The native Discord adapter does NOT use it — it calls serenity `create_thread_from_message` directly (`discord.rs:148`) and auto-creates a thread for top-level channel messages via `get_or_create_thread`.

### Attachment handling caps
Text-file attachments are bounded independently of Discord's own 10 MiB upload cap: 1 MB total across all text files (`TEXT_TOTAL_CAP`) and max 5 files per message (`TEXT_FILE_COUNT_CAP`), enforced with a Discord-reported-size pre-check before download (`discord.rs:847`, `discord.rs:887`). Image URLs from Discord expire ~24h, which is surfaced to the agent in the injected block (`discord.rs:925`).

### DMs
DM channels can't hold threads, so DMs reuse the DM channel directly and are treated as an implicit @mention; gated only by `allow_dm` + user allowlist (`should_process_dm` / `should_skip_thread_creation`, `discord.rs:680`, `discord.rs:967`).

### Findings log
- 2026-07-04 (A) Default file-upload cap is 10 MiB/file (raised by Nitro/Boost); the adapter's own text-attachment caps (1 MB total / 5 files) are stricter. [Reference](https://docs.discord.com/developers/reference)
- 2026-07-04 (A) Global REST rate limit is 50 req/sec/bot plus per-route buckets (`X-RateLimit-Bucket`); invalid requests capped at 10,000/10min — relevant to proactive-push and streaming edit-loop cadence. [Rate Limits](https://docs.discord.com/developers/topics/rate-limits)
- 2026-07-04 (A) Message content hard limit is 2000 chars, matching `DiscordAdapter::message_limit()`; the router chunks longer replies. [Channel](https://docs.discord.com/developers/resources/channel)
- 2026-07-04 (A) Thread channel types are ANNOUNCEMENT_THREAD (10) / PUBLIC_THREAD (11) / PRIVATE_THREAD (12), API v9+; identified by `thread_metadata`, not `parent_id` (category children also carry `parent_id`); `detect_thread` follows this. [Channel](https://docs.discord.com/developers/resources/channel)
- 2026-07-04 (A) Transport is a bot-initiated persistent WebSocket Gateway (WSS) authenticated by Identify(op 2) + bot token + intents — no per-event inbound signature to verify (unlike webhook platforms). [Gateway](https://docs.discord.com/developers/topics/gateway)
- 2026-07-04 (B) Section-2 ref audit: chunking lives at `adapter.rs:686`/`:1105` (not the trait def near `:306`); slash-command registration is `set_global_commands` at `discord.rs:1386`; `is_denied_user` at `discord.rs:2922`, trusted-bot bypass at `discord.rs:2947`. Corrected stale line refs. [PR #TBD]
