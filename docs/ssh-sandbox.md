# SSH Sandbox for Local Deployments

OpenAB targets k3s on cloud, where Kubernetes NetworkPolicy and Pod isolation handle security. For local deployments (developer laptop, home server), the default config runs the agent with full host permissions:

```toml
[agent]
command = "claude"
args = ["--acp"]
```

The Claude subprocess inherits the host's full filesystem and network access. For a Discord bot accepting messages from arbitrary users, this is a meaningful attack surface.

## SSH as a Zero-Code-Change Transport

`AcpConnection::spawn()` treats the agent as a stdio JSON-RPC process. SSH is a transparent byte pipe over that same stdio — no changes to the ACP protocol, `SessionPool`, or `AcpConnection` internals are needed.

```
Current                              Proposed
───────────────────────              ──────────────────────────────
OpenAB                               OpenAB
  │ spawn                              │ spawn
  ▼                                    ▼
claude (host permissions)            ssh -T user@sandbox
  ├─ reads ~/.ssh ✗                    │ encrypted stdio pipe
  ├─ reads ~/Documents ✗               ▼
  └─ unrestricted network ✗          claude (inside sandbox)
                                       ├─ restricted filesystem ✓
                                       ├─ network allowlist ✓
                                       └─ MCP via host proxy ✓
```

## Configuration

```toml
[agent]
command = "ssh"
args = [
  "-T",                                     # no PTY — required (see below)
  "-o", "BatchMode=yes",                    # fail-fast, no interactive prompts
  "-o", "ServerAliveInterval=30",           # keep-alive for long sessions
  "-o", "ServerAliveCountMax=3",
  "-o", "StrictHostKeyChecking=accept-new", # daemon has no terminal for prompts
  "user@sandbox-host",
  "claude", "--acp"
]
working_dir = "/tmp"
```

### Why `-T` Is Required

| Flag | Behavior | JSON-RPC safe? |
|------|----------|----------------|
| `-T` | Clean byte pipe, stderr separated | Yes |
| `-t` | Warns "PTY not allocated", stderr leaks into stdout | No — corrupts JSON stream |
| `-tt` | Forced PTY + piped stdin → hangs indefinitely | No — deadlock |

PTY inserts CR/LF conversion (`\n` → `\r\n`), merges stderr into stdout, and enables echo mode — all of which break JSON-RPC parsing. **`-T` is mandatory, not optional.**

### Why `BatchMode=yes`

OpenAB runs as a daemon without a terminal. Interactive password prompts will hang the process. `BatchMode=yes` forces fail-fast behavior. SSH key-based auth must be configured beforehand.

## Sandbox Options

The SSH target is your choice — OpenAB does not care what is behind the SSH connection:

| Environment | SSH target | Notes |
|-------------|-----------|-------|
| Mac (OrbStack) | `vm-name@orb` | Via `~/.orbstack/ssh/config` ProxyCommand |
| Linux | `user@nspawn-container` | systemd-nspawn with SSH |
| Remote machine | `user@10.0.0.5` | Any Linux server |
| Docker | wrapper script using `docker exec` | Alternative to SSH |

## MCP Server Access from Sandbox

If MCP servers run on the host, the sandbox cannot reach them via `localhost` (which resolves to the sandbox's own loopback). Options:

**Option A: Host DNS alias (OrbStack)**
```
claude (VM) ──http://host.internal:PORT──► MCP server (host)
```

**Option B: SSH port forwarding (universal)**
```bash
# add to ssh args:
"-L", "8080:localhost:8080"
```
```
claude (VM) ──http://localhost:8080──► [tunnel] ──► MCP server (host)
```

**Option C: Network bridge (Docker `--network host`)**
```
claude (container) ──http://localhost:PORT──► MCP server (host)
```

## Known Limitations

### `kill_on_drop` does not reliably terminate remote processes

Killing the local SSH client process leaves the remote subprocess running. The SSH server sends SIGHUP to the remote shell, but the agent may survive (especially with `nohup` or ControlMaster active).

Mitigations:
- Do **not** use SSH ControlMaster for agent connections
- Ensure the SSH server has `ClientAliveInterval` set to detect dead clients
- Session pool TTL cleanup (`session_ttl_hours`) will eventually reclaim idle sessions

### SSH connection startup latency

Each `AcpConnection::spawn()` incurs an SSH handshake (~50–200 ms). This is negligible for long-lived sessions (default pool TTL = 24 h), but noticeable if sessions are frequently recycled.

### SSH key auth is required

OpenAB runs as a daemon without a terminal. Configure SSH key-based authentication on the sandbox host before using this transport.
