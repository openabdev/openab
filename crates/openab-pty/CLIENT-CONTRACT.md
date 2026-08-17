# openab-pty client contract

Implementation spec for a native client (macOS first, then iOS). Every shape
below was captured from the running Phase 1 runtime, not transcribed from the
source, so it reflects what a client will actually receive.

**Status:** internal spec. Not user documentation — the Gate A revision on the
tracking issue excludes user docs while the runtime is unpublished.

---

## 1. What the client stores

```
connection profile = { base URL, admin credential }
```

Two values, in the Keychain. Everything else is derived: session names come from
the API, attach tokens are minted by the client, stream offsets are tracked in
memory.

This makes the client the **operator**, which is what the ADR intends by
"tokens are returned only to the external control client". It does not weaken the
sandbox: the container holds only a SHA-256 verifier, never the credential, so a
session child still has nothing to authenticate with.

**Two trust levels exist. Pick the second.**

| Level | Holds | Can do | Consequence |
|---|---|---|---|
| Attach-only | base URL + one attach token | attach to one named session until the token expires | dies at token expiry with no recovery path |
| **Operator** | base URL + admin credential | everything below, including minting its own tokens | self-sufficient |

Attach-only is why dogfooding kept stalling: expiry left no way back in without
an operator issuing a new token by hand.

## 2. Transport

- `wss://` wherever the deployment terminates TLS at an Ingress (the ADR's MVP
  default). Plain `ws://` is only defensible inside a WireGuard tailnet, which
  the runtime acknowledges by requiring `tls_terminated_upstream = true` before
  it will bind off-loopback at all.
- **Native clients send `Authorization: Bearer <token>`.** The
  `Sec-WebSocket-Protocol: openab.bearer.<token>` form exists solely because
  browsers cannot set headers on an upgrade. Do not use it.
- `Origin` is never consulted by the runtime. Possession of a valid attach token
  is the entire authorization, by decision, not oversight.

## 3. Admin API

All five require `Authorization: Bearer <admin-credential>`. Missing or wrong
credentials return `401` with an empty body and are counted against a per-source
failure throttle.

### `GET /admin/sessions`

```json
{
  "sessions": [
    { "name": "laptop", "generation": 1, "alive": true, "attached": false,
      "bytes_written": 28, "tier": "tier1-best-effort-process-group",
      "teardown_best_effort": true, "absolute_ttl_best_effort": true }
  ],
  "draining": false,
  "kill_domain": { "tier": "tier1-best-effort-process-group",
                   "teardown_best_effort": true, "absolute_ttl_best_effort": true,
                   "leaked_processes": 0, "tracked_processes": 3,
                   "tracked_process_ceiling": 512,
                   "subreaper": { "status": "active" } },
  "metrics": { "sessions_created": 4, "self_exits": 1, "ttl_expired": 0,
               "takeovers": 0, "takeovers_rate_limited": 0,
               "admission_rejected": 0 },
  "abuse": { "upgrade_failures": 5, "upgrade_bans": 0, "admin_auth_failures": 0,
             "malformed_frames": 0, "oversize_frames": 0,
             "input_backpressure_disconnects": 0 }
}
```

`teardown_best_effort` and `absolute_ttl_best_effort` are `true` under the
default kill domain (Tier 1). **Surface that honestly** — the ADR requires
best-effort semantics be labelled wherever they appear, and a client that implies
a hard guarantee is misreporting.

### `POST /admin/sessions` — body `{"name":"laptop"}`

```json
{ "session": "laptop", "generation": 1,
  "token": "<64 hex chars>", "token_expires_in_secs": 43199 }
```

Names are `[a-z0-9-]{1,32}`; anything else returns
`{"error":"invalid session name \"BAD NAME\": expected [a-z0-9-]{1,32}"}`.
Validate client-side so the user sees it before a round trip.

### `POST /admin/sessions/{name}/renew`

Same response shape. **The session process survives**; scrollback is kept. The
generation is bumped, so every outstanding token for that session dies at once,
and an attached connection is evicted with `4003`.

### `POST /admin/sessions/{name}/restart`

For reattach-to-dead: same name, fresh process, new generation, empty scrollback.

### `DELETE /admin/sessions/{name}`

Attached clients close with `4008`.

Errors are `{"error":"..."}`, e.g. `no such session: nosuch`,
`session capacity exceeded (limit 3)`.

