# ADR: Teams Text Command Parity

- **Status:** Proposed
- **Date:** 2026-08-13
- **Author:** @NeoHsu
- **Related:**
  - [Slash commands](../slash-commands.md)
  - [Teams typed scope and mention routing](teams-typed-scope-and-mention-routing.md)
  - [Multi-platform adapters](multi-platform-adapters.md)

---

## Context

Teams bot command menus do not create a Discord-style interaction callback.
The stable Teams app-manifest `bots[].commandLists[]` surface inserts configured
text into the compose box, and the user sends it as an ordinary authenticated
`message` activity. OpenAB must therefore parse commands only after the normal
Bot Framework validation, typed scope, identity, and structured-mention gates.

The first parity set is:

- `/models`
- `/agents`
- `/cancel`
- `/reset`
- `/cancel-all`
- `/usage`

At the reviewed design baseline, the implementation was split across platform entry points:

| Command | Discord native interaction | Gateway text path |
| --- | --- | --- |
| `/models` | Ephemeral select menu | Numbered text list through `/models` and `/model …` |
| `/agents` | Ephemeral select menu | Numbered text list through `/agents` and `/agent …` |
| `/cancel` | Implemented | Implemented in both Standalone and Unified loops |
| `/reset` | Implemented | Implemented in both Standalone and Unified loops |
| `/cancel-all` | Implemented | Missing |
| `/usage` | Ephemeral, Kiro-specific ACP query | Missing |

The two Gateway paths also duplicated command dispatch. At that baseline, the
Standalone path executed inside the WebSocket reader and used an unacknowledged
fire-and-forget response to avoid waiting for a reply that only the same reader
could dispatch. That was incompatible with Teams required send acknowledgements.

`/usage` has an additional privacy boundary. Discord responses are ephemeral,
but an ordinary Teams reply in a group chat or Team channel is visible to the
conversation. Account plan, limit, and overage information must not be exposed
there merely because the command text is valid.

## Prior Art

### OpenClaw

OpenClaw separates a shared command registry and parser from native-platform
fast paths. Definitions describe native names, text aliases, scope, arguments,
and tiers, while authorization is resolved before command execution. Unknown or
colliding prefixes are not accidentally consumed.

