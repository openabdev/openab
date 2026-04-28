# Feature Request: Hybrid LINE Reply/Push API Strategy

**Title**: `feat(gateway): implement hybrid LINE Reply/Push API strategy`

**Labels**: `feature`

**GitHub Issue**: [#607](https://github.com/openabdev/openab/issues/607)

## 1. Description

Add a hybrid reply strategy to the LINE adapter in `openab-gateway`.
When an agent response arrives within the 1-minute Reply API window,
the gateway uses the free Reply API; otherwise it falls back to the
existing Push Message API. This saves messaging quota on free-tier
LINE accounts without modifying OAB core.

## 2. Use Case

As a LINE bot operator on a free-tier plan (200 push messages/month),
I want the bot to prefer the Reply API so that routine Q&A exchanges
do not consume my push quota.

- **Problem**: The current Push-only strategy (`docs/adr/line-adapter.md`
  Section 3, "Reply Strategy: Push Messages") exhausts the 200-message
  limit within days of active development or personal use.
- **Trigger**: Any 1:1 or group message that receives an agent response
  within 1 minute of the webhook event.
- **Beneficiaries**: Individual developers, testers, and users in
  LINE-dominant regions (Taiwan, Japan, Thailand).

## 3. Proposed Solution

Implement a **Stateful Token Cache** inside `gateway/src/main.rs`.
The cache maps `event_id` to the LINE `replyToken` received in the
webhook payload. OAB core remains completely unmodified — it already
returns `reply_to: "evt_..."` in every `GatewayReply`.

```text
+--------------+      OAB Reply        +------------------+
|    openab    |---------------------->|  Custom Gateway  |
|    (Rust)    |<--------------------->|    (Stateful)    |
+--------------+      Gateway WS       +--------+---------+
                                                |
                                                V [Logic]
                                       1. Match reply_to with Cache
                                       2. If exists -> Use Reply API (Free)
                                       3. Else -> Fallback to Push API
```

### Implementation Details

1. **Cache storage**: When `line_webhook()` processes a message event,
   extract the `replyToken` from the LINE payload and insert it into
   a thread-safe `HashMap<String, (String, Instant)>` keyed by the
   generated `event_id`, with a TTL of 50 seconds (conservative
   margin within LINE's 1-minute limit).

2. **Zero core modification**: OAB core is not modified. If OAB sends
   a `reply_to` matching a cached `event_id`, the gateway uses the
   Reply API. If `reply_to` is empty or not found in the cache, the
   gateway falls back to the Push API. No auto-fill or inference is
   performed — the gateway never guesses which event a reply belongs to.

3. **Hybrid dispatch** (in the reply handler, ~line 537-551):
   - Look up `reply.reply_to` in the token cache.
   - **Hit + fresh**: call `POST v2/bot/message/reply` with the cached
     `replyToken`. On success, done (free, no quota consumed).
   - **Hit + reply API returns 400**: token expired; fall through.
   - **Miss or fallback**: call `POST v2/bot/message/push` (existing
     logic, consumes quota).

4. **Cache cleanup**: A background `tokio::spawn` task sweeps expired
   entries using the same `REPLY_TOKEN_TTL_SECS` constant to prevent
   memory growth.

### Alignment with Existing Architecture

- `docs/adr/custom-gateway.md` Section 3 (line 167) already lists
  `reply_context` as a deferred schema concern: _"Reply token, quote
  target, original message reference."_ This implementation stays
  entirely within the gateway, avoiding premature schema changes
  while addressing the concrete cost problem now.

- `docs/adr/line-adapter.md` Section 3 (line 107-113) documents the
  Push-only decision and its trade-off. This feature preserves Push
  as the guaranteed fallback while opportunistically using Reply.

## 4. Prior Art

- **OpenAB ADR (line-adapter.md)**: Explicitly chose Push API because
  "agent processing typically exceeds the 1-minute reply token window."
  This proposal respects that decision by keeping Push as fallback.
- **OpenAB ADR (custom-gateway.md)**: Lists `reply_context` as a
  known deferred concern, confirming this is an anticipated extension.
- **OpenClaw**: Manages LINE bridging via a plugin architecture with
  buffered responses and loading animations.
- **LINE Official Docs**: `replyToken` is valid for ~1 minute;
  webhook must respond with HTTP 200 within ~2 seconds.

## 5. Related Issues

None found. This complements the architecture in `docs/adr/line-adapter.md`
and `docs/adr/custom-gateway.md`.
