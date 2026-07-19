# MCP-over-ACP tunnel — extension implementation contract

This is the wire contract the **browser extension** (the ACP client / MCP server end)
implements so the OpenAB gateway can tunnel MCP to it over the existing `/acp` WebSocket, per
[ADR: Browser control via MCP-over-ACP](./adr/acp-server-websocket-mcp-browser.md). It adopts
the official [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp).

Scope: only the **gateway ↔ extension** hop (the sole hop that leaves the pod). How OpenAB
routes tool calls internally (core-hosted MCP proxy, agent subprocess) is out of scope for
the extension and may change without affecting this contract.

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
      { "type": "acp", "id": "<uuid>", "name": "browser" }
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
- **Notification** (outer frame has no `id`): fire-and-forget inner MCP notification; no
  reply. The **extension** sends these upward for server-originated MCP notifications (e.g.
  `notifications/tools/list_changed` when its tool set changes).

Inner MCP methods the extension must handle as a server:
- `initialize` → advertise `capabilities.tools`.
- `tools/list` → return the browser tools (§6).
- `tools/call` → execute the named tool in the active tab; return an MCP `CallToolResult`.

## 5. Closing — `mcp/disconnect` (gateway → extension, request)

```json
{ "jsonrpc":"2.0", "id":<n>, "method":"mcp/disconnect", "params": { "connectionId":"<conn>" } }
```
Extension releases the connection and replies `{ "result": {} }`.

## 6. Browser tools (the MCP tools the extension serves)

Baseline DOM-semantic set (model-agnostic; OpenAB also static-advertises these so they appear
even before the extension attaches). `tools/call` executes in the **active tab**.

| name | arguments | behaviour |
|---|---|---|
| `browser.click` | `{ "selector": string }` | click the element matching the CSS selector |
| `browser.read_dom` | `{ "selector"?: string }` | return a DOM snapshot (optionally scoped) |
| `browser.navigate` | `{ "url": string }` | navigate the active tab to the URL |
| `browser.type` | `{ "selector": string, "text": string }` | type text into the matched element |
| `browser.screenshot` | `{}` | capture a screenshot of the active tab |

`tools/call` returns an MCP `CallToolResult` (`{ "content": [ { "type":"text", "text":... } ] }`,
or an image content block for `screenshot`). On failure return an MCP tool error result. The
extension MAY expose additional tools beyond this baseline; they surface to the agent via
`tools/list` + a `tools/list_changed` notification.

## 7. Permissions

OpenAB core auto-approves tool permissions today (ADR D1); the extension does **not** need a
per-call consent UX yet. Fine-grained consent is a later addition to this contract.

## Notes for implementers

- One `connectionId` per `mcp/connect`; the gateway may reconnect (the MCP server / HTTP
  proxy inside OpenAB is decoupled from the WS lifecycle, so the extension may attach after a
  session has already started — ADR D4).
- Never assume an inner MCP `id`; always correlate by the outer ACP frame `id`.
- Keep tool execution idempotent where possible; the agent may retry.
