# MCP-over-ACP tunnel — extension implementation contract

This is the wire contract the **browser extension** (the ACP client / MCP server end)
implements so the OpenAB gateway can tunnel MCP to it over the existing `/acp` WebSocket, per
[ADR: Reverse MCP-over-ACP over WebSocket](./adr/acp-server-websocket-reverse-mcp.md). It adopts
the official [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp).

Scope: only the **gateway ↔ extension** hop (the sole hop that leaves the pod). How OpenAB
routes tool calls internally (today: the OAB MCP Facade and the agent subprocess) is out of scope
for the extension and may change without affecting this contract — as it already has: the
per-session MCP proxy and the stdio bridge that this line used to name are both gone.

## Roles

- **Extension = ACP client + MCP server.** It opens the `/acp` WS, drives the chat session,
  and *serves* the browser MCP tools over that same socket.
- **Gateway = ACP server + MCP client (connector).** It initiates `mcp/connect` /
  `mcp/message` / `mcp/disconnect` toward the extension.

An MV3 extension cannot open a listening socket, so MCP is tunnelled over the outbound WS the
extension already holds — that is the whole point of this contract.

## 1. Transport + auth (unchanged from the base)

`GET /acp` WebSocket. Bearer auth via the `Sec-WebSocket-Protocol` offer
`openab.bearer.<token>, acp.v1`; the server echoes `acp.v1`. All frames are JSON-RPC 2.0.

## 2. Declaring the MCP server (in `session/new`)

When the extension creates a session it declares its browser MCP server in the `mcpServers`
array with the `acp` transport type:

```json
{ "method": "session/new",
  "params": {
    "cwd": "...",
    "mcpServers": [
      { "type": "acp", "id": "<uuid>", "name": "katashiro" }
    ] } }
```

- `id` is extension-generated and stable for the session; the gateway uses it as the `acpId`
  in `mcp/connect`.
- The gateway records this declaration per session (it does not yet act on it until it
  connects — see §3).

## 3. Opening the tunnel — `mcp/connect` (gateway → extension, request)

```json
{ "jsonrpc":"2.0", "id":<n>, "method":"mcp/connect", "params": { "acpId":"<uuid>" } }
```

Extension replies with a fresh, extension-assigned connection handle:

```json
{ "jsonrpc":"2.0", "id":<n>, "result": { "connectionId":"<conn>" } }
```

`connectionId` scopes all subsequent `mcp/message` traffic for this MCP connection.

## 4. Carrying MCP — `mcp/message` (bidirectional)

The inner MCP method + params are **flattened** into the `mcp/message` params (there is **no**
inner MCP `id`; correlation is by the outer ACP JSON-RPC id):

```json
{ "jsonrpc":"2.0", "id":<n>, "method":"mcp/message",
  "params": { "connectionId":"<conn>", "method":"<mcp-method>", "params": { ... } } }
```

- **Request** (outer frame has `id`): the extension executes the inner MCP method and replies
  with the **inner MCP result as the ACP response `result`**:
  ```json
  { "jsonrpc":"2.0", "id":<n>, "result": { ...inner MCP result... } }
  ```
  An inner MCP-level error is returned as the outer JSON-RPC `error`.
- **Notification** (outer frame has no `id`): fire-and-forget inner MCP notification; no reply.
  These travel in **both** directions. The extension sends them upward for server-originated MCP
  notifications; the gateway sends them downward, and the extension must forward them to its inner
  MCP server exactly as it forwards requests — see `notifications/initialized` below.

### Lifecycle: the gateway initializes before it asks for anything

Immediately after `mcp/connect` the gateway performs the MCP handshake on the new connection:

1. `mcp/message` **request** carrying inner `initialize` — the extension forwards it to its MCP
   server and returns the server's `InitializeResult`.
2. `mcp/message` **notification** (no outer `id`) carrying inner `notifications/initialized` — the
   extension forwards it to the server and replies with nothing.

Only then does the gateway send `tools/list` or `tools/call`.

**A server that fails `initialize` is not registered**, so its tools never become reachable and no
later call is attempted against it. Forwarding the notification matters as much as answering the
request: MCP servers are entitled to reject work until they have received `initialized`, and an
extension that swallows it leaves its own server permanently un-initialized while the gateway
believes the handshake completed.

Inner MCP methods the extension must handle as a server:
- `initialize` → forward to the server; return its `InitializeResult`.
- `notifications/initialized` → forward to the server; no reply.
- `tools/list` → return the browser tools (§6).
- `tools/call` → execute the named tool in the active tab; return an MCP `CallToolResult`.

## 5. Closing — `mcp/disconnect` (gateway → extension, request)

```json
{ "jsonrpc":"2.0", "id":<n>, "method":"mcp/disconnect", "params": { "connectionId":"<conn>" } }
```
Extension releases the connection and replies `{ "result": {} }`.

