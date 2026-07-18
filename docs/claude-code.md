# Claude Code

Claude Code uses the [@agentclientprotocol/claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp) adapter for ACP support.

## Docker Image

```bash
docker build -f Dockerfile.claude -t openab-claude:latest .
```

The image installs `@agentclientprotocol/claude-agent-acp` and `@anthropic-ai/claude-code` globally via npm.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.claude.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.claude.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.claude.command=claude-agent-acp \
  --set agents.claude.workingDir=/home/node \
  --set image.tag=beta
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

### Image Tag

Use `--set image.tag=<version>` to set the image version globally.
The chart auto-appends `-<agent>` to produce the final tag (see [image-tags.md](image-tags.md) for full details).

| Tag | Resolves to | Description |
|-----|-------------|-------------|
| `beta` | `beta-claude` | Floating beta channel (latest pre-release) |
| `0.9.0-beta.2` | `0.9.0-beta.2-claude` | Pinned to exact version |
| `0.9` | `0.9-claude` | Latest patch in minor (floating) |
| `stable` | `stable-claude` | Floating stable channel |

To override a single agent's image instead of the global tag:
```bash
--set agents.claude.image=ghcr.io/openabdev/openab:beta-claude
```

> ⚠️ There is no `latest` tag. Use `beta` or `stable`, or pin to an exact version.

## Manual config.toml

```toml
[agent]
# command defaults from OPENAB_AGENT_COMMAND="claude-agent-acp"
# Only override if you need non-default behavior
```

## Authentication

There are two ways to authenticate the Claude backend. **For container deployments,
the long-lived token is preferred** — see the comparison below.

### Option A (preferred): long-lived token via `claude setup-token`

Generate the token once on any machine where you can open a browser. Supported
for Claude Pro, Max, Team, and Enterprise plans:

```bash
claude setup-token   # interactive OAuth, prints a long-lived token (~1 year)
```

Inject it into the agent process as the `CLAUDE_CODE_OAUTH_TOKEN` environment
variable — ideally from a secret store rather than inline:

```toml
[secrets.refs]
claude_token = "aws-sm://my-secret#CLAUDE_CODE_OAUTH_TOKEN"

[agent]
env = { CLAUDE_CODE_OAUTH_TOKEN = "${secrets.claude_token}" }
```

The SDK reads the env var directly — no `.credentials.json` on disk is needed.

> ⚠️ Make sure you use the token printed by `claude setup-token`, which bills
> against your Claude subscription — **not** an `ANTHROPIC_API_KEY` from the
> API console, which bills per token.

### Option B: interactive OAuth session login

Sign in interactively; short-lived credentials are stored at
`~/.claude/.credentials.json` (persist via PVC across pod restarts):

```bash
kubectl exec -it deployment/openab-claude -- sh -c "$OPENAB_AGENT_AUTH_COMMAND"
kubectl rollout restart deployment/openab-claude
```

> Organizations using SSO can force the SSO flow with `claude auth login --sso`
> (e.g. override `OPENAB_AGENT_AUTH_COMMAND="claude auth login --sso"`). The
> plain command supports Claude Pro, Max, Team, Enterprise, and Console accounts.

### Comparison

| | A: `setup-token` + env var | B: `claude auth login` |
|---|---|---|
| Credential lifetime | ~1 year, static | Short-lived access token (hours, observed ~8 h) + rotating refresh token |
| Storage | Env var (secret store) | `~/.claude/.credentials.json` on disk |
| Survives restarts/backup-restore | ✅ Always | ⚠️ Only if the restored file holds the *latest* token pair |
| Concurrent agent processes | ✅ Safe | ⚠️ Refresh race can invalidate tokens ([#24317](https://github.com/anthropics/claude-code/issues/24317)) |
| Failure mode | Token expires after ~1 year → regenerate | Refresh with a rotated-out token **has been observed to wipe the credentials file** to an empty template ([#37402](https://github.com/anthropics/claude-code/issues/37402), [#65761](https://github.com/anthropics/claude-code/issues/65761)) |
| Renewal | Manual, yearly | Automatic while the file stays current |
| Exposure | Visible in the agent subprocess env (OpenAB logs a prompt-injection warning) | File readable by the agent process anyway |

### Why the long-lived token is preferred for containers

As observed with Claude Code 2.1.212, the OAuth session flow **rotates the
refresh token on use** — a refresh invalidates the previous token — and access
tokens are short-lived (hours). Anthropic does not document these internals as
a stable contract, so exact intervals and failure behavior may change between
versions, but the observed behavior interacts badly with how containers manage
state:

- **Snapshot/restore drift**: any backup of `.credentials.json` goes stale the
  moment the live file refreshes (hours, observed ~8 h). Seeding a new pod/task from a
  stale backup means the first inference attempts a refresh with a dead token,
  fails, and the SDK **zeroes out the credentials file** — the bot is locked out
  until a human re-authenticates.
- **Rolling deploys**: the new task typically starts before the old one's
  shutdown backup completes, so it seeds pre-rotation state by construction.
- **Session pools**: multiple concurrent agent processes sharing one credentials
  file can race on refresh and invalidate each other.

> ⚠️ **If your CD flow is a rolling update — ECS rolling deployments, Kubernetes
> rolling updates, or any strategy that starts the new instance before
> terminating the old one — use the long-lived token (Option A).** During the
> overlap window two instances share the same account: either can rotate the
> refresh token out from under the other, and the replacement instance seeds
> its credentials from a backup taken before the final rotation. Session-login
> credentials (Option B) are only reliable with stop-then-start deployments
> where the old instance fully shuts down (and backs up its state) before the
> new one boots.

The `setup-token` credential has no refresh dance at all, so none of these
failure modes exist. The trade-offs — yearly manual renewal and the token being
visible in the agent's environment — are minor by comparison, and the exposure
is equivalent to any other secret passed via `[agent].env`.

## Troubleshooting

### `Login failed: Request failed with status code 400` at "Paste code here if prompted"

The `Paste code here if prompted >` prompt is Claude Code's manual OAuth flow — the
claude-agent-acp adapter delegates authentication to the underlying `claude` binary,
so the 400 comes from the token exchange with Anthropic's OAuth server, not from the
ACP adapter or OpenAB.

Check these in order:

1. **Stale or partial code (most common).** The auth code is single-use and
   short-lived. Get a fresh code and paste the **entire** string, including
   everything after the `#` (the format is `code#state`).
2. **Old Claude Code version.** Claude Code v2.1.105–v2.1.107 had a bracketed-paste
   regression that truncated the pasted code, causing the token exchange to fail
   with 400. Fixed in v2.1.108. Deploy an image tag whose pinned Claude Code version
   is ≥ 2.1.108 (see [Image Tag](#image-tag)) — the default chart security context
   is non-root with a read-only root filesystem, so packages cannot be upgraded
   inside a running container, and a live install would not survive pod recreation
   anyway. If you build your own image, bump the `CLAUDE_CODE_VERSION` /
   `CLAUDE_AGENT_ACP_VERSION` build args to explicit pinned versions.
3. **Anthropic OAuth outage.** Waves of OAuth 400/500 errors have been server-side
   incidents (see [anthropics/claude-code#10719](https://github.com/anthropics/claude-code/issues/10719)).
   Check [status.claude.com](https://status.claude.com) before debugging further.

Always authenticate interactively via `kubectl exec` (as shown above) rather than
through the ACP stdio session, then restart the pod to load the new credentials.
