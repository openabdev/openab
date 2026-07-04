# Messaging platforms — schemas & index

Engineering/reviewer-facing knowledge base for how each messaging platform behaves and how OpenAB maps it. **Distinct from** the operator setup guides in `docs/<platform>.md`.

This README is **not a giant table** — it defines the **schemas** that every per-platform page follows. One page per platform (`line.md`, `slack.md`, …), each filled against the schemas below. See [`_template.md`](./_template.md) for a blank page.

## How it works

Each platform page has three schema-driven sections:

1. **Platform capability** (fixed fields) — the platform's intrinsic nature and what a bot can/can't do inside it. Same fields for every platform. Source of truth = **official docs**.
2. **OpenAB feature support** (fixed fields) — for each OpenAB capability, whether this platform implements it, and how. Same fields for every platform. Source of truth = **our code (`file:line`) + the PR that decided it**.
3. **Platform quirks** (flexible) — anything that doesn't fit a fixed field (e.g. LINE's reply/push model). Free-form, plus a dated findings log.

**Sourcing rule:** attach the source that answers *"why should I trust or keep this?"* — intrinsic facts link the **official platform doc**; OpenAB decisions/findings link the **PR/issue**.

## Schema versioning

Schemas evolve. Each schema carries a version; each platform page declares which versions it was written against, in front-matter:

```yaml
---
platform: line
maintainer: "@handle"
last_verified: 2026-07-04
schema_versions:
  platform-capability: v1
  openab-feature-support: v1
  platform-quirks: v1
---
```

**Current versions:** `platform-capability: v1` · `openab-feature-support: v1` · `platform-quirks: v1`

The conformance table below shows each page's declared versions — a page lagging the current version is visible at a glance and needs an update pass. When a schema changes, bump its version here and update this table.

### Conformance

| Platform | capability | feature-support | quirks | last verified |
|---|---|---|---|---|
| [line](./line.md) | v1 | v1 | v1 | 2026-07-04 |
| [slack](./slack.md) | v1 | v1 | v1 | — |
| [telegram](./telegram.md) | v1 | v1 | v1 | — |
| [discord](./discord.md) | v1 | v1 | v1 | — |
| [feishu](./feishu.md) | v1 | v1 | v1 | — |
| [wecom](./wecom.md) | v1 | v1 | v1 | — |
| [googlechat](./googlechat.md) | v1 | v1 | v1 | — |
| [teams](./teams.md) | v1 | v1 | v1 | — |

---

## Schema 1 — `platform-capability` (v1)

Fixed fields. Every platform fills all of them. Each value carries an official-doc link where the fact isn't self-evident. Use `?` only when genuinely unverified (note what's missing).

| Field | Meaning / allowed values |
|---|---|
| `transport` | how events arrive: webhook / websocket / socket-mode / long-poll |
| `inbound_auth` | L1 request-auth / signature scheme (e.g. HMAC-SHA256, JWT RS256, AES) |
| `threads` | native / reply-to-only / emulated / none — plus the model |
| `slash_commands` | supported? how registered / delivered? |
| `mentions` | how the bot detects being addressed (@mention, username, isSelf flag…) |
| `emoji_reactions` | can a bot **add** / **remove** reactions? does it **receive** reaction events? |
| `edit_message` | can a bot edit its own already-sent message? |
| `delete_message` | can a bot delete a message? (own / others) |
| `rich_content` | cards / buttons / markdown / rich-text support |
| `attachments` | inbound & outbound media types + size limits |
| `message_length_limit` | max chars per outbound message (chunking implication) |
| `dm_support` | 1:1 direct messages supported? |
| `group_model` | group / channel / room / space taxonomy |
| `group_sender_identity` | is a stable per-user sender id available in group events? consent-gated? |
| `send_model` | reply vs push; any reply window / token TTL |
| `proactive_push` | can the bot message unsolicited? quota / rate limits |
| `bot_to_bot` | does the platform deliver other bots' messages to this bot? |
| `typing_indicator` | supported? |

## Schema 2 — `openab-feature-support` (v1)

Fixed fields = the OpenAB capabilities exercised across adapters (derived from the `ChatAdapter` trait in `crates/openab-core/src/adapter.rs` + the trust/ingress layer). For each, give a **status** + note + `file:line` + PR ref.

**Status enum:** `implemented` · `partial` · `workaround` · `not-implemented` · `n/a` (platform can't support it).
Always explain `workaround` / `partial` / `limited` — that "why" is the valuable part.

| Feature | Notes to capture |
|---|---|
| `send_message` | basic outbound |
| `message_split/chunking` | long-message handling (`split_delivery`) |
| `streaming` | `stream_begin` / `stream_append` / `stream_finish` — live vs batched |
| `reply/quote` | `send_message_with_reply` |
| `edit_message` | own-message edit (`edit_message`) |
| `delete_message` | `delete_message` |
| `emoji_reactions` | `add_reaction` / `remove_reaction` |
| `threads/topics` | `create_thread` / `create_topic` |
| `media_inbound` | images / files / audio ingestion |
| `voice_stt` | speech-to-text on voice notes |
| `trust_gate` | allowlist / identity-trust enforcement point |
| `deny_echo` | reply-on-deny behavior + delivery constraints |
| `mention_gating` | require @mention in groups |
| `slash_commands` | `/reset`, `/cancel` handling |
| `multibot` | multiple bots in one channel |
| `group_routing` | group/session routing |

## Schema 3 — `platform-quirks` (v1)

Flexible. Anything not captured by Schema 1/2 — special models, gotchas, structural constraints. Two parts:

- **Quirks** — free-form subsections (e.g. "Reply/Push model"). No fixed fields; whatever the platform needs.
- **Findings log** — dated entries, newest first, one line each. Tag `(A)` intrinsic → official-doc link; `(B)` OpenAB decision/finding → PR/issue link.

```
- YYYY-MM-DD (A|B) <finding>. [source link]
```