## 4. Attach

```
GET /pty/{session}          Authorization: Bearer <attach-token>
GET /pty/{session}?since=N  resume from stream offset N
```

First text frame after upgrade:

```json
{ "v": 1, "type": "attach-notice", "stream_offset": 28,
  "ephemeral_workspace": true, "teardown_best_effort": true,
  "externalise_with": "git push (lifecycle hooks are backup, not primary)" }
```

- **Keep `stream_offset` and advance it by every payload byte received.** Pass it
  as `?since=` on reconnect to replay only what was missed. The first client
  ignored this and restarted from scratch every time.
- `ephemeral_workspace` must reach the user. The workspace does not survive pod
  replacement, and a terminal looks exactly like a local shell, so the assumption
  runs the other way unless stated.

Frames after that: **binary = PTY bytes**, **text = control JSON**.

Client → server control: `{"v":1,"type":"resize","cols":120,"rows":40}`,
`{"v":1,"type":"ping"}`, `{"v":1,"type":"detach"}`. Strict allowlist —
unknown types and out-of-range values count toward an abuse metric and
disconnect after three.

Server → client control: `gap` (with `dropped_bytes`, on ring-buffer overflow —
clear and redraw rather than rendering a sliced ANSI stream) and `ttl-warning`
(precedes forced teardown).

## 5. Close codes

| Code | Meaning | Client action |
|---|---|---|
| 4001 | idle or absolute TTL elapsed | offer to create a new session |
| 4002 | another connection took over | offer to take it back; explain single-attach |
| 4003 | admin renewed the token | reconnect with the new token |
| 4004 | the shell exited | offer `restart` — **not** "expired" |
| 4005 | client too slow to drain | reconnect; consider a larger buffer |
| 4006 | runtime replaced (pod/task) | say the workspace was reset |
| 4007 | capacity or admission bound | show the limit; do not auto-retry hard |
| 4008 | operator killed it | say so plainly |
| 4009 | internal fault | do not invite retry-create |
| **1006** | **the browser/WS layer never opened** | see below |

`1006` is not ours. It is what a client reports when the upgrade itself was
rejected, because the HTTP status is not visible at that layer. **Treat
"close 1006 with no prior open" as "the server refused the handshake".**

## 6. Disambiguating 401 — required, not optional

The runtime returns `401` for an expired token *and* for a session that does not
exist, deliberately, so session names cannot be enumerated. The two need
opposite responses, and guessing gets it wrong: during dogfooding a user was told
"your token expired" when the audit log showed
`SessionKill termination=Some(SelfExit)` — the shell had exited and the session
needed recreating.

On a rejected attach:

1. `GET /admin/sessions`
2. name present → the token is stale → `renew`, then retry attach silently
3. name absent → the session is gone → offer to create it, saying the previous
   shell exited

This keeps the anti-enumeration property in the runtime and puts the
disambiguation where the credential already is.

## 7. Two things that decide whether it feels good

Both are client-side. The runtime measured **1.0 ms** echo round-trip from its
own host, against **78–82 ms** from a laptop on WiFi, so nothing on the server
moves this number.

- **Local echo prediction.** Draw typed characters immediately, reconcile against
  the server echo. Without it, every character waits a full round trip. This is
  the largest single lever on perceived quality.
- **Keepalive while focused.** Latency was bimodal — a few samples at 4 ms among
  many at ~80 ms, with 0% packet loss and 41.5 ms of jitter — which is WiFi power
  saving, not distance. The protocol's own ping is 15–30 s, three orders of
  magnitude too slow to hold a radio awake. Ping every ~50 ms while the terminal
  has focus. Cheaper than prediction and it removes much of the same pain.

Do not attempt to fix either by changing the network path: the LAN path was
measured and was **not** better than the tailnet path. ICMP is not a proxy for
small-packet TCP over WiFi.

## 8. Minimum viable client

1. Store `{base URL, admin credential}` in the Keychain
2. List sessions
3. Create one if absent
4. Attach with `Authorization: Bearer`
5. Track `stream_offset`; reconnect with `?since=`
6. On `401`, disambiguate per §6 and recover without involving the user
7. Ping every ~50 ms while focused
8. Local echo prediction

Steps 1–6 are "works". Steps 7–8 are "feels good". Never silently swallow input
into a closed socket — that presented as data corruption when the real cause was
a takeover.
