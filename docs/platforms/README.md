# Messaging platforms — capability matrix

Engineering-facing reference for how each messaging platform behaves and how OpenAB's adapters map it. **Distinct from** the operator setup guides in `docs/<platform>.md` — this tree is for maintainers/reviewers.

Each platform has its own page with three parts: **(A) platform-intrinsic facts** (official-doc sourced), **(B) OpenAB mapping** (`file:line` + PR sourced), and a **findings log**. See [`line.md`](./line.md) for the worked example.

**Sourcing rule:** attach the source that answers *"why should I trust or keep this?"* — type (A) facts link the official platform doc; type (B) decisions link the PR/issue where the reasoning lives.

## Matrix

| Aspect | LINE | Slack | Telegram | Feishu | WeCom | Google Chat | Teams | Discord |
|---|---|---|---|---|---|---|---|---|
| Transport | gateway | Socket Mode | gateway | unified/gateway | gateway | gateway | gateway | native WS |
| L1 auth | HMAC-SHA256 | app_token | secret_token + IP | SHA256 + encrypt key | Token sig + AES-256-CBC | JWT RS256 (JWKS) | JWT OIDC (JWKS) | bot_token |
| Reply/echo model | Reply(≈50s, free) / Push(quota) | chat API | send | send | send | send | send | send |
| Group sender ID | absent w/o consent → `unknown` | present | present | present | UserID | `users/...` | `activity.from.id` | present |
| Display name in event | ✗ (Profile API) | ✓ | ✓ | ✓ | ? | ? | ✓ | ✓ |
| Mention model | `mention.isSelf` | ? | bot_username | ? | ? | ? | ? | @mention + multibot |
| Bot-to-bot delivery | ✗ (`is_bot` always false) | ✓ | ✓ | ? | ? | ? | ? | ✓ (trusted_bot_ids) |
| Per-platform page | [line.md](./line.md) | _TODO_ | _TODO_ | _TODO_ | _TODO_ | _TODO_ | _TODO_ | _TODO_ |

Cells marked `?` / `TODO` are owned by that platform's maintainer — please fill from your adapter and official docs (don't guess; leave `?` if unverified). Only LINE is verified so far (against `crates/openab-gateway/src/adapters/line.rs`).
