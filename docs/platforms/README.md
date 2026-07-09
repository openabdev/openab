# Messaging platforms — schema & index

Engineering/reviewer-facing knowledge base for how each messaging platform behaves and how OpenAB maps it. **Distinct from** the operator setup guides in `docs/<platform>.md`.

This directory is **not a giant table** — it defines a **schema** that every platform fills in its own `schema/<platform>.toml`. The files are the machine-checked source of truth, validated by the `crates/platform-schema` conformance tests (run in CI). See [`_template.toml`](./_template.toml) for the blank schema + per-field docs.

## How it works

Each `schema/<platform>.toml` has three schema-driven parts:

1. **Platform capability** (`[capability.*]`, fixed fields) — the platform's intrinsic nature and what a bot can/can't do inside it. Same fields for every platform. Source of truth = **official docs** (each field carries a `source` URL).
2. **OpenAB feature support** (`[[openab_features]]`, the closed 17-feature set) — for each OpenAB capability, a `status` + note + code `source`. Source of truth = **our code + the PR that decided it**.
3. **Platform quirks** (`[[quirks]]`, freeform dated log) — anything that doesn't fit a fixed field (e.g. LINE's reply/push model), plus a findings log.

**Sourcing rule:** attach the source that answers *"why should I trust or keep this?"* — intrinsic `(A)` facts link the **official platform doc** (a `source` URL); OpenAB `(B)` decisions/findings point at the **code** (`file.rs#symbol`) and, where relevant, the **PR** (`pr` / `refs`). Code refs use a grep-stable `#symbol` (no line numbers), so conformance can confirm they still exist without breaking on unrelated edits above the target.

## Conformance

`crates/platform-schema` deserializes every `schema/*.toml` into typed structs and, in CI, enforces:

- **structural validity** — required fields, closed enum sets, the exact 17-feature set, unknown-key rejection;
- **version currency** — every file's `schema_version` matches the current one (a stale file fails the build);
- **anti-drift** — every `source` code-ref still resolves to a real file + `#symbol` in the tree.

- **Current schema version: `2026-07-08`** — the top-line `schema_version` in each file. Bump it when the schema changes; the conformance test then flags every file that hasn't been re-verified.

## Platforms

| Platform | Schema file |
|---|---|
| line | [schema/line.toml](./schema/line.toml) |
| slack | [schema/slack.toml](./schema/slack.toml) |
| telegram | [schema/telegram.toml](./schema/telegram.toml) |
| discord | [schema/discord.toml](./schema/discord.toml) |
| feishu | [schema/feishu.toml](./schema/feishu.toml) |
| wecom | [schema/wecom.toml](./schema/wecom.toml) |
| googlechat | [schema/googlechat.toml](./schema/googlechat.toml) |
| teams | [schema/teams.toml](./schema/teams.toml) |

---

## Schema reference

The authoritative field list + types live in [`_template.toml`](./_template.toml) and the structs in `crates/platform-schema/src/lib.rs`. Summary below.

### 1 — `[capability.*]` (platform-capability)

Fixed fields, same for every platform; each carries a typed value + `note` + official-doc `source`. Use `?` in a note only when a fact is genuinely unverified.

| Section | Meaning / allowed values |
|---|---|
| `transport` | how events arrive: `webhook` / `websocket` / `socket_mode` / `long_poll` |
| `inbound_auth` | L1 request-auth / signature scheme: `hmac_sha256` / `jwt_rs256` / `aes` / `shared_secret` / `oauth` / `none` |
| `threads` | `native` / `reply_to_only` / `emulated` / `none` |
| `slash_commands` | supported? how registered / delivered? |
| `mentions` | how the bot detects being addressed: `at_mention` / `username` / `self_flag` / `none` |
| `emoji_reactions` | can a bot **add** / **remove** reactions? does it **receive** reaction events? |
| `edit_message` | can a bot edit its own already-sent message? |
| `delete_message` | can a bot delete a message? scope: `none` / `own` / `others` / `own_and_others` |
| `rich_content` | markdown / cards / buttons support |
| `attachments` | inbound & outbound media types (`image`/`audio`/`video`/`file`) + size cap |
| `message_length_limit` | max chars per outbound message (chunking implication) |
| `dm_support` | 1:1 direct messages supported? |
| `group_model` | group / channel / room / space taxonomy |
| `group_sender_identity` | stable per-user sender id in group events: `yes` / `no` / `consent_gated` |
| `send_model` | `any_time` / `reply_only` / `push_only` / `hybrid`; reply-token TTL; batch cap |
| `proactive_push` | can the bot message unsolicited? quota model: `unlimited` / `metered` / `none` |
| `bot_to_bot` | does the platform deliver other bots' messages to this bot? |
| `typing_indicator` | supported? |

