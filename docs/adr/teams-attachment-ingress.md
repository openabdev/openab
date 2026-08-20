# ADR: Teams Metadata-First Attachment Ingress

- **Status:** Proposed
- **Date:** 2026-08-09
- **Author:** @NeoHsu
- **Related:**
  - [Inbound attachments](../inbound-attachments.md)
  - [Custom Gateway](custom-gateway.md)
  - [Gateway capabilities and delivery semantics](gateway-capabilities-and-delivery-semantics.md)
  - [Teams ephemeral ingress state](teams-ephemeral-ingress-state.md)
  - [Teams typed scope and mention routing](teams-typed-scope-and-mention-routing.md)
  - [Microsoft: Send and receive files](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/bots-filesv4)

---

## Context

Teams message activities can carry inline images and, in Personal chats with a
separate `supportsFiles = true` manifest profile,
`application/vnd.microsoft.teams.file.download.info` attachments. Microsoft
states that file-consent APIs support Personal context only; wider file support
requires Microsoft Graph. This proposal remains Graph-free, so it supports only
inline images and Personal image or UTF-8 text files.

The existing Gateway attachment envelope can carry base64 bytes or a local
path. Neither existing delivery mode is sufficient as the Teams security
contract:

- Gateway receives Microsoft credentials and download URLs, but it does not own
  Core's configured L2 scope and L3 identity decision.
- Core owns the authoritative trust gate, but Microsoft credentials and URLs
  must not cross into Core or the ACP child.
- A Gateway-local path is invalid when Standalone Gateway and Core use separate
  filesystems.
- Downloading before the Core gate would let an authenticated but untrusted
  sender consume network, memory, and image-processing resources.

The attachment-ingress design therefore needs a two-phase contract that works
identically in Unified and Standalone deployments while preserving default-off behavior and
rolling compatibility.

## Decision

### Choose metadata-first materialization

Use metadata-first materialization: Gateway publishes bounded attachment
metadata plus an opaque reference, and Core asks Gateway to materialize that
reference only after the shared trust gate returns Allow.

Do not copy Core trust configuration into Gateway. A duplicated evaluator would
create two policy authorities and could drift across rolling upgrades. The
existing Core gate remains the sole L2/L3 decision point.

The flow is:

```text
Microsoft activity
  -> Gateway validates JWT, tenant, route fields, and service URL
  -> Gateway stores URL/auth metadata only in the bounded process-local route
  -> GatewayEvent carries filename/MIME/declared size + opaque reference
  -> Core runs structural filter, typed L2, and shared L3
  -> Deny: no materialization command and no download
  -> Allow: Core requests each reference sequentially
  -> Gateway validates event/route/reference ownership, then downloads
  -> Gateway returns bounded normalized attachment bytes or rejected metadata
  -> Core converts the result to ACP ContentBlocks before dispatcher admission
```

No ACP session, processing indicator, streaming placeholder, or proactive
promotion begins before materialization completes.

### Additive v1 wire fields and capability

Keep the existing schema and protocol version. Add only optional fields:

- `Attachment.reference`: opaque Gateway-generated materialization reference;
- `GatewayReply.attachment_ref`: requested opaque reference;
- `GatewayResponse.attachment`: one normalized attachment result; and
- `AdapterCapabilities.supports_attachment_materialization`: fail-closed
  operation capability.

The operation is `command = "materialize_attachment"`. It always carries a
non-empty `request_id`; Core sends it only after a valid hello explicitly
advertises the capability. Unknown or legacy peers are never probed
optimistically.

`reply_to` remains the authenticated Gateway event correlation, and
`channel.id` remains the conversation correlation. Gateway must validate both
against the process-local route before resolving `attachment_ref`. A reference
is random, contains no URL or platform ID, is scoped to one event, and expires
with that event's route. It is not promoted to proactive state and does not
survive restart or replica changes.

Core makes at most one materialization request per reference in a turn and does
not automatically retry timeout, disconnect, malformed response, or rejection.
A download is read-only at Microsoft, but unbounded retry would still amplify
attacker-controlled resource use.

