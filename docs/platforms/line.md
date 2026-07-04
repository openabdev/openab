# LINE — platform notes

Engineering-facing capability & quirks reference for the LINE adapter. For operator setup, see the LINE setup guide. This doc is maintained by the LINE maintainer.

> Structure: **(A) platform-intrinsic facts** (official-doc sourced) · **(B) OpenAB mapping** (code + PR sourced) · **Findings log**. See [`README.md`](./README.md) for the cross-platform matrix.

---

## (A) Platform-intrinsic facts

Provider truths, independent of OpenAB. Source of authority = LINE official docs.

| Aspect | Behavior | Official ref |
|---|---|---|
| L1 auth | Webhook body signed with HMAC-SHA256 over the raw body using the channel secret; sent in `X-Line-Signature` | [Signature validation](https://developers.line.biz/en/reference/messaging-api/#signature-validation) |
| Reply model | `replyToken` per webhook event — **single-use** and **short-lived** (must reply within ~1 min). Free, does not consume quota | [Send reply message](https://developers.line.biz/en/reference/messaging-api/#send-reply-message) |
| Push model | `POST /message/push` by `userId` — works without a reply token, but **consumes the monthly message quota** (paid beyond the free tier) | [Send push message](https://developers.line.biz/en/reference/messaging-api/#send-push-message) · [Pricing](https://developers.line.biz/en/docs/messaging-api/overview/) |
| Group identity | In group/room events, `source.userId` is **only present when the user consents to providing their info**; otherwise it is absent | [Source object](https://developers.line.biz/en/reference/messaging-api/#source-user) |
| Display name | **Not** included in webhooks. Must be fetched via Profile API (`GET /profile/{userId}`, group variant `GET /group/{groupId}/member/{userId}/profile`). Profile API resolves `userId → name`; it **cannot** recover a missing `userId` | [Get profile](https://developers.line.biz/en/reference/messaging-api/#get-profile) · [Group member profile](https://developers.line.biz/en/reference/messaging-api/#get-group-member-profile) |
| Bot-to-bot | LINE does not deliver other bots' messages to your webhook — no bot-message path | [Webhook events](https://developers.line.biz/en/reference/messaging-api/#webhook-event-objects) |
| Mention | Group/room text events carry a `mention` object; the bot's own mentionee has `isSelf = true` (no bot-username env var needed) | [Message event / mention](https://developers.line.biz/en/reference/messaging-api/#wh-text) |

## (B) OpenAB mapping

How the adapter (in the **gateway** crate) implements the above. Refs are `crates/openab-gateway/src/adapters/line.rs` unless noted.

| Aspect | Implementation | Ref |
|---|---|---|
| L1 verify | HMAC-SHA256 over raw body vs `X-Line-Signature`; reject on missing/invalid | `line.rs:84-103` |
| Reply/Push dispatch | `dispatch_line_reply()` — hybrid: try Reply (cached token), fall back to Push when token expired/consumed. **Lives in the gateway crate**, not core | `line.rs:646` |
| Reply-token cache | `ReplyTokenCache`, TTL `REPLY_TOKEN_TTL_SECS = 50`, cap `REPLY_TOKEN_CACHE_MAX = 10_000` | `lib.rs:17`, `lib.rs:20` |
| Identity normalize | 1:1 → `channel_id = userId`; group → `group_id`; room → `room_id`. Sender `userId` falls back to `"unknown"` when absent | `line.rs:333-354` |
| SenderInfo | `id`/`name`/`display_name` all set to the raw `userId` (no name resolution today); `is_bot` hardcoded `false` | `line.rs:388-391` |
| @mention gating | Group/room messages that don't mention the bot are dropped **during normalization** (upstream of any trust check) | `line.rs:373-380` |
| Current trust | Shared gateway `should_skip_event()` in core — no LINE-specific trust today | `openab-core/src/gateway.rs:832` |

## Trust / echo design (agreed for the ADR #1291 revision)

- Trust decision in **core**; echo **delivery delegated to the gateway adapter** (LINE reuses `dispatch_line_reply`).
- deny-echo is **Reply-only, never Push** (no valid token → drop silently) to avoid push-quota DoS.
- Group config: `default_group_policy` + per-group `policy` (`open` = any member who @mentions; `members` = must be in `allowed_users`). `"unknown"` is always deny and never allowlistable.
- @mention gating stays **upstream** of the trust gate.
- Echo scope: 1:1 includes the sender UID; group/room carries **no ID** (generic message); both hard rate-limited.
- Name resolution enhancement: Profile API + local cache keyed by `userId` (long TTL; respects Profile API rate limit).

---

## Findings log

Newest first. Type (A) → official-doc link; type (B) → PR/issue link.

- **2026-07-04** (B) deny-echo on LINE must be **Reply-only, never fall back to Push** — reply token dies in ~50s, so echoing denies to a spammer would mostly hit Push and burn the paid quota (DoS amplification). [PR #1291]
- **2026-07-04** (B) Trust decision in core, but echo **delivery** delegated to the gateway adapter — LINE's send path (`dispatch_line_reply`) lives in the gateway crate, so "core does the echo" doesn't hold. [PR #1291]
- **2026-07-04** (A/B) In groups, `allowed_users` is unreliable because `userId` may be absent (`"unknown"`). Two-mode group config (`open`/`members`); `"unknown"` is never allowlistable. [PR #1291]
- **2026-07-04** (B) @mention gating must stay **upstream** of the trust gate — downstream would deny-echo ordinary group chatter not addressed to the bot. [PR #1291]
- **2026-07-04** (A) Profile API resolves `userId → name` only; it cannot recover a missing `userId` (chicken-and-egg). Name resolution + local cache fixes readable names, not the genuine `"unknown"` case. [Get profile](https://developers.line.biz/en/reference/messaging-api/#get-profile)
- **2026-07-04** (A) `is_bot` is always false for LINE — no bot-to-bot webhook delivery, so bot-bypass trust semantics are a no-op here. [Webhook events](https://developers.line.biz/en/reference/messaging-api/#webhook-event-objects)