### 2 — `[[openab_features]]` (openab-feature-support)

The closed set of OpenAB capabilities (derived from the `ChatAdapter` trait in `crates/openab-core/src/adapter.rs` + the trust/ingress layer). Each block: `feature` + `status` + `note` + `source` (array of `file.rs#symbol`) + optional `pr`.

**Status enum:** `implemented` · `partial` · `workaround` · `not_implemented` · `n_a` (platform can't support it). Always explain `workaround` / `partial` — that "why" is the valuable part.

| Feature key | Covers |
|---|---|
| `send_message` | basic outbound |
| `message_split` | long-message handling (`split_delivery`) |
| `streaming` | `stream_begin` / `stream_append` / `stream_finish` — live vs batched |
| `reply_quote` | `send_message_with_reply` |
| `edit_message` | own-message edit |
| `delete_message` | delete own / others |
| `emoji_reactions` | `add_reaction` / `remove_reaction` |
| `threads_topics` | `create_thread` / `create_topic` |
| `media_inbound` | images / files / audio ingestion |
| `voice_stt` | speech-to-text on voice notes |
| `trust_gate` | allowlist / identity-trust enforcement point |
| `deny_echo` | reply-on-deny behavior + delivery constraints |
| `mention_gating` | require @mention in groups |
| `slash_commands` | `/reset`, `/cancel` handling |
| `multibot` | multiple bots in one channel |
| `group_routing` | group / session routing |
| `cron_dispatch` | scheduled cron job delivery via `cronjob.toml` |

### 3 — `[[quirks]]` (platform-quirks)

Freeform dated log — anything not captured by sections 1/2 (special models, gotchas, structural constraints) plus a findings trail. Each block:

- `date` (`YYYY-MM-DD`), `title`, `note` (prose) — required.
- `kind` (required): `intrinsic` (a platform fact) or `openab_decision` (a choice/finding of ours).
- `source` (optional): official-doc URL, or `file.rs` / `file.rs#symbol`.
- `refs` (optional): PR/ADR links, e.g. `["#1291"]`.

---

## How to update

### Adding a new feature to the closed set

When OpenAB gains a new capability (e.g. `voice_call`), update in **one PR**:

1. **`crates/platform-schema/src/lib.rs`** — add to `EXPECTED_FEATURES`
2. **`docs/platforms/_template.toml`** — add a `[[openab_features]]` block
3. **`docs/platforms/README.md`** — add a row to the feature table above
4. **All 8 `schema/*.toml` files** — add the feature block with appropriate status:
   ```toml
   [[openab_features]]
   feature = "voice_call"
   status  = "not_implemented"   # or implemented / partial / workaround / n_a
   note    = "..."
   source  = ["crates/openab-gateway/src/adapters/line.rs#handle_voice"]
   pr      = "#XXXX"
   ```
5. **Bump `SCHEMA_VERSION`** in `lib.rs` + update `schema_version` in all `.toml` files

CI enforces completeness: a missing feature block in any platform file fails the build.

### Changing a feature's status on one platform

When an adapter adds or drops support (e.g. LINE gains streaming):

1. Edit only that platform's `schema/<platform>.toml`
2. Update `status`, `note`, `source`, and `pr` fields
3. No other files need to change (no version bump required for status-only changes)

### Adding a new platform

1. Copy `_template.toml` to `schema/<platform>.toml`
2. Fill all `[capability.*]` sections (source: official platform docs)
3. Fill all 17 `[[openab_features]]` blocks (source: adapter code)
4. Add `[[quirks]]` for platform-specific behaviors
5. Add the platform to `EXPECTED_PLATFORMS` in `tests/conformance.rs`
6. Add a row to the Platforms table in this README

### Architecture: TOML vs Markdown

| Layer | Purpose | Audience |
|-------|---------|----------|
| `docs/platforms/schema/*.toml` | Machine-readable facts schema | CI, automation, onboarding |
| `docs/<platform>.md` | Human-readable setup/operator guide | Operators deploying OAB |

These are **complementary, not overlapping**. TOML captures "what the platform can do + what OpenAB implements"; Markdown captures "how to configure and deploy". Update the TOML when adapter behavior changes; update the Markdown when deployment instructions change.
