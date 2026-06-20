# Gateway Context Providers

OpenAB gateway adapters can optionally enrich an admitted user turn with recent chat context that the bot would otherwise miss because of mention gating or platform-specific admission rules.

The gateway-level `ContextProvider` abstraction keeps this behavior shared across platforms:

- `observe()` records a message that was seen by an adapter but not dispatched to the agent.
- `fetch_context()` returns recent context for an admitted turn.
- `inject_context()` prepends that context with a clear boundary before the current message.

## Provider Types

| Provider | Intended platforms | Data source |
|---|---|---|
| `BufferedContextProvider` | LINE, Telegram, WeChat/WeCom, Feishu fallback | webhook observe -> local bounded buffer |
| `ApiFetchContextProvider` | Discord, Slack, Teams, Google Chat where available | platform history API |
| Hybrid provider | Google Chat and other mixed-permission platforms | API fetch when possible, buffer fallback otherwise |

The first implementation wires LINE group/room text to `BufferedContextProvider`. Future adapters can reuse the same trait without changing the prompt injection format.

## Defaults

Context capture is disabled by default.

| Setting | Default |
|---|---|
| `enabled` | `false` |
| `ttl` | `24h` |
| `max_messages` | `50` |
| `max_chars` | `8000` |

Gateway-wide environment variables use the `GATEWAY_CONTEXT_*` prefix. Platform-specific settings can override them; for example LINE uses `LINE_GROUP_CONTEXT_*`.

## Scope And Storage

Buffered context is:

- scoped by platform, channel, optional thread, and bot id
- in-memory only
- drained after injection
- bounded by TTL, message count, and total characters
- not long-term memory, retrieval storage, or GBrain state

This is intentionally short-term conversational continuity. Platforms with reliable history APIs can later implement API-backed or hybrid providers instead of relying only on local buffers.
