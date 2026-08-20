# Microsoft Teams Live-Validation Tracker

- **Status:** Active validation index
- **Evidence through:** 2026-08-20
- **Scope:** Microsoft commercial public cloud

This file tracks mutable Microsoft 365 acceptance state. It is **not** an ADR,
product specification, implementation plan, or substitute for automated tests.
The linked ADR owns each durable decision. This tracker owns the current live
acceptance matrix, while the [detailed validation records](msteams-live-validation-records.md)
preserve sanitized observations and tenant-specific test procedures. Current
source and the [Teams platform schema](platforms/schema/teams.toml) describe
as-built behavior.

Do not store credentials, service URLs, tenant or conversation identifiers,
message content, raw production logs, absolute event timestamps, or screenshot
paths here. Evidence summaries must remain sanitized and bounded.

## Status vocabulary

- **RECORDED:** the detailed validation records contain bounded evidence for the
  stated subset.
- **OPEN:** the case still requires authorized live validation.
- **SKIPPED:** the required environment or authorization was unavailable; this
  is never equivalent to success.
- **NOT OBSERVED:** an authorized bounded probe did not produce the platform
  condition; this is not proof that the condition cannot occur.

## Bot-only validation matrix

| Area | Recorded live baseline | Open, skipped, or not-observed cases | Decision record |
| --- | --- | --- | --- |
| Ingress route and duplicate publication | **RECORDED:** Personal no-consumer retry reached one accepted publication after rollback of the failed reservation. | **OPEN:** post-accept and concurrent duplicate arbitration in live traffic; crash replay remains explicitly unsupported. | [Ephemeral ingress](adr/teams-ephemeral-ingress-state.md) |
| Normal send and explicit reply correlation | **RECORDED:** Personal normal send and `ReplyToActivity` returned real activity IDs; the tested client rendered both without quote UI. | **SKIPPED:** group-chat and channel root/reply presentation. **OPEN:** malformed live success IDs, route-TTL boundary, and live `429 Retry-After`. | [Real send acknowledgement](adr/teams-real-send-acknowledgement.md) |
| Bot-owned update and delete | **RECORDED:** Personal update, delete, post-delete rejection, and restart-boundary rejection. | **SKIPPED:** group-chat/channel mutations. **OPEN:** externally deleted or malformed IDs, eviction/TTL boundaries, live `429`, and app reinstall. | [Bot-owned mutations](adr/teams-owned-message-mutations.md) |
| Public-preview reactions | **RECORDED:** Personal add/remove, completion/error cleanup, expiry/restart rejection, and soft-/hard-stall mappings. | **SKIPPED:** group-chat and channel scopes. **NOT OBSERVED:** live `429`. **OPEN:** remaining mappings, cross-scope rejection, and tenants without preview rollout. | [Message reactions](adr/teams-message-reactions-preview.md) |
| Typed scope and mention routing | **RECORDED:** Personal admission and old/new rolling compatibility. | **SKIPPED:** GroupChat, Team channel root, Team channel reply, and structured recipient-mention cleanup in those scopes. | [Typed scope and mention routing](adr/teams-typed-scope-and-mention-routing.md) |
| Processing-message lifecycle | **RECORDED:** Personal create, terminal update, final delivery, cleanup, and reaction coexistence. | **OPEN:** Personal intermediate tool transition, error/timeout, and cleanup failure. **SKIPPED:** group-chat and channel scopes. | [Processing indicator](adr/teams-processing-indicator.md) |
| Progressive content | **RECORDED:** the Personal success, overflow, restart/expiry, reaction-progress, explicit-reply, and ambiguous-write subsets listed in the detailed validation records. | **OPEN:** recovery-delete/send, explicit-reply and overflow `Unknown`, cleanup failure, live `429`, and a production-length turn near default TTL. **SKIPPED:** group-chat and channel scopes. | [Progressive response](adr/teams-progressive-response.md) |
| Graph-free attachments | **RECORDED:** Personal inline image, text-plus-attachment, and attachment-only subsets. | **OPEN:** Personal paperclip image/UTF-8 text, rejected metadata, default-off, and commercial file-host cases. **SKIPPED:** group-chat/channel inline image. | [Attachment ingress](adr/teams-attachment-ingress.md) |
| Formatting and long messages | **RECORDED:** the Personal boundary, code/table, and ordered-delivery subsets listed in the detailed validation records. | **OPEN:** scope-specific rendering not closed by the Personal evidence and the remaining follow-up matrix. | [Formatting and long messages](adr/teams-formatting-and-long-messages.md) |
| Text commands | **RECORDED:** Personal canonical commands, compatibility forms, cancellation, reset, and supported-backend `/usage`. | **SKIPPED:** menu discovery, structured-mention command cleanup, GroupChat, and Team channel. | [Text command parity](adr/teams-text-command-parity.md) |
| Persistent conversation registry | **RECORDED:** Personal post-trust registration ordering, generation reload, file modes, and single-response UI. | **SKIPPED:** GroupChat/channel, uninstall revocation, and blocked-403 lifecycle. | [Persistent registry](adr/teams-trusted-persistent-conversation-registry.md) |
| Operator cron delivery | **RECORDED:** Personal Core-first compatibility, exact active-route delivery, registry preservation, UI structure, restoration, and quiet-soak subsets. | **SKIPPED:** GroupChat/channel and destructive blocked/not-in-roster lifecycle cases. | [Operator cron delivery](adr/teams-operator-cron-delivery.md) |

## Shared open lifecycle cases

The relevant ADR remains authoritative for product invariants. The detailed
validation records own tenant-specific preconditions and expected outcomes. The
following cross-cutting cases remain open unless this tracker or a detailed
record explicitly states otherwise:

- Connector `429 Retry-After` at the documented operation-specific bounds;
- post-accept duplicate delivery;
- cross-tenant and cross-conversation target rejection;
- malformed, externally deleted, or empty activity IDs;
- token refresh and signing-key rotation;
- app uninstall and reinstall;
- long-running turns near default route expiry; and
- manifest upgrade and tenant feature propagation.

Enterprise consent, RSC revoke, history boundaries, and selected permissions are
tracked separately by [Microsoft Teams enterprise deployment](msteams-enterprise.md).

## Freshness and revalidation

Reconcile this tracker when any of the following changes:

- Teams adapter, Gateway wire schema, Core routing/status/dispatch behavior, or
  the Teams platform schema;
- Unified or Standalone deployment topology;
- Bot Connector, Teams manifest, public-preview reaction, or Microsoft cloud
  endpoint behavior; or
- a durable decision, current matrix verdict, or detailed validation procedure.

Revalidation procedure:

1. Identify the affected proposed ADR, matrix row, and detailed validation
   record. If a pull request exists, copy its draft Review Contract into the PR description;
   per [`review-contract.md`](review-contract.md), that submitted PR description
   is the canonical copy after maintainer review.
2. Run the ADR-specific tests plus the shared documentation gates:

   ```bash
   cargo test -p openab-core --all-features -- \
     --skip secrets::tests::resolve_exec_nonzero_exit
   cargo test -p openab-gateway --features teams
   cargo test --manifest-path crates/platform-schema/Cargo.toml
   ```

3. Obtain explicit authorization before any Microsoft 365 live write or
   destructive lifecycle probe.
4. Record only sanitized outcomes in the detailed validation records.
5. Update this matrix without promoting `SKIPPED` or `NOT OBSERVED` to success.
6. Update the Teams platform schema and setup/enterprise documentation when the
   public capability claim changes.
