# WeCom (企業微信) — platform notes

**Schema version:** 2026-07-04

Engineering-facing capability & quirks reference for the WeCom adapter. For operator setup see `docs/wecom.md`. Follows the schemas in [`README.md`](./README.md).

WeCom is integrated via the **self-built app (自建应用 / agentid) callback model**, not the newer "智能机器人 / 群机器人" model. The adapter (`crates/openab-gateway/src/adapters/wecom.rs`) receives a user's 1:1 message via an AES-encrypted callback and replies proactively via `/cgi-bin/message/send`.

## 1. Platform capability (`platform-capability`)

| Field | Value | Source |
|---|---|---|
| `transport` | webhook (HTTP callback; WeCom POSTs AES-encrypted XML to the configured callback URL) | [self-built app receive overview](https://developer.work.weixin.qq.com/document/path/90238) |
| `inbound_auth` | L1: `msg_signature` = SHA1 of `sort(token, timestamp, nonce, encrypt).concat()`; body is AES-256-CBC decrypted with the 43-char `EncodingAESKey` (base64→32 bytes), IV = first 16 key bytes, WeCom PKCS7 block_size=32; inner corp_id suffix validated (`decrypt_message`, `wecom.rs:99` sig, `wecom.rs:125` decrypt) | [receive / callback doc](https://developer.work.weixin.qq.com/document/path/90238) |
| `threads` | none — self-built app callback is a flat 1:1 conversation; no thread/topic primitive | [receive msg doc](https://developer.work.weixin.qq.com/document/path/90239) |
| `slash_commands` | Not a platform primitive. No command registration/delivery; text like `/reset` arrives as ordinary `text` content and any interpretation is OpenAB-side | [receive msg doc](https://developer.work.weixin.qq.com/document/path/90239) |
| `mentions` | n/a in the self-built app 1:1 model — every callback is a direct message from one `FromUserName`; no @mention concept. (Group @mention exists only under the separate 智能机器人 model, which this adapter does not use) | [receive msg doc](https://developer.work.weixin.qq.com/document/path/90239) |
| `emoji_reactions` | No reaction API for self-built apps: bot cannot **add** or **remove** reactions, and reaction events are **not** delivered. The receive doc enumerates only six inbound callback types: text, image, voice, video, location, link (no reaction, no edit) | [receive msg doc](https://developer.work.weixin.qq.com/document/path/90239) |
| `edit_message` | No edit API. The only mutation is **recall** (`/cgi-bin/message/recall`), which deletes rather than edits, and only within 24h on messages this app sent | [撤回应用消息](https://developer.work.weixin.qq.com/document/path/94867) |
| `delete_message` | Own messages only, via recall (`/cgi-bin/message/recall`), within **24 hours** of send; cannot delete users'/others' messages (data already delivered to the WeChat-plugin end also can't be pulled back) | [撤回应用消息](https://developer.work.weixin.qq.com/document/path/94867) |
| `rich_content` | Supported outbound msgtypes: text, image, voice, video, file, **textcard**, **news (图文)**, **mpnews**, **markdown**, miniprogram_notice, **template_card**. Adapter uses only `text` | [message/send](https://developer.work.weixin.qq.com/document/path/90236) |
| `attachments` | Outbound media upload caps (`media/upload`): image ≤10MB (JPG/PNG), voice ≤2MB/60s (AMR), video ≤10MB (MP4), general file ≤20MB; min 5 bytes; temp media_id valid 3 days. Inbound: image/voice/video callbacks carry a `MediaId` pulled via `media/get` (same 3-day validity). Note: the receive doc's six enumerated inbound types are text/image/voice/video/location/link — inbound *file* (`MediaId` + `FileName`) is not in that list yet is delivered in practice and handled by the adapter | [上传临时素材](https://developer.work.weixin.qq.com/document/path/90253) · [receive msg doc](https://developer.work.weixin.qq.com/document/path/90239) |
| `message_length_limit` | `text` content ≤ **2048 bytes** ("超过将截断" — server truncates; ~680 CJK chars at 3 bytes each). Chunking required for long replies | [message/send](https://developer.work.weixin.qq.com/document/path/90236) |
| `dm_support` | Yes — the self-built app model *is* 1:1 (app ↔ member via `touser`) | [message/send](https://developer.work.weixin.qq.com/document/path/90236) |
| `group_model` | Self-built app has no group callback. A separate `appchat` (群聊) send API and the 智能机器人 model exist but are not used by this adapter | [appchat/send](https://developer.work.weixin.qq.com/document/path/90248) |
| `group_sender_identity` | n/a for this adapter (1:1 only). In 1:1, the stable sender id is `FromUserName` (member UserID), always present, no consent gate | [receive msg doc](https://developer.work.weixin.qq.com/document/path/90239) |
| `send_model` | **Push** — app calls `message/send` with `access_token` + `agentid` + `touser`. No reply-window / reply-token; a user need not have messaged first | [message/send](https://developer.work.weixin.qq.com/document/path/90236) |
| `proactive_push` | Allowed and unsolicited. Quotas: per app ≤ (account cap × 200) person-times/day; per app→member ≤ 30/min and 1000/hour (excess "会被丢弃不下发" — silently dropped) | [message/send](https://developer.work.weixin.qq.com/document/path/90236) |
| `bot_to_bot` | n/a — self-built app callbacks originate from human members (`FromUserName` UserID); the platform does not deliver other apps'/bots' messages to a self-built app | [receive msg doc](https://developer.work.weixin.qq.com/document/path/90239) |
| `typing_indicator` | Not supported by the API | [message/send](https://developer.work.weixin.qq.com/document/path/90236) |

## 2. OpenAB feature support (`openab-feature-support`)

| Feature | Status | Note | Ref |
|---|---|---|---|
| `send_message` | implemented | `message/send` msgtype=text with `agentid` + `touser`; token cached & auto-refreshed, retries once on errcode 42001 | `wecom.rs:558` (`send_text`), `wecom.rs:584` (`post_with_token_retry`) |
| `message_split/chunking` | implemented | Byte-aware split at 2048 (WeCom's server-side byte cap); prefers `\n` boundaries, splits over-long lines at UTF-8 char boundaries (`char_indices`) so multibyte chars aren't severed. Uses local `split_text_lines`, not the trait `split_delivery` | `wecom.rs:517` (call), `wecom.rs:840` (`split_text_lines`); `adapter.rs:149` (trait `split_delivery`) |
| `streaming` | workaround | No edit API in callback mode, so "streaming" = optional "⏳..." placeholder + debounce-buffer chunks into a `watch` channel + recall placeholder + resend consolidated text (`flush_thinking`). Causes client flicker → **default OFF** (`WECOM_STREAMING_ENABLED`, `debounce_secs=3`). With streaming off, chunks buffer silently and one consolidated message is sent | `wecom.rs:38-47` (config), `wecom.rs:381-459` (buffer/spawn), `wecom.rs:773` (`flush_thinking`) |
| `reply/quote` | n/a | No reply/quote primitive in the self-built app model. The trait `send_message_with_reply` default just falls back to plain send; adapter never wires it | `adapter.rs:336` (trait default) |
| `edit_message` | workaround | `reply.command == "edit_message"` only pushes new text into an in-flight streaming `watch` channel (`handle_edit_message`); there is no real WeCom edit. Outside a pending stream it is a no-op. The trait `edit_message` default (returns "not supported") otherwise applies | `wecom.rs:357` (dispatch), `wecom.rs:546` (`handle_edit_message`); `adapter.rs:330` (trait default) |
| `delete_message` | not-implemented | Recall API exists (24h window) but adapter never calls `/cgi-bin/message/recall` for user-facing delete — only internally to remove the thinking placeholder. Trait `delete_message` default edits to zero-width space, which WeCom can't honor | `wecom.rs:786` (internal recall only); `adapter.rs:353` (trait default) |
| `emoji_reactions` | n/a | `reply.command` `add_reaction` / `remove_reaction` are explicitly matched and ignored with a log line — WeCom self-built apps have no reaction API | `wecom.rs:351-356` |
| `threads/topics` | n/a | `create_topic` command explicitly ignored (logged). No thread primitive on platform | `wecom.rs:351-356` |
| `media_inbound` | partial | Inbound `image` → download over HTTPS-only (SSRF guard), reject >10MB, resize ≤1200px + JPEG q75 (GIF passthrough). `file` → `media/get` (retry on 42001), reject >20MB, **text files only** (extension/filename allowlist) and must be valid UTF-8; binary/office files rejected. voice/video/location/link msgtypes are dropped (only text/image/file forwarded) | `wecom.rs:1103` (`download_wecom_image`), `wecom.rs:1249` (`fetch_media_with_retry`), `wecom.rs:1287` (`download_wecom_file`), `wecom.rs:1234` (`is_text_file`), `wecom.rs:1410` (`resize_and_compress`) |
| `voice_stt` | not-implemented | `voice` msgtype is not in the accepted set (`text\|image\|file`); voice callbacks are dropped, no STT | `wecom.rs:1022` |
| `trust_gate` | implemented | Shared. Gateway ingress gate `gate_incoming` (L2 scope + L3 identity) runs before dispatch; wecom events carry `sender.id = FromUserName` (UserID) and `channel.id = wecom:{corp_id}:{from_user}` for keying | `wecom.rs:1059-1076` (event build); `gateway.rs:1196-1198` (gate call); `adapter.rs:495` (`gate_incoming` def) |
| `deny_echo` | implemented | Shared. On `DenyIdentity` the gateway echoes the sender their UserID with a request-access hint (throttled via `echo_allowed`); `DenyScope` silently drops. Platform-agnostic path, applies to wecom replies | `gateway.rs:1201-1226` |
| `mention_gating` | n/a | Shared `@mention` gating only fires for group/supergroup channel_type; wecom events are `channel_type="direct"`, so gating is bypassed by design | `wecom.rs:1064`; `gateway.rs:72-80` |
| `slash_commands` | partial | Shared. No platform slash mechanism; commands arrive as plain text and are handled by OpenAB's generic command layer, not in the wecom adapter | `wecom.rs:1030-1035` |
| `multibot` | n/a | Self-built app 1:1 callbacks come only from human members (`is_bot: false`); no other-bot delivery, no multi-bot channel | `wecom.rs:1067-1072` |
| `group_routing` | partial | Sessions keyed by `wecom:{corp_id}:{from_user}` (per-user 1:1); no group routing since there are no group callbacks in this model | `wecom.rs:1059` |

## 3. Platform quirks (`platform-quirks`)

### Send / push model (no reply window)

WeCom self-built apps are pure push: given a valid `access_token` + `agentid`, the app can message any member via `touser` at any time — there is no LINE-style reply token or reply window. `access_token` (7200s TTL) is cached with a 300s refresh margin (`TOKEN_REFRESH_MARGIN_SECS`, `wecom.rs:225`) and force-refreshed on errcode 42001 across both `message/send` and `media/get`.

### Crypto / callback specifics

- `EncodingAESKey` is 43 base64 chars **without** padding; adapter appends `=` and decodes with Indifferent padding + `allow_trailing_bits` (the 43rd char's last 2 bits are not payload). Result must be exactly 32 bytes (`decode_aes_key`, `wecom.rs:74`).
- WeCom uses **PKCS7 with block_size=32** (not 16); adapter decrypts AES-256-CBC with `NoPadding` and strips padding manually (pad value 1–32). Plaintext = `random(16) + msg_len(4 BE) + msg + corp_id`; inner corp_id must equal configured `CORP_ID` (`wecom.rs:149-182`). IV = first 16 key bytes (`wecom.rs:134`).
- Defense-in-depth: outer envelope `ToUserName` must equal `CORP_ID` (`wecom.rs:971`); `msg_signature` compared in constant time (`subtle::ConstantTimeEq`, `wecom.rs:122`); stale callbacks (>300s timestamp skew) rejected (`wecom.rs:947-953`); 30s TTL / 10k-entry dedupe cache on `MsgId` absorbs WeCom's ~5s retries (`wecom.rs:189-219`).
- `gettoken` requires `corpsecret` as a **query param** (protocol-mandated) — operators must redact query strings on `/cgi-bin/gettoken` in proxy logs; gateway never logs that URL (`wecom.rs:275-283`).

### "Streaming" is recall + resend

Because callback mode has no message-edit API, streaming is emulated: optional "⏳..." placeholder, debounce-buffer deltas into a `tokio::sync::watch` channel (default 3s), then recall the placeholder and send the consolidated final text via `flush_thinking`. This flickers, so it is **off by default** (`WECOM_STREAMING_ENABLED`). With it off, deltas are buffered silently and one message is sent when the debounce settles — no flicker, no recall. A 300s idle cap on the debounce task prevents an orphaned pending entry.

### Inbound filtering is aggressive

Only `text`, `image`, `file` msgtypes are forwarded (`wecom.rs:1022`); `voice` / `video` / `location` / `link` are dropped. Files must pass a text-extension/filename allowlist AND be valid UTF-8 — office/binary files are rejected (no doc parsing). Images are HTTPS-only, ≤10MB, downscaled to ≤1200px JPEG q75 (GIF passthrough). Placeholder prompts are injected for media: image → "Describe this image.", file → "User sent a file: {name}" (`wecom.rs:1030-1035`).

### Findings log

- 2026-07-04 (A) Self-built app receive doc enumerates **six** inbound types: text/image/voice/video/location/link — file is *not* listed there (yet delivered & handled by the adapter). `FromUserName` = member UserID, `MsgId` = 64-bit; no group/@mention/reaction/edit in this model. [https://developer.work.weixin.qq.com/document/path/90239]
- 2026-07-04 (A) `text` content capped at 2048 bytes (truncated); push quota per app→member 30/min, 1000/hr, per-app (account-cap×200)/day, excess silently dropped. msgtypes: text/image/voice/video/file/textcard/news/mpnews/markdown/miniprogram_notice/template_card. [https://developer.work.weixin.qq.com/document/path/90236]
- 2026-07-04 (A) No edit API; only recall (`/cgi-bin/message/recall`) within 24h on this app's own messages (delete, not edit; WeChat-plugin-end data can't be pulled back). [https://developer.work.weixin.qq.com/document/path/94867]
- 2026-07-04 (A) Media caps: image 10MB (JPG/PNG), voice 2MB/60s (AMR), video 10MB (MP4), file 20MB; min 5 bytes; temp media_id valid 3 days. [https://developer.work.weixin.qq.com/document/path/90253]
- 2026-07-04 (A) Callback auth = SHA1 msg_signature (sorted token/ts/nonce/encrypt) + AES-256-CBC via 43-char EncodingAESKey, PKCS7 block_size=32, IV = first 16 key bytes. [https://developer.work.weixin.qq.com/document/path/90238]
- 2026-07-04 (B) All section-2 `file:line` refs verified against the tree at `/tmp/openab-check`; corrected `send_text` / `fetch_media_with_retry` / `gate_incoming` line numbers and the mention-gating range. `WecomAdapter` is a standalone handler (does **not** impl `ChatAdapter`); trait defaults referenced are in `adapter.rs`. [PR: @TBD]

*Sourcing: `(A)` intrinsic facts → official WeCom doc; `(B)` OpenAB decisions/findings → `file:line` (PR link `@TBD`).*
