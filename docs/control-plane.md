# Agent Control Plane (`openab-cp`)

Standalone control-plane service for direct agent-to-agent delegation over
WebSocket JSON-RPC, so agents delegate work to each other without
round-tripping through a chat platform. Design and wire contract:
[ADR: Agent Control Plane](adr/agent-control-plane.md).

> **Status: PR 1/4 of the control-plane stack.** This slice ships the CP
> server binary (registry, policy, router, wire protocol). The OAB-runtime
> client (`[control_plane]` config + registration), the MCP facade/CLI, and
> streaming land in the follow-up slices — until then nothing connects to
> this server in a stock deployment, and there is no packaged container
> image yet.

## Run

```bash
cargo run -p openab-cp -- --config cp.toml
```

Start from the annotated example config:

```bash
cp crates/openab-cp/cp.toml.example cp.toml
```

Every field is documented in the example file, including the security
rationale. The essentials:

- `listen` — defaults to loopback (`127.0.0.1:9800`). A non-loopback bind is
  refused unless `allow_insecure_bind = true` is set explicitly, and then a
  TLS-terminating proxy (`wss://`) or a private network in front is
  required: runtimes authenticate with bearer keys that must never cross
  untrusted cleartext TCP.
- `[[identity]]` — one entry per agent identity: the auth key (supports
  `${ENV_VAR}` expansion) and its immutable `namespace`/`name`/`type`
  claims. A connecting runtime must register as exactly the identity its
  key is bound to.
- Heartbeats, lease expiry, registration deadline, per-identity connection
  quotas, and frame/prompt/result size caps are all configurable with safe
  defaults.

## Health

`GET /health` answers `ok` (liveness only; deeper checks are tracked in
issue #1474).

## Client behavior to expect

- CP-initiated closes use WS code 1008 with a reason: `registration
  timeout`, `lease expired`, or `outbound queue overflow`. On any of these,
  reconnect, re-authenticate, and re-register.
- After a lease expires or the CP restarts, in-flight delegations are gone:
  initiators reconcile against their own deadlines and re-delegate.
