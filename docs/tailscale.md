# Tailscale Integration


> *"Agents stay as thin clients, OpenAB as the thin bridge — let the remote monsters take the heavy lifting."*

How to give an OpenAB bot network access into a private Tailscale tailnet (e.g. a home lab) via lifecycle hooks — no custom image build required.

## Why

OAB containers run in ephemeral, unprivileged environments (ECS Fargate, Kubernetes with no elevated capabilities). A bot that needs to reach private infrastructure — SSH into a home lab host, hit an internal API, query a database — normally has no path to that network.

Tailscale's **userspace-networking mode** solves this without root, without a TUN device, and without modifying the container image: `tailscaled` runs as a regular unprivileged process and joins the tailnet purely in userspace.

## Design Principles

- **Per-pod / per-agent isolation, not host-shared.** Each bot runs its own `tailscaled` process inside its own container and joins the tailnet as its **own node** with its **own identity** (own Tailscale IP, own hostname, own ACL scope). It does **not** reuse or tunnel through the underlying host's network stack or an existing Tailscale connection on the node the container happens to be scheduled on. Two bots on the same physical host are two independent, individually-scoped tailnet nodes — one bot's tailnet access is not implicitly shared with another's.
- **Survives restarts.** `tailscaled.state` (node identity/keys) and `~/.ssh/` round-trip through the same `pre_shutdown` → S3 → `pre_seed` backup/restore cycle already used for agent auth (see [Persistence](#persistence)). A redeploy, Spot interruption, or `ecsctl restart` does not require re-registering the node or regenerating SSH keys.
- **Runtime-portable — the same recipe works everywhere.** Because the whole pattern is unprivileged userspace processes plus S3 objects plus one shell script, it is not tied to any specific compute platform. The identical `pre_seed`/`pre_boot` configuration runs unmodified on:
  - AWS ECS Fargate (what this doc's examples target)
  - Any Kubernetes distribution (EKS, k3s, self-managed) — no `NET_ADMIN` capability or privileged pod needed
  - Zeabur or other container-PaaS instances
  - OrbStack / Docker Desktop on a local macOS laptop
  
  There is no platform-specific branch anywhere in this setup — only environment variables (`STATE_BUCKET`, `OPENAB_AGENT_NAME`, cloud credentials) differ between deployments.

## Architecture

```
                         ┌───────────────────────────────────┐
                         │  S3: shared/tailscale-bin.tar.gz  │
                         │  (tailscale, tailscaled, ssh)     │
                         └────────────────┬───────────────────┘
                                          │ pre_seed (download + extract)
                                          ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  OpenAB Container (unprivileged, e.g. ECS Fargate)                     │
│                                                                         │
│  $HOME/.local/bin/{tailscale,tailscaled}   $HOME/bin/{ssh,scp}          │
│                                                                         │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │ pre_boot (runs as unprivileged agent user)                       │  │
│  │                                                                   │  │
│  │  1. aws secretsmanager get-secret-value ─────┐                   │  │
│  │                                              ▼                   │  │
│  │                                   ┌─────────────────────────┐    │  │
│  │                                   │ Secrets Manager         │    │  │
│  │                                   │ my-app/tailscale         │    │  │
│  │                                   │ {"..._KEY":"tskey-..."}  │    │  │
│  │                                   └─────────────────────────┘    │  │
│  │                                                                   │  │
│  │  2. tailscaled --tun=userspace-networking \                      │  │
│  │       --socks5-server=localhost:1055 \                           │  │
│  │       --outbound-http-proxy-listen=localhost:1055 &              │  │
│  │                                                                   │  │
│  │  3. tailscale up --authkey=$TS_AUTHKEY                            │  │
│  └───────────────────────────────┬───────────────────────────────────┘  │
│                                  │                                     │
│                                  ▼                                     │
│                   ┌───────────────────────────┐                        │
│                   │  tailscaled (userspace)   │                        │
│                   │  • no TUN device          │                        │
│                   │  • no root required       │                        │
│                   │  • SOCKS5 + HTTP CONNECT  │                        │
│                   │    proxy on :1055         │                        │
│                   │  • MagicDNS resolved      │                        │
│                   │    internally, bypassing  │                        │
│                   │    /etc/resolv.conf       │                        │
│                   └─────────────┬─────────────┘                        │
│                                │                                       │
│  Runtime access patterns:      │                                       │
│                                │                                       │
│   curl (ALL_PROXY=socks5h://localhost:1055) ──┤                        │
│   ssh via ~/.ssh/config ProxyCommand          │                        │
│     "tailscale nc %h %p" ─────────────────────┤                        │
└────────────────────────────────┼───────────────────────────────────────┘
                                 │
                                 │  encrypted WireGuard tunnel
                                 ▼
                  ┌──────────────────────────────────────┐
                  │          Tailscale Tailnet          │
                  │                                      │
                  │  ┌─────────┐  ┌────────┐  ┌────────┐ │
                  │  │ macmini │  │  rpi2  │  │  ...   │ │
                  │  │100.x.x.x│  │100.x.x.x│  │        │ │
                  │  └─────────┘  └────────┘  └────────┘ │
                  └──────────────────────────────────────┘
```

**Key points illustrated above:**

- `pre_seed` is a pure download/extract step — no code runs, just S3 → filesystem
- `pre_boot` runs as the same unprivileged user as the main agent process; it fetches its own secret directly (see [why below](#why-the-authkey-cant-use-secretsrefs)) since `[secrets.refs]` isn't resolved yet at this point in the lifecycle
- `tailscaled` never touches `/etc/resolv.conf` or needs root — everything routes through its own SOCKS5/HTTP proxy or the `nc` subcommand
- Once `tailscale up` succeeds, the container is a first-class (if ephemeral) node in the tailnet, reachable by and able to reach every other node per your ACL policy

## Prerequisites

- A [Tailscale](https://tailscale.com) account with the target hosts already joined to your tailnet
- A **reusable + ephemeral** auth key ([Admin Console → Settings → Keys](https://login.tailscale.com/admin/settings/keys))
  - **Reusable**: survives multiple container boots (Fargate Spot churn, redeploys)
  - **Ephemeral**: node auto-removes from the tailnet when it disconnects — prevents zombie nodes accumulating from Spot interruptions
- The task/pod IAM role needs `secretsmanager:GetSecretValue` on the secret holding the authkey

## Why the authkey can't use `[secrets.refs]`

OpenAB's boot sequence resolves `[secrets.refs]` **after** `pre_boot` runs:

```
1. Parse config.toml
2. Run [hooks.pre_boot]        ← tailscaled needs to start here
3. Resolve [secrets.refs]      ← too late for pre_boot
4. Substitute ${secrets.*}
```

`${secrets.ts_authkey}` inside `pre_boot.inline` would pass through as a literal, unexpanded string — `pre_boot` is a separate process that has already exited by the time secrets resolve. `pre_boot` must fetch the authkey itself, using whatever cloud credentials are available in its sanitized environment (`AWS_*`, `ECS_CONTAINER_METADATA_URI*` are passed through — see [hooks.md](hooks.md#environment)).

## Step 1: Build the binary layer

Bundle `tailscale`, `tailscaled`, and (optionally) a static `ssh`/`scp`/`ssh-keygen` into one `pre_seed` layer.

```bash
# Tailscale binaries (match your container's arch)
curl -fsSL https://pkgs.tailscale.com/stable/tailscale_1.98.8_amd64.tgz -o ts.tgz
tar xzf ts.tgz
mkdir -p layer/.local/bin layer/bin
cp tailscale_1.98.8_amd64/{tailscale,tailscaled} layer/.local/bin/

# Optional: OpenSSH client, extracted from the official .deb (no sudo needed)
curl -fsSL -o openssh-client.deb \
  http://ftp.debian.org/debian/pool/main/o/openssh/openssh-client_9.2p1-2+deb12u10_amd64.deb
mkdir ssh-extract && cd ssh-extract
ar x ../openssh-client.deb
tar xf data.tar.xz ./usr/bin/ssh ./usr/bin/scp ./usr/bin/ssh-keygen
cp usr/bin/{ssh,scp,ssh-keygen} ../layer/bin/
cd .. && chmod +x layer/.local/bin/* layer/bin/*

tar -czf tailscale-bin.tar.gz -C layer .
```

> **Verify the `.deb` checksum** against the one listed on the [Debian package page](https://packages.debian.org/bookworm/amd64/openssh-client/download) before extracting.

Upload **without** `--checksum-algorithm SHA256` — see [Gotcha: S3 multipart checksums](#gotcha-s3-multipart-checksums-break-pre_seed) below.

```bash
aws s3 cp tailscale-bin.tar.gz s3://my-bucket/shared/tailscale-bin.tar.gz
```

## Step 2: Store the authkey

```bash
aws secretsmanager create-secret --name my-app/tailscale \
  --secret-string '{"MYBOT_TAILSCALE_KEY":"tskey-auth-..."}'
```

Grant the task role read access:

```json
{
  "Effect": "Allow",
  "Action": ["secretsmanager:GetSecretValue"],
  "Resource": "arn:aws:secretsmanager:*:*:secret:my-app/tailscale-*"
}
```

## Step 3: Configure `pre_seed` + `pre_boot`

```toml
[hooks.pre_seed]
sources = [
  "s3://my-bucket/shared/base.tar.gz",
  "s3://my-bucket/shared/utils.tar.gz",        # provides aws CLI — see below
  "s3://my-bucket/shared/tailscale-bin.tar.gz",
  "s3://my-bucket/my-bot-home.tar.gz",          # last layer wins — personal state preserved
]
timeout_seconds = 120
on_failure = "abort"

[hooks.pre_boot]
timeout_seconds = 60
on_failure = "warn"
inline = '''
#!/bin/sh
set -e

export PATH="$HOME/bin:$HOME/.local/bin:$PATH"

if [ -x "$HOME/.local/bin/tailscaled" ]; then
  mkdir -p "$HOME/.local/share/tailscale"

  # Remove stale socket from previous run to avoid race condition
  rm -f /tmp/tailscaled.sock

  "$HOME/.local/bin/tailscaled" \
    --state="$HOME/.local/share/tailscale/tailscaled.state" \
    --socket=/tmp/tailscaled.sock \
    --tun=userspace-networking \
    --socks5-server=localhost:1055 \
    --outbound-http-proxy-listen=localhost:1055 \
    >/tmp/tailscaled.log 2>&1 &

  for i in 1 2 3 4 5 6 7 8 9 10; do
    [ -S /tmp/tailscaled.sock ] && break
    sleep 1
  done

  TS_AUTHKEY=$(aws secretsmanager get-secret-value \
    --secret-id my-app/tailscale \
    --query SecretString --output text \
    --region us-east-1 2>/dev/null | \
    grep -o '"MYBOT_TAILSCALE_KEY"[[:space:]]*:[[:space:]]*"[^"]*"' | cut -d'"' -f4)

  if [ -n "$TS_AUTHKEY" ]; then
    export TS_AUTHKEY
    "$HOME/.local/bin/tailscale" \
      --socket=/tmp/tailscaled.sock \
      up \
      --hostname="${OPENAB_AGENT_NAME:-openab-bot}" \
      --accept-routes \
      --timeout=30s && echo "export ALL_PROXY=socks5h://localhost:1055" > "$HOME/.tailscale-proxy.env" \
      || echo "tailscale up failed, continuing without VPN"
    unset TS_AUTHKEY
  else
    echo "tailscale: authkey not found — tailscaled running with existing state (if any)"
  fi
else
  echo "tailscale: binary not found, skipping"
fi
'''
```

### `--socks5-server` / `--outbound-http-proxy-listen`

Userspace-networking mode has no TUN device, so `tailscaled` cannot manage `/etc/resolv.conf` (and typically can't — the process runs as an unprivileged user with no write access to `/etc/*`). Without this, MagicDNS hostnames (`ssh macmini`) fail with `Could not resolve hostname`, even though the tailnet connection itself is up.

Passing both flags starts a combined SOCKS5 + HTTP CONNECT proxy on `localhost:1055`. Any tool that resolves through the proxy (not the OS resolver) gets working MagicDNS:

```bash
export ALL_PROXY=socks5h://localhost:1055   # note the trailing 'h' — resolves DNS via the proxy
curl https://macmini.tailnet-name.ts.net/health
```

### SSH via `tailscale nc`

`curl`'s SOCKS proxy support only works for request/response protocols — it does not tunnel a persistent bidirectional stream, so it cannot proxy a full SSH session. Use Tailscale's own `nc` subcommand as `ProxyCommand` instead:

```bash
mkdir -p ~/.ssh
ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519 -N "" -C "mybot@openab"

cat > ~/.ssh/config << 'EOF'
Host *.ts.net myhost1 myhost2
    ProxyCommand $HOME/.local/bin/tailscale --socket=/tmp/tailscaled.sock nc %h %p
    StrictHostKeyChecking accept-new
    User myuser
    ConnectTimeout 10
    ServerAliveInterval 15
    ServerAliveCountMax 3
    ControlMaster auto
    ControlPath ~/.ssh/control-%r@%h:%p
    ControlPersist 10m
EOF
chmod 600 ~/.ssh/config ~/.ssh/id_ed25519
```

Add `~/.ssh/id_ed25519.pub` to the target host's `authorized_keys`, then `ssh myhost1` works with plain hostname resolution — no MagicDNS/`resolv.conf` dependency at all, since `tailscale nc` resolves and relays the TCP stream entirely inside `tailscaled`.

## Thin Client, Thin Bridge, Remote Monsters

This integration is built around a specific division of labor, not just "give the bot network access":

```
┌───────────────┐        ┌───────────────┐        ┌─────────────────────────┐
│  Chat platform │ ─────► │  OAB (thin    │ ─SSH──► │  Remote "monster" host │
│  (Discord etc) │        │  bridge)      │        │  (macmini, rpi2, ...)   │
└───────────────┘        └───────────────┘        └─────────────────────────┘
                          thin client:              heavy lifting:
                          • LLM inference            • cargo build / compilation
                          • conversation state        • large file processing
                          • tool-call orchestration   • GPU/CPU-bound workloads
                          • SSH dispatch only          • anything that doesn't fit
                                                        in a small Fargate task
```

- **OAB agents are thin clients.** The bot container itself should do inference, conversation/session management, and orchestration — not compute-heavy work. It has no business running `cargo build`, video transcoding, or anything CPU/memory-intensive locally.
- **OAB is the thin bridge, not the workhorse.** The Tailscale connection exists so the agent can **dispatch** work over SSH to a trusted, adequately-provisioned host — the "remote monster" — and bring back results. The container stays small (matching its ECS task CPU/memory allocation), while the actual heavy lifting happens on hardware sized for it.
- **Remote monsters do the real work.** A beefy Mac mini (M4, 16GB), a home lab Linux box, or any other trusted host on the tailnet is where compilation, builds, and resource-intensive jobs actually run — reached via `ssh` through the tunnel this doc sets up. See [Build Offloading](principles.md#build-offloading) for a concrete example: never run `cargo build` on the light bot container — SSH to a capable host, build there, `scp` the artifact back.

This keeps container sizing cheap and predictable (no bot needs to provision for a worst-case compile job) while still giving agents access to real compute when a task genuinely needs it.

## Persistence

- `~/.ssh/`, `~/.local/share/tailscale/tailscaled.state` (node identity/keys) persist across restarts via the same `pre_shutdown` → S3 → `pre_seed` round-trip used for agent auth — see [hooks.md](hooks.md#real-world-example-s3-restore--backup-round-trip). Nothing needs to be added to the exclude list.
- Because the auth key is **ephemeral**, if the container is destroyed without a clean `pre_shutdown` (OOM kill, host failure) the node disappears from the tailnet on its own — no manual cleanup needed in the admin console.
- If `tailscaled.state` *is* restored from a previous boot, `tailscale up --authkey=...` reuses the existing node identity rather than re-registering, so the tailnet doesn't accumulate duplicate entries per restart.

## Gotcha: S3 multipart checksums break `pre_seed`

`aws s3 cp --checksum-algorithm SHA256` on a file large enough to trigger multipart upload (observed above ~8 MiB) stores a **composite** checksum in the form `<hash>-<part-count>` (e.g. `e+Yy...Ess=-5`). This is not valid standalone base64, and OpenAB's automatic S3-checksum verification (see [hooks.md](hooks.md#safety)) fails to parse it:

```
hooks.pre_seed failed (on_failure=abort) layer=5 error=hooks.pre_seed: invalid base64 in S3 checksum: Invalid symbol 61, offset 43.
```

**Fix**: upload without `--checksum-algorithm`:

```bash
aws s3 cp tailscale-bin.tar.gz s3://my-bucket/shared/tailscale-bin.tar.gz
```

If you need integrity verification, use the `sha256s` field in `[hooks.pre_seed]` config instead of relying on S3-native checksums for archives that will be multipart-uploaded.

## Gotcha: `pre_seed` max 5 sources

`[hooks.pre_seed].sources` is capped at 5 entries. If you're already at the limit, look for redundant layers to merge instead of adding a 6th:

- `shared/utils.tar.gz` (if your fleet has one) often already bundles `aws` CLI — check before adding a separate AWS CLI layer
- A `gh`/`ghp`-only layer can usually be superseded by a broader `utils` layer without any bot losing functionality (verify with `tar -tzf` before swapping)

## Verifying the connection

```bash
# From inside the agent's shell/exec session:

# 1. Check daemon status and tailnet membership
$HOME/.local/bin/tailscale --socket=/tmp/tailscaled.sock status
```

You should see your bot's hostname alongside the rest of your tailnet, each with its Tailscale IP (`100.x.x.x`).

```bash
# 2. End-to-end connectivity test via SOCKS5 proxy
curl -I --proxy socks5h://localhost:1055 http://<your-tailnet-host>:port/

# 3. SSH connectivity test (if SSH is configured)
ssh -o BatchMode=yes -o ConnectTimeout=5 myhost1 echo "ok"
```

If step 1 succeeds but step 2 fails, check your Tailscale ACL rules — the bot's tag may not have permission to reach the target host/port.

## Security Notes

- The authkey grants join access to your **entire tailnet** — scope it with [Tailscale ACL tags](https://tailscale.com/kb/1068/acl-tags) (e.g. `tag:oab-bot`) restricting which hosts/ports it can reach, rather than relying on the default "trust everything" tailnet policy.
- Treat the authkey with the same sensitivity as any other bot credential — Secrets Manager, not baked into an image layer or committed to a config repo.
- `--accept-routes` lets the bot reach subnets advertised by other tailnet nodes (e.g. a home LAN behind a subnet router). Omit it if the bot should only reach other tailnet *nodes* directly, not routed subnets.
- **Key expiry**: Tailscale nodes have a default key expiry of 180 days. Long-running OAB agents will lose connectivity when the node key expires. To prevent this, disable key expiry for your bot nodes in the [Admin Console → Machines](https://login.tailscale.com/admin/machines) page (click the node → Disable key expiry), or apply an ACL `autoApprovers` policy for your `tag:oab-bot` tag that automatically approves re-authentication.
