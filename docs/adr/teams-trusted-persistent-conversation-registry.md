# ADR: Teams Trusted Persistent Conversation Registry

- **Status:** Proposed
- **Date:** 2026-08-19
- **Author:** @NeoHsu
- **Related:**
  - [Teams ephemeral ingress state](teams-ephemeral-ingress-state.md)
  - [Teams typed scope and mention routing](teams-typed-scope-and-mention-routing.md)
  - [Gateway capability and delivery semantics](gateway-capabilities-and-delivery-semantics.md)
  - [Teams real send acknowledgement](teams-real-send-acknowledgement.md)
  - [Basic cron scheduler](basic-cronjob.md)

---

## Context

OpenAB can reply to an authenticated Teams activity only while its bounded
Gateway-local `TeamsIngressRoute` remains available. That route contains the
validated Bot Framework `serviceUrl` and is indexed by an OpenAB event ID. It is
deliberately process-local, expires after `route_ttl_secs`, and disappears when
the Gateway restarts.

Operator-scheduled delivery needs a trustworthy Teams destination after the
original turn and ephemeral route have ended. Persisting every JWT-valid
webhook is unsafe because Teams L2 scope and L3 sender identity are decided in
Core, after Gateway publication. Persisting only after an agent reply is also
incorrect: a trusted conversation remains a valid destination when a command
short-circuits, an attachment is rejected, the ACP backend fails, or the turn is
cancelled.

The trust and secret boundary is therefore split:

- Gateway alone validates Bot Framework JWT, tenant, endpoint, and route fields
  and retains `serviceUrl`;
- Core alone has the authoritative shared L2/L3 Allow decision; and
- neither Core nor an ACP child may receive a persistent conversation
  reference or `serviceUrl`.

This proposal creates the registry and post-trust promotion handshake. It does
not send proactive messages or schedule work.

## Constraints

- Existing deployments must remain behaviorally unchanged unless an operator
  configures a registry path.
- A denied, malformed, unauthenticated, cross-tenant, or expired route must
  never create or refresh a persistent record.
- The registry is routing authority and must be treated as sensitive state even
  though it contains no bot credential or message body.
- `serviceUrl` remains Gateway-local and must not appear in Gateway wire events,
  ACP input, command responses, or application logs.
- Standalone and Unified mode must apply the same promotion and storage rules.
- The baseline supports one Gateway process and one direct Core consumer; this
  is not a distributed registry or durable inbox.

## Prior Art and Industry Research

Research was performed against immutable source revisions on 2026-08-19.

### Microsoft Teams and Bot Framework

Microsoft requires a bot to retain a `conversationId` or
`conversationReference` for out-of-context delivery and recommends using the
`serviceUrl` from the incoming activity. The app must already be installed in
the target scope. A blocked or uninstalled bot may receive HTTP 403 with
`MessageWritesBlocked`; Teams also emits authenticated `installationUpdate`
activities with `action = add` or `remove`.

OpenAB adopts the incoming-reference and uninstall-signal model, but adds its
own shared L2/L3 promotion gate because JWT validation alone is not OpenAB user
or scope authorization. This proposal does not use Microsoft Graph to install the app
or discover new conversations.

### OpenClaw

