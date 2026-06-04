# Troubleshooting

Diagnostic guide for OpenAB runtime issues. Most user-facing errors fall into one
of the categories below.

## `-32603 Internal Error` with no actionable detail

### Symptom

In Discord / Slack you see:

```
⚠️ **Internal Error** (code: -32603)
Internal error
```

…but no further context, and `kubectl logs` show the same opaque message.

### Why it happens

JSON-RPC 2.0 §5.1 reserves `-32603` as the "Internal error" catch-all. ACP
agents use it for wildly different failure modes (auth, model not supported,
context overflow, crash). When the agent doesn't populate `error.data`, the
client can only display the generic message.

| Agent | `error.data` shape | Diagnostic source |
|-------|-------------------|-------------------|
| codex-acp | `{"message": "<nested JSON>"}` | `data.message` extracted (PR #885) |
| opencode | `{}` (empty) | **stderr tail** (this PR) |
| hermes-agent | absent | **stderr tail** (this PR) |
| claude-agent-acp | (varies) | `data.message` or stderr |

Since OpenAB 0.8.4 (PR #885 merged 5/21) the broker extracts `error.data.message`
when present. Since the next release, when `data` is empty/absent, the recent
agent stderr is shown as a "Recent agent output:" blockquote.

### Diagnostic path

1. **Read the user-facing error first.** The stderr blockquote (if any) usually
   points at the root cause directly.
2. **If still unclear**, fetch the broker logs:
   ```bash
   kubectl logs -l app.kubernetes.io/name=openab --since=10m | grep -A 5 "agent="
   ```
   The `tracing::warn!` path in `connection.rs` always emits every sanitized
   stderr line with `agent=<command name>`.
3. **Match the cause to the table below and apply the fix.**

### Common causes

| stderr / data.message pattern | Cause | Fix |
|------------------------------|-------|-----|
| `Missing API key`, `API key not set`, `unauthorized`, `401` | Missing / invalid credentials | Verify the `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `*_TOKEN` env var is set in `[agent].env` or `[agent].inherit_env`. See `config.toml.example`. |
| `model 'X' not supported`, `does not exist`, `deprecated` | Wrong / deprecated model | Switch `model` via `session/set_config_option`, or pin a supported model in agent CLI args. |
| `rate limit`, `429`, `quota` | Upstream rate limit | Wait, or reduce concurrency via `pool.max_sessions`. |
| `context length`, `too many tokens`, `maximum context` | Prompt too long | Start a new thread (auto-resets history), or `/cancel` then retry with a shorter prompt. |
| `connection refused`, `DNS`, `tls handshake` | Network / DNS | Check egress from broker pod; verify `cluster.egressProxy` if set. |
| Empty stderr + opaque `-32603` | Bug in agent | File an issue against the agent upstream with the `acp_recv` debug log line. |

### Verifying the fix landed

After deploying a build with the stderr-tail fallback:

```bash
# 1. Force an auth failure at the BROKER level by stripping the key from
#    the broker's env (not the agent's — the broker spawns the agent
#    subprocess and controls which env vars reach it via [agent].env /
#    [agent].inherit_env). kubectl exec into the broker pod runs *inside*
#    the broker, not as a spawned agent, so the test there would not
#    exercise the agent's auth path.
kubectl set env deploy/openab ANTHROPIC_API_KEY-

# 2. Roll the deployment so the new env takes effect
kubectl rollout restart deploy/openab
kubectl rollout status deploy/openab

# 3. Send any prompt in Discord / Slack. The user-facing message should
#    now include the stderr line under a "Recent agent output:" blockquote:
#
#    ⚠️ **Internal Error** (code: -32603)
#    Internal error
#    > _Recent agent output:_
#    > Error: ANTHROPIC_API_KEY not set
#
# If the agent populates `error.data.message` (codex-acp, claude-agent-acp
# in most configs), the `data.message` text is shown instead of the stderr
# tail — that is the documented precedence, not a regression.
```

## Other categories

- **Connection Lost** — agent process crashed mid-prompt. `kubectl logs` for the
  broker will show `Agent process died` from the liveness check in
  `AdapterRouter::stream_prompt_blocks`.
- **Request Timeout** — agent didn't respond within 30s (or 120s for
  `session/new`). Either upstream is slow, agent is hung on a tool call, or
  the env config is wrong and initialization is looping.
- **Agent Not Found** — the configured `command` doesn't exist or isn't
  executable. Check `[agent].command` in `config.toml`.
- **Service Busy** — `pool.max_sessions` reached. Increase the limit or
  wait for existing sessions to TTL out (`pool.session_ttl_hours`).