### One bounded base64 response for both deployment modes

Both Standalone and Unified return normalized bytes in
`GatewayResponse.attachment.data` as bounded base64. Unified deliberately uses
the same logical transport instead of a Gateway-local path so both modes share
validation, limits, failure behavior, and tests.

Existing `Attachment.path` remains available to other colocated adapters; Teams
attachment ingress neither emits nor accepts a path. This prevents a Standalone
Gateway path from being interpreted in the Core container.

The materialized response must fit the explicit internal WebSocket frame cap.
Core rejects an oversized response before JSON or base64 processing. The URL,
query, OAuth token, and raw Microsoft attachment object never enter the event,
response, Core log, agent prompt, or ACP child environment.

### Explicit opt-in

Add one first-class Teams setting:

```toml
[teams]
inbound_attachments = false
```

The environment fallback is `TEAMS_INBOUND_ATTACHMENTS`. Only `true`, `false`,
`1`, and `0` are accepted; malformed values fail closed to `false`. The default
remains disabled.

When disabled, Teams text behavior is unchanged and attachment metadata is
ignored. An attachment-only activity creates no Core turn. Enabling the setting
still requires the negotiated materialization capability.

### Supported Microsoft attachment forms

This proposal supports:

1. **Inline images in Personal, groupChat, and channel scopes**
   - declared MIME must be `image/*`;
   - `contentUrl` remains Gateway-local;
   - downloaded bytes must decode as an accepted image;
   - images are resized to at most 1200 pixels on the longest side and encoded
     as JPEG, except bounded GIF passthrough.

2. **Personal `file.download.info` attachments**
   - conversation type must be Personal;
   - the app uses a separate manifest profile with `supportsFiles = true`;
   - image extensions use the image pipeline;
   - the existing text-extension whitelist is accepted only with strict UTF-8;
   - the preauthenticated `downloadUrl` is fetched without the Bot bearer
     token.

3. **Attachment-only events**
   - empty text is accepted only when at least one bounded attachment metadata
     entry exists;
   - the turn proceeds only if materialization produces a usable content block
     or a rejected-attachment system block.

Adaptive Cards, file-info cards sent by the bot, audio, video, PDF, archives,
Office binaries, non-UTF-8 text, and channel/groupChat paperclip files are not
materialized. They become `Attachment::rejected` metadata after trust, rather
than being silently reclassified or downloaded.

### URL, redirect, and credential policy

Every attachment request uses a dedicated manual-redirect downloader:

- HTTPS only in production, port 443, no userinfo or fragment;
- URL query is permitted because Microsoft download URLs can be
  preauthenticated, but it is always redacted;
- IP literals, loopback, link-local, private destinations, and arbitrary
  operator-provided hosts are rejected;
- inline-image `contentUrl` is bound to the validated Bot Connector public-cloud
  origin;
- Personal file URLs and every redirect hop must match the compiled Microsoft
  commercial-cloud file-host profile;
- each redirect is resolved and revalidated before the next request;
- the Bot bearer token is attached only to an inline-image request and is never
  forwarded across an origin change;
- Personal `downloadUrl` requests never receive the Bot bearer token; and
- error messages contain operation class and rejection category, never URL,
  query, token, tenant, conversation, activity, or opaque-reference values.

The commercial public-cloud profile does not add a user-configurable host
allowlist. New Microsoft cloud profiles or observed official hosts require a
reviewed policy update.

### Resource limits

| Control | Limit |
| --- | ---: |
| Metadata entries examined | 10 per activity |
| Opaque references retained | 10 per accepted route |
| Inline image raw download | 10 MiB |
| Personal text file | 512 KiB |
| Aggregate raw download budget | 20 MiB per event |
| GIF passthrough | 5 MiB |
| Materialized response / WS text frame | 8 MiB |
| Redirect hops | 4 |
| Individual HTTP request | existing Teams request timeout |
| Whole materialization batch | 45 seconds |
| Filename after sanitization | 200 Unicode scalars |