- [Shared command registry](https://github.com/openclaw/openclaw/blob/7179d21d977aacd0b07c0d5b12c31d1be251df7d/src/auto-reply/commands-registry.shared.ts)
- [Boundary-aware text parser](https://github.com/openclaw/openclaw/blob/7179d21d977aacd0b07c0d5b12c31d1be251df7d/src/auto-reply/reply/commands-slash-parse.ts)
- [Native command fast path](https://github.com/openclaw/openclaw/blob/7179d21d977aacd0b07c0d5b12c31d1be251df7d/src/auto-reply/reply/get-reply-native-slash-fast-path.ts)

### Hermes Agent

Hermes exposes a central messaging command surface, applies a separate
per-platform and per-scope command-access policy, and declares native command
manifests independently from command execution. Relay interactions normalize
back into the same command dispatcher rather than creating a second handler.

- [Messaging command handlers](https://github.com/NousResearch/hermes-agent/blob/a871948d8d4b0f774d4ec40467bab1078a9f28d5/gateway/slash_commands.py)
- [Per-platform command access](https://github.com/NousResearch/hermes-agent/blob/a871948d8d4b0f774d4ec40467bab1078a9f28d5/gateway/slash_access.py)
- [Relay command manifest](https://github.com/NousResearch/hermes-agent/blob/a871948d8d4b0f774d4ec40467bab1078a9f28d5/gateway/relay/command_manifest.py)

OpenAB does not need either project's full registry or permission-tier system.
The useful common pattern is a small semantic command service with separate
ingress admission and platform rendering.

## Decision

### 1. Add one platform-neutral command service

Core will own a small command module used by Discord native interactions,
Standalone Gateway events, and Unified Gateway events. It will contain:

- an exact parser for the six canonical commands and the existing Gateway
  `/model …` and `/agent …` compatibility forms;
- command execution against `SessionPool` and `Dispatcher`;
- structured, platform-neutral results for config options, session-control
  acknowledgements, usage data, validation errors, and unavailable operations;
- bounded user-facing error classes rather than raw backend errors; and
- shared config-option selection validation.

The command service will not depend on Serenity, Gateway wire types, Teams Bot
Framework types, Adaptive Cards, or any platform SDK. Platform adapters retain
presentation responsibilities:

- Discord renders ephemeral messages, selects, pagination controls, and the
  existing usage colour/footer embed.
- Teams and other approved text surfaces render bounded Markdown/plain text.

### 2. Parse only standalone, boundary-valid commands

Outer whitespace is ignored. Canonical command names are lowercase ASCII and
must end at the input boundary because the first set takes no arguments.
Recognized compatibility forms retain their current syntax:

```text
/model
/model list
/model set <number or exact name>
/agent
/agent list
/agent set <number or exact name>
```

A known command with unsupported trailing arguments returns a bounded usage
message. Prefix collisions such as `/reset-now`, `/usage-report`, or
`/cancel-all-now` are not commands and continue through the ordinary agent
prompt path. Unknown slash-prefixed text also continues to the ACP backend so
agent-native commands such as `/compact` remain usable.

Command interception occurs after structural, L2 scope, L3 identity, and Teams
recipient-mention handling, but before attachment materialization and dispatcher
submission. A recognized command does not create a session, consume an agent
turn, add queued/progress reactions, create a placeholder, or download an
attachment.

### 3. Keep one logical-thread command key

Commands use the same session key as normal dispatch:

```text
{platform}:{thread_id-or-channel_id}
```

`/reset` and `/cancel-all` clear all dispatcher handles whose key belongs to the
same `(platform, logical_thread_id)`, including every per-sender lane. They do
not affect another platform or conversation with a coincidentally equal native
ID.

### 4. Define the six command semantics

| Command | Core behavior |
| --- | --- |
| `/models` | Require existing session config options; return model options with current selection. Discord keeps its paginated select UI. Text rendering is current-first, displays at most 25 entries, and reports the omitted count. Existing `/model set …` searches the full option set. |
| `/agents` | Same as `/models`, accepting both `agent` and `mode` ACP categories. Existing `/agent set …` searches the full option set. |
| `/cancel` | Send one lock-free ACP `session/cancel` notification for the logical session. Do not clear buffered messages. |
| `/cancel-all` | Remove and abort all buffered dispatcher lanes for the logical thread, then send the same ACP cancel notification when a session exists. Report only whether buffering was cleared, not a race-prone exact count. |
| `/reset` | Remove and abort all buffered lanes, issue best-effort ACP cancellation, purge active/suspended/persisted session state through `SessionPool::reset_session`, and let the next ordinary message create a fresh session. |
| `/usage` | Require an active session, a backend that supports the existing usage extension, and a response surface proven private. Return the existing plan, breakdown, overage, currency, and reset information without logging it. |

Config mutations validate the requested `config_id` and value against the
current active session options before calling `session/set_config_option`.
Discord component payloads and text set commands use the same validation.

No command retries an ACP control notification or a platform response. A
platform write that is `Rejected` or `Unknown` remains terminal for that
response.

### 5. Preserve privacy and trust before execution

All command entry points must pass their normal platform admission before the
service is called.

For Teams:

1. JWT, tenant, required route fields, and public-cloud service URL are already
   validated by Gateway.
2. Structural bot/mention filtering runs.
3. Typed L2 scope and L3 identity admission runs.
4. Only an authenticated recipient mention entity is removed.
5. The remaining text is parsed as a command.

Plain-text `@OpenAB`, an unbound `<at>OpenAB</at>`, malformed typed scope, a
disallowed surface, or a denied identity cannot invoke a command.

Discord native interactions will be routed through the existing adapter-level
DM/channel/user policy and shared L3 identity gate before a shared command is
executed. Denial is acknowledged ephemerally and does not disclose another
user's or session's state.

`/usage` executes only when the response is private by construction:

- Discord native interaction: ephemeral response;
- Teams: authenticated typed `personal` scope with `is_dm = true`.

A Team channel, group chat, or old Teams event without typed privacy proof gets
a generic private-chat-only response and does not call the ACP usage extension.
No account values are logged or included in that rejection.

### 6. Keep Standalone and Unified execution equivalent

Both Gateway paths will call the same post-gate command helper. The duplicated
`/reset`, `/cancel`, and config-command blocks are removed.

The Standalone WebSocket reader must never await a command response whose
required Gateway ACK is dispatched by that same reader. It spawns bounded
command execution/delivery work, continues reading frames, and uses the normal
outcome-aware `ChatAdapter` send path from the spawned task. Unified uses the
same command service and renderer without a wire hop.

Command response delivery therefore follows negotiated Teams semantics:

- valid new-peer hello: required real-ID send ACK;
- old Gateway or no valid hello: existing legacy send behavior;
- first `Rejected` or `Unknown`: stop without retry or a second warning send.

No Gateway protocol version or capability field is added.

### 7. Add a conservative Teams manifest command menu

The documented manifest v1.25 profile will add classic
`bots[].commandLists[]`. Titles contain the exact text command, including the
leading slash.

The Personal list advertises all six commands. The Team/group-chat list omits
`/usage` and advertises the other five. This is discoverability only; selecting
an item still produces an ordinary message and all runtime trust/mention gates
remain authoritative.

This proposal does **not** enable the newer `supportsTargetedMessages` plus
`commandLists[].triggers = ["slash"]` agent surface. Microsoft's current agent
slash-command guidance says that surface switches group conversations into
private targeted-message mode. OpenAB does not negotiate, route, or support that privacy model, and manifest
v1.25 does not define those fields.

Changing an installed app manifest remains an operator-controlled package
upgrade. Runtime deployment does not silently mutate a tenant app package.

### 8. Keep observability content-free

One command completion record may include platform, canonical command name,
semantic outcome class, and response write outcome. It must not include command
arguments, option values, usage values, sender/conversation/activity IDs,
serialized responses, tokens, or URLs.

## Rolling Compatibility

| Core | Gateway / surface | Result |
| --- | --- | --- |
| old Core | new Gateway or unchanged manifest | Existing partial text-command behavior; new Gateway does not execute commands. |
| new Core | old Gateway / no valid hello | Ordinary event decoding remains compatible; non-sensitive commands use legacy response delivery. `/usage` fails closed without authenticated typed privacy proof. |
| new Core | new Gateway | Shared service plus negotiated response ACKs. |
| new Unified | embedded Teams adapter | Same parser, execution, privacy, and result semantics without WebSocket transport. |
| any runtime | old installed manifest | Commands remain manually typeable; no menu is required for correctness. |
| any runtime | updated command-list manifest | Menu selection sends ordinary text; runtime gates remain authoritative. |

## Acceptance criteria

Automated verification must cover:

- exact command boundaries, outer whitespace, known invalid arguments, and
  unknown slash text passing through to ACP;
- all six semantic command results plus existing `/model` and `/agent`
  compatibility forms;
- current-first 25-entry text rendering with deterministic truncation count;
- full-option validation for config selection and rejection of stale or forged
  IDs/values before ACP mutation;
- `/cancel` preserving buffers, `/cancel-all` clearing every lane, and `/reset`
  clearing every lane plus session state without cross-thread/platform effects;
- active, absent, unsupported, malformed, over-limit, and no-cap usage reports;
- `/usage` denied before backend access on public or unproven-private surfaces;
- Teams structural → typed L2 → L3 → mention cleanup → command ordering in both
  Standalone and Unified paths;
- recognized command events skipping attachments, dispatcher submission,
  reactions, processing status, and progressive placeholders;
- Standalone command execution not blocking the WebSocket response reader;
- required-ACK response `Delivered`, `Rejected`, `Unknown`, and timeout behavior
  without retry or duplicate response;
- new Core ↔ old Gateway/no-hello behavior and Unified parity;
- Discord interaction admission, ephemeral presentation, config pagination, and
  existing `/models`, `/agents`, `/cancel`, `/cancel-all`, `/reset`, and `/usage`
  regressions;
- manifest v1.25 schema validation, command-list scope separation, command count,
  and absence of targeted-message, Graph, RSC, delegated, or Adaptive Card
  permissions; and
- platform-schema conformance plus existing Core/Gateway/Unified tests.


## Consequences

### Positive

- Command semantics stop drifting between Discord, Standalone, and Unified.
- Teams gains the missing control and usage commands without a new protocol or
  permission.
- Usage data cannot be accidentally published to a group or channel.
- Required ACKs can be used without blocking the Standalone WebSocket reader.
- Manifest discovery remains independent from runtime authorization.

### Negative

- The shared service introduces structured semantic results and platform
  renderers instead of a single string-returning helper.
- Discord native command admission must be made explicit during the refactor.
- Teams text menus cannot match Discord's interactive select controls without
  the deferred Adaptive Card work.
- Operators must install a revised app package before command-menu entries
  appear.

## Alternatives Rejected

1. **Copy Discord handlers into Teams.** This preserves drift and imports
   platform-specific interaction assumptions into ordinary messages.
2. **Forward every command to the agent.** Session control and account usage are
   broker responsibilities and must work without consuming an agent turn.
3. **Keep the duplicated Gateway blocks.** `/cancel-all` and `/usage` would still
   diverge, and Standalone response ACKs would remain unsafe.
4. **Publish `/usage` in all scopes.** Teams ordinary replies are not ephemeral
   and could expose account information.
5. **Enable targeted messages now.** The newer manifest surface changes group
   privacy and delivery semantics that OpenAB has not modeled or validated.
6. **Use Adaptive Cards for parity.** Cards add a separate callback trust path
   and are explicitly deferred.
7. **Add a new Gateway command protocol.** Commands already arrive as ordinary
   authenticated events; a protocol change adds rolling risk without need.

## References

- [Microsoft: expose slash commands from agents and apps](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/create-a-bot-commands-menu)
- [Microsoft Teams manifest v1.25 schema](https://developer.microsoft.com/en-us/json-schemas/teams/v1.25/MicrosoftTeams.schema.json)
- [Microsoft classic bot command-menu source at reviewed commit](https://github.com/MicrosoftDocs/msteams-docs/blob/c4611ef2586b/msteams-platform/bots/how-to/create-a-bot-commands-menu.md)