Reviewed revision:
[`4994f7bacf308269a0770b4a912c44a746cccec7`](https://github.com/openclaw/openclaw/tree/4994f7bacf308269a0770b4a912c44a746cccec7).

OpenClaw's Teams plugin stores conversation references in keyed SQLite state,
hashes the conversation ID for its storage key, bounds retained conversations,
uses a one-year TTL, merges sparse refreshes, and validates stored
`serviceUrl` hosts before proactive use:

- [`conversation-store-state.ts`](https://github.com/openclaw/openclaw/blob/4994f7bacf308269a0770b4a912c44a746cccec7/extensions/msteams/src/conversation-store-state.ts)
- [`conversation-store-helpers.ts`](https://github.com/openclaw/openclaw/blob/4994f7bacf308269a0770b4a912c44a746cccec7/extensions/msteams/src/conversation-store-helpers.ts)
- [`bot-framework-service-url.ts`](https://github.com/openclaw/openclaw/blob/4994f7bacf308269a0770b4a912c44a746cccec7/extensions/msteams/src/bot-framework-service-url.ts)

Its normal admitted-message path asynchronously upserts the reference after
access checks, although its pairing path intentionally stores a reference for a
not-yet-allowlisted DM. OpenAB adopts bounded versioned storage, sparse-field
refresh, and endpoint revalidation. It diverges by prohibiting pairing or any
other denied activity from promotion, keying by app plus tenant plus Bot
Framework channel plus conversation, and recording disabled/revoked state
rather than exposing every stored route as immediately usable.

### Hermes Agent

Reviewed revision:
[`00e5a361b60f621ae2246dffdd0a0252895d8493`](https://github.com/NousResearch/hermes-agent/tree/00e5a361b60f621ae2246dffdd0a0252895d8493).

Hermes keeps Teams `ConversationReference` objects in an in-memory `chat_id`
map for cards and captures them before its shared `handle_message` admission.
The map disappears on restart. Separate-process cron instead requires an
operator-configured home conversation, tenant, and service URL; that path
allowlists service hosts and validates conversation IDs:

- [`plugins/platforms/teams/adapter.py`](https://github.com/NousResearch/hermes-agent/blob/00e5a361b60f621ae2246dffdd0a0252895d8493/plugins/platforms/teams/adapter.py)

OpenAB adopts the explicit endpoint and identifier validation but not the
pre-admission cache or static global home route. Those approaches cannot prove
that a dynamically observed conversation passed OpenAB's shared L2/L3 gate and
do not provide restart-safe per-conversation authority.

## Decision

### Opt-in configuration

Persistence is disabled when `teams.conversation_registry_path` is absent or
empty. This preserves all existing route and filesystem behavior.

When configured, the registry resolves:

| Setting | Default when enabled | Purpose |
| --- | ---: | --- |
| `conversation_registry_path` | none | Registry JSON file; absence disables the feature |
| `conversation_registry_max_entries` | `1000` | Independent persistent-record cap |
| `conversation_registry_ttl_secs` | `31536000` | One-year active/disabled retention window |

Standalone environment fallbacks are
`TEAMS_CONVERSATION_REGISTRY_PATH`,
`TEAMS_CONVERSATION_REGISTRY_MAX_ENTRIES`, and
`TEAMS_CONVERSATION_REGISTRY_TTL_SECS`.

A relative path resolves beneath `$HOME/.openab/`; an absolute path is accepted
as an explicit operator choice. Empty components, `.`/`..`, NUL, and any
existing symlink component are rejected. Helm does not silently create a new
Gateway PVC: Standalone operators must mount durable storage explicitly, while
Unified deployments may place the file on their existing HOME volume.

### Versioned record

The file has a closed top-level schema and records a generation number plus a
bounded list of entries. Each entry contains only:

- schema version;
- bot app ID;
- tenant ID;
- Bot Framework channel ID;
- conversation ID and canonical type;
- validated `serviceUrl`;
- optional Team and Teams channel IDs;
- `last_validated_at` and `updated_at` wall-clock timestamps;
- `active`, `disabled`, or `revoked` state;
- a bounded reason code and consecutive forbidden-write count.

No sender ID, user display name, message/activity ID, message text, attachment,
credential, OAuth token, or agent/session identifier is persisted. Configuration
accepts `1..=10000` entries; the serialized file is independently capped at 16
MiB. App, tenant, Bot Framework channel, conversation type, and reason fields
are capped at 256 UTF-8 bytes; conversation, Team, and Teams channel IDs at
2048 bytes; and the already endpoint-validated service URL at 4096 bytes.

The composite identity is:

```text
(app_id, tenant_id, bot_framework_channel_id, conversation_id)
```

Lookup always requires the complete identity. Conversation IDs alone are never
global keys.

### Trust-confirmed promotion

Gateway advertises an additive Teams capability only when the registry is
configured, safely opened, and the supported single-consumer topology is in
use. New Core treats an absent capability as persistence unavailable and sends
no new command. Old Core and old Gateway combinations retain process-local
behavior.

After structural admission and shared L2/L3 `Allow`, Core submits a bounded
`register_conversation` Gateway command before command, attachment, session, or
agent work can determine the turn outcome. The command carries only the
existing origin event ID and logical channel correlation. It never carries a
`serviceUrl` or serialized reference.

Gateway resolves the origin event ID back to the still-valid authenticated
`TeamsIngressRoute`, verifies the reply channel and single-consumer topology,
and atomically upserts that route into the persistent registry. Missing,
expired, evicted, cross-conversation, or capability-mismatched routes are
rejected before filesystem mutation.

Standalone registration runs in a tracked bounded task outside the WebSocket
reader because its correlated response can only be dispatched by that reader.
Unified invokes the same Gateway method in process. Registration is independent
from ACP success and is allowed for trusted recognized commands, ordinary
turns, and trusted turns that later become empty after mention cleanup.

A storage failure does not retroactively reject the already authenticated
inbound message or prevent its current reactive turn. It returns a correlated
`Rejected` or `Unknown` registration outcome, leaves no new usable record, and
is never blindly retried. A later independently authenticated inbound activity
may safely attempt a fresh upsert.

### Refresh and state transitions

Only a newly authenticated ephemeral route plus a new shared L2/L3 Allow may
refresh address fields or `last_validated_at`:

```text
Absent   -- trusted inbound + committed write --> Active
Active   -- trusted inbound + committed write --> Active (refreshed)
Disabled -- trusted inbound + committed write --> Active (re-enabled)
Revoked  -- trusted inbound + committed write --> Active (re-installed proof)
```

Gateway-local delivery reconciliation exposes transitions for
operator-scheduled delivery:

- two consecutive explicit blocked/not-in-roster 403 outcomes disable an active
  record;
- a successful proactive write clears the consecutive forbidden count;
- 429, 5xx, timeout, disconnect, and ambiguous outcomes do not disable or
  refresh a record; and
- an authenticated, tenant-allowed `installationUpdate` with `remove` or
  `remove-upgrade` marks an existing matching record revoked without creating a
  new record.

An `installationUpdate add` does not activate a route by itself because it has
not passed user L3 admission. A subsequent trusted inbound activity may
reactivate the record. This proposal records and tests these transitions;
operator-scheduled delivery is the first caller of reconciliation.

### Filesystem transaction and recovery

All mutations are serialized by one registry lock and follow
clone → validate → write temporary file → flush → atomic rename → parent
flush → publish in-memory state. Readers never observe a candidate state before
its durable commit.

On Unix, newly created directories are mode `0700` and the registry and
temporary files are mode `0600`. Existing registry permissions are tightened
before use. The final file, temporary file, and every existing path component
must be regular/non-symlink objects of the expected type. Record fields, entry
count, and total JSON bytes are bounded before allocation or replacement.

Malformed JSON, an unknown schema version, an oversized file, unsafe path, or
permission failure makes the persistent capability unavailable and does not
replace the existing file. Reactive Teams messaging continues with the current
process-local route contract, while health/logging reports a content-free
registry initialization error.

A crash before rename leaves the prior generation authoritative. Startup
ignores and safely removes only this registry's own validated temporary-file
pattern; unrelated files are never touched. No automatic migration from an
unknown future schema is attempted.

### Capacity and retention

Expired active or disabled entries are removed during load and mutation.
Revoked tombstones are retained within the same hard cap so a restart does not
silently erase the last known platform revocation before fresh trusted evidence
arrives. At capacity, the oldest expired, then disabled, then active record may
be evicted deterministically. Revoked records are not evicted to admit a new
key; saturation rejects the new promotion and emits a count-only warning.

This registry is not shared between replicas. Two Gateway processes must not
write the same path. The registry performs no cross-process locking and advertises no
persistent capability when the existing topology report is unsupported.

### Observability and privacy

Logs and tests may expose schema/generation, aggregate entry/state counts,
operation class, result class, elapsed time, and bounded reason codes. They must
not expose app, tenant, conversation, Team/channel, activity, sender, full path,
or service URL values. Filesystem tests use synthetic identifiers only.

The registry is never mounted into or passed through the agent subprocess
environment. Core and ACP observe only registration capability and the
correlated outcome.

## Compatibility and rollout

- Missing configuration means no disk read/write and no advertised capability.
- New Gateway with old Core never receives promotion and remains process-local.
- New Core with old or unavailable Gateway sees capability false and sends no
  unsupported command.
- Registry state is Gateway-local; rolling Core replacement does not require a
  file migration.
- A Gateway rollback leaves the versioned file untouched. The old binary does
  not know its path unless separately configured.
- Enabling Standalone persistence requires an operator-owned durable mount and
  a rollback backup. It does not recreate Core or change the Teams app
  manifest.

## Acceptance criteria

Automated coverage must include:

1. versioned round-trip, stable composite keys, sparse refresh, and complete
   record-field bounds;
2. absent-path no-op defaults and config/env/Unified/Standalone equivalence;
3. `0600`/`0700`, traversal and symlink rejection, temporary-file isolation,
   corrupt/unknown/oversized file fail-closed behavior, and old-or-new recovery
   across an interrupted write;
4. deterministic TTL/capacity behavior and revoked-tombstone saturation;
5. no promotion for structural, tenant, typed L2, L3, malformed-scope, expired
   route, cross-conversation, or unsupported-topology rejection;
6. promotion before command/attachment/session/agent side effects and
   independence from ACP failure;
7. correlated Standalone response without WebSocket reader deadlock, timeout
   retry, or unsolicited old-peer response;
8. Unified and Standalone promotion parity;
9. refresh/reactivation, consecutive blocked-403 disable, successful-write
   reset, ambiguous-outcome no-disable, and authenticated uninstall revocation;
10. serialization/logging guards proving that route identifiers, `serviceUrl`,
    messages, and credentials do not cross into Core, ACP, or logs; and
11. changed-line formatting, targeted Core/Gateway/platform-schema tests,
    Clippy, config documentation, relative links, secret scanning, and
    `git diff --check`.


## Consequences

### Positive

- Restart-safe Teams destinations inherit OpenAB's existing L2/L3 trust rather
  than JWT validity alone.
- Gateway retains full ownership of service URLs and persistent route data.
- PR 12 receives an explicit active/disabled/revoked lookup contract instead of
  reconstructing routes from session IDs.
- Default deployments gain no new filesystem side effect.
- Atomic versioned storage gives rollback and corruption behavior a testable
  boundary.

### Negative

- One trusted inbound event now creates an additional asynchronous control
  exchange when persistence is enabled.
- A JSON rewrite is proportional to the bounded registry size.
- Standalone operators must provide a durable Gateway mount explicitly.
- The one-writer restriction prevents active-active Gateway replicas from
  sharing this file.
- Filesystem permissions protect confidentiality only as strongly as the
  Gateway account and volume.

## Alternatives Rejected

1. **Persist every JWT-valid webhook in Gateway.** This bypasses Core L2/L3 and
   turns denied conversations into proactive authority.
2. **Persist only after a bot reply succeeds.** Commands, ACP failures, and
   cancelled turns would fail to register otherwise trusted conversations.
3. **Send the full conversation reference to Core.** This leaks `serviceUrl`
   across the established Gateway-local boundary and risks agent exposure.
4. **Infer a route from Core session keys.** Session identifiers do not contain
   authenticated service URL, tenant, app, or install-state evidence.
5. **Use only a static home channel.** It cannot represent per-conversation
   trust, refresh, disable, or revocation and invites cross-scope mistakes.
6. **Enable a default HOME file automatically.** Existing deployments would
   gain a new sensitive persistent side effect without operator opt-in or a
   guaranteed durable Gateway mount.
7. **Use SQLite in PR 11.** A single-writer bounded registry does not yet need a
   new database dependency; atomic JSON is inspectable and sufficient. A shared
   transactional store remains the multi-replica follow-up.
8. **Delete on the first 403.** A single response may be transient or
   misclassified; bounded consecutive evidence preserves the record while
   stopping future use after sustained explicit rejection.

## References

- [Microsoft: Send proactive messages](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/send-proactive-messages)
- [Microsoft: Conversation events and installation updates](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/subscribe-to-conversation-events)
- [Microsoft: Bot Connector authentication](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication?view=azure-bot-service-4.0)
- [Microsoft: Bot Connector REST API](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-api-reference?view=azure-bot-service-4.0)
- [OpenClaw Teams conversation store at reviewed revision](https://github.com/openclaw/openclaw/tree/4994f7bacf308269a0770b4a912c44a746cccec7/extensions/msteams/src)
- [Hermes Teams adapter at reviewed revision](https://github.com/NousResearch/hermes-agent/blob/00e5a361b60f621ae2246dffdd0a0252895d8493/plugins/platforms/teams/adapter.py)