`Content-Length`, when present, is checked before reading but never trusted as
the only bound. Bodies are streamed and stopped before appending bytes past the
remaining per-file or per-event budget. Declared size, actual raw size, output
size, MIME, image decoding, text extension, and UTF-8 are validated
independently.

Materialization is sequential per event. Existing per-event route capacity,
TTL, dedupe, and one-consumer topology remain in force.

### Rejection behavior

A metadata or materialization failure returns `Attachment::rejected` with a
stable category and sanitized detail:

- `size exceeded`;
- `unsupported format`;
- `download failed`;
- `processing failed`;
- `invalid content`;
- `security rejected`; or
- `configuration error`.

A rejected attachment has empty `data`, no `path`, and no reusable reference.
Core surfaces the rejected metadata as a system content block only after Allow.
One failed attachment does not discard usable text or another valid attachment.

Protocol-level failures such as missing capability, unknown reference, expired
route, cross-conversation request, oversized frame, or malformed base64 also
fail closed and never trigger a second download.

### Rolling compatibility

| Core | Gateway | Result when configured |
| --- | --- | --- |
| old | new | Text behavior unchanged; unknown references are ignored and no download occurs. |
| new | old or no valid hello | Attachments disabled because materialization capability is absent. |
| new | new with valid capability | Metadata is materialized only after Core Allow. |
| Unified | embedded new adapter | Uses the same reference, limits, and normalized response contract. |

A new Gateway may publish metadata references before a client hello because
this has no external side effect. Only a new, capability-aware Core can issue
the materialization command. An old Core drops attachment-only metadata and
continues processing text exactly as before.

## Security and reliability boundaries

- JWT and tenant validation remain L1 at Gateway.
- Core typed L2 and shared L3 remain authoritative and precede download.
- Attachment URLs and Microsoft credentials stay Gateway-local.
- Opaque references are process-local capabilities, not bearer URLs.
- The internal WebSocket token still authenticates Core-to-Gateway commands.
- Route expiry, restart, replica change, malformed reference, and conversation
  mismatch fail closed.
- No Graph, RSC, delegated token, durable attachment queue, replay, or
  exactly-once download is introduced.
- Microsoft commercial public cloud remains the only supported cloud profile.

## Manifest profiles

The base manifest keeps:

```json
"supportsFiles": false
```

Operators enabling Personal paperclip files use a separate reviewed profile
with `supportsFiles = true`. Inline images do not require this manifest opt-in.
The profile adds no Microsoft Graph or RSC permission.

## Acceptance criteria

Automated verification must prove:

- default off and malformed environment fail closed;
- no download command before structural, L2, and L3 Allow;
- denied text-plus-attachment and attachment-only events cause zero download;
- missing capability and old peers cause zero download;
- opaque reference event/conversation/TTL enforcement;
- URL and every redirect hop are validated without leaking URL or token;
- strict streamed limits, image decode, text extension, and UTF-8;
- unsupported and failed attachments become sanitized rejected metadata;
- attachment-only dispatch after one successful or rejected materialization;
- Standalone and Unified produce equivalent content blocks;
- response and WebSocket frame ceilings; and
- no path crosses a non-shared deployment boundary.

## Consequences

### Positive

- Trust-before-fetch is enforced by one authoritative Core policy.
- Gateway retains Microsoft credentials and sensitive URLs.
- Standalone no longer depends on an accidental shared filesystem.
- Additive capability negotiation keeps rolling upgrades fail closed.
- Unified and Standalone share observable behavior and limits.

### Negative

- Materialization adds one internal request/response per attachment.
- Base64 adds bounded memory and approximately one-third encoding overhead.
- Attachment-only turns wait for materialization before showing progress.
- Route-local references are intentionally lost on restart or expiry.
- Strict Microsoft host policy may reject a newly introduced official host until
  the profile is reviewed and updated.