## 6. Browser tools (the MCP tools the extension serves)

Baseline DOM-semantic set (model-agnostic), as served by the example `katashiro` extension. OpenAB
ships no catalog of its own: what appears is what the operator allowlisted in
`[[mcp.acp_servers]]`, intersected with what the extension published — and nothing appears until the
extension's first `tools/list` returns. `tools/call` executes in the **active tab**.

| name | arguments | behaviour |
|---|---|---|
| `katashiro.click` | `{ "selector": string }` | click the element matching the CSS selector |
| `katashiro.read_dom` | `{ "selector"?: string }` | return a DOM snapshot (optionally scoped) |
| `katashiro.navigate` | `{ "url": string }` | navigate the active tab to the URL |
| `katashiro.type` | `{ "selector": string, "text": string }` | type text into the matched element |
| `katashiro.screenshot` | `{}` | capture a screenshot of the active tab |

`tools/call` returns an MCP `CallToolResult` (`{ "content": [ { "type":"text", "text":... } ] }`,
or an image content block for `screenshot`). On failure return an MCP tool error result. The
extension MAY expose additional tools beyond this baseline; they surface to the agent via
`tools/list` + a `tools/list_changed` notification.

## 7. Cancellation and limits

The gateway bounds how long it will wait for any tunnelled request. When that bound is reached it
stops waiting **and tells you**, so the extension is never left working on a request nobody reads.

### `mcp/cancel` (gateway → extension, notification)

```json
{ "jsonrpc": "2.0", "method": "mcp/cancel", "params": { "requestId": 42 } }
```

`requestId` is the **outer ACP frame `id`** of the request being abandoned — the same id you would
have replied to. There is no `id` on this frame: it is a notification, so no reply is owed and none
is read.

On receipt, stop the work and release whatever it holds (the tab, the navigation, the script). A
late reply to a cancelled `requestId` is discarded, so answering costs the gateway nothing and buys
you nothing. Cancellation is best-effort in one direction only: if the socket is already gone the
notification is simply never delivered, so do not treat its absence as "keep going".

### Limits

| Limit | Value | Meaning |
|---|---|---|
| Tunnel request timeout | `[mcp] tunnel_timeout_seconds`, default **180s** | one `mcp/message` request |
| Connect / handshake timeout | 30s | `mcp/connect` and the `initialize` that follows it |
| Servers per session | 8 (`MAX_ACP_SERVERS_PER_SESSION`) | `type:acp` entries accepted per `session/new` |
| In-flight establishes | 64 (`MAX_INFLIGHT_ESTABLISHES`) | concurrent tunnel setups per connection |
| Any inbound frame | 8 MiB (`MAX_FRAME_BYTES`) | checked **before** parsing. Exceeding it **closes the connection**, whatever the frame is — an unparseable frame has no recoverable `id` to answer |
| A `method` frame that is a **request** | 1 MiB (`MAX_NON_TUNNEL_FRAME_BYTES`) | checked **after** parsing. Answered with an error, **connection kept** |
| A `method` frame that is a **notification** | 1 MiB (same check) | **silently dropped.** Not a limitation — the gateway could answer with a null id and deliberately does not, because replying to a notification violates JSON-RPC. The refusal is required, and the cost is that you get no signal |

Three failure modes, and the line is drawn by **which limit you crossed**, not by whether the frame
was a request or a response. The 8 MiB allowance exists for tool results, which arrive as responses
(`id`, no `method`); method-bearing frames are held at 1 MiB so that allowance cannot be reused to
hold prompt text. The case worth planning for is the third: an oversized notification produces no
error and no acknowledgement of any kind, so a sender that assumes silence means success will lose
data without noticing.

The request timeout is operator-configurable because openab is the requester here and the peer is
an extension it neither ships nor controls. It sits beneath the ACP idle timeout, so raising it
past that bound moves the wall rather than removing it.

### `session/resume` withdraws what it does not re-declare

A resume re-presents the client's **whole** declaration set. Any `type:acp` server registered for
that session and absent from the new set is treated as withdrawn: its tunnel is retired and its
connection receives an `mcp/disconnect`. Re-declare every server you still want, on every resume —
including across a reconnect.

## 8. Permissions

OpenAB core auto-approves tool permissions today (ADR D1); the extension does **not** need a
per-call consent UX yet. Fine-grained consent is a later addition to this contract.

## Notes for implementers

- One `connectionId` per `mcp/connect`; the gateway may reconnect (the facade listener inside
  OpenAB is decoupled from the WS lifecycle, so the extension may attach after a
  session has already started — ADR D4).
- Never assume an inner MCP `id`; always correlate by the outer ACP frame `id`.
- Keep tool execution idempotent where possible; the agent may retry.
