# Native Gmail Adapter (`gmail-native`)

Serve the six-tool Gmail profile (OAB MCP Adapter ADR §6.5) from Gmail's
**generally-available REST API** as a loopback Streamable HTTP MCP server —
no Google Workspace Developer Preview enrollment, works with consumer
`@gmail.com` accounts.

> **Why not Google's hosted Gmail MCP server?** During Developer Preview,
> `gmailmcp.googleapis.com` rejects every `tools/call` unless both the user
> account **and** the GCP project are enrolled in the Workspace Developer
> Preview Program — which only accepts Google Workspace accounts (consumer
> accounts are rejected at application review), and whose terms prohibit
> shipping preview features in public applications. This adapter mirrors the
> hosted server's tool names and argument shapes, so when the hosted server
> reaches GA the cut-over is a one-line `mcp.json` change.

## Capabilities (read + drafts only)

| Tool | Gmail REST call | Notes |
|------|-----------------|-------|
| `search_threads` | `threads.list` | Gmail query syntax (`from:alice in:inbox newer_than:7d`); returns thread ids + snippets |
| `get_thread` | `threads.get` | `format`: `metadata` (default, headers + snippet), `full`, `minimal` |
| `get_message` | `messages.get` | same formats |
| `list_labels` | `labels.list` | |
| `list_drafts` | `drafts.list` | |
| `create_draft` | `drafts.create` | **Never sends.** UTF-8 subjects (RFC 2047), reply threading via `replyToMessageId` |

There is deliberately **no send, delete, or label-mutation surface**. Draft
results state explicitly that nothing was sent.

## Setup

### 1. Google Cloud OAuth client (one-time)

1. Create/choose a GCP project and enable the **Gmail API**.
2. Configure the OAuth consent screen (External → add your Gmail as a test
   user) and add the two scopes:
   - `https://www.googleapis.com/auth/gmail.readonly`
   - `https://www.googleapis.com/auth/gmail.compose`
3. Create an OAuth client (**Web application**) with redirect URI
   `http://localhost:53692/callback`. Note the client ID and secret.
   (A secret-less **Desktop** client also works — omit the secret env var.)

### 2. Log in (paste-back, once per account)

```sh
export GMAIL_OAUTH_CLIENT_ID=<client-id>
export GMAIL_OAUTH_CLIENT_SECRET=<client-secret>   # omit for Desktop clients
openab mcp gmail-native login
```

Open the printed URL, approve, and paste the full
`localhost:53692/callback?...` redirect URL back (the page itself won't
load — expected). The flow requests `access_type=offline` + `prompt=consent`,
so Google issues a **refresh token** and headless deployments survive
access-token expiry without re-login. Credentials are stored under the
`gmail-native` key in `~/.openab/agent/auth.json` (0600).

If the login warns that no refresh token was returned, revoke the app at
<https://myaccount.google.com/permissions> and log in again.

### 3. Serve

```sh
openab mcp gmail-native serve --listen 127.0.0.1:8850
```

Loopback-only — non-loopback binds are refused at startup. The endpoint has
**no authentication layer**: the host/pod boundary is the trust boundary
(same model as the OAB MCP Facade). Any process on the host can use the
authorized mailbox capabilities while the adapter runs.

The serve process needs `GMAIL_OAUTH_CLIENT_ID` (+ secret) in its
environment for token refresh.

### 4. Register

**Via the OAB MCP Facade** (recommended — agents see only
`search_capabilities`/`execute_capability`, and `tool_filter` least-privilege
applies). In `~/.openab/agent/mcp.json`:

```json
{
  "mcpServers": {
    "gmail": {
      "type": "http",
      "url": "http://127.0.0.1:8850/mcp",
      "tool_filter": {
        "include": ["search_threads", "get_thread", "get_message",
                    "list_labels", "list_drafts", "create_draft"]
      }
    }
  }
}
```

**Direct** (any MCP client; the six tools appear natively) — e.g. Kiro CLI:

```json
{ "mcpServers": { "gmail": { "url": "http://127.0.0.1:8850/mcp" } } }
```

> **Kiro CLI gotcha:** when kiro runs with `--agent <name>`, the MCP server
> list comes from `~/.kiro/agents/<name>.json` (`mcpServers` +
> `allowedTools`), **not** the global `~/.kiro/settings/mcp.json`. Add the
> entry there and include `"@gmail"` in `allowedTools`.

## Security notes

- Scopes are the minimum for the profile: `gmail.readonly` + `gmail.compose`
  (drafts). Trash/delete would require `gmail.modify` or full-mailbox scope
  and a deliberate policy expansion — not part of this adapter.
- MIME header values are validated (CR/LF refused — header injection);
  Gmail resource ids are validated before use as URL path segments.
- The refresh token in `auth.json` grants durable mailbox access — treat the
  file like a credential (it is one). Home-directory persistence layers
  (e.g. S3 backup) will carry it.
- Prompt-injection caution: mail content is untrusted input to the agent.
  Keep the write surface drafts-only and review drafts before sending.
