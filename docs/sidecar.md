# Extending OpenAB with Sidecars and Init Containers

## Overview

OpenAB's Helm chart supports **init containers** and **sidecar containers** via `extraInitContainers`, `extraContainers`, `extraVolumes`, and `extraVolumeMounts`. This is an advanced pattern for cases where the [agent-installable tools](agent-installable-tools.md) approach isn't sufficient.

```
  ┌─────────────────────────────────────────────────────────┐
  │  Pod                                                    │
  │                                                         │
  │  ┌─────────────────┐    ┌─────────────────────────────┐│
  │  │  Init Container  │───►│  Main Container (openab)    ││
  │  │                  │    │                              ││
  │  │  Runs BEFORE the │    │  Agent runtime + tools from ││
  │  │  main container. │    │  ~/bin/ (PVC)               ││
  │  │  Pre-install     │    │                              ││
  │  │  tools, seed     │    └─────────────────────────────┘│
  │  │  configs, etc.   │                                   │
  │  └─────────────────┘    ┌─────────────────────────────┐│
  │                          │  Sidecar Container          ││
  │                          │                              ││
  │                          │  Runs ALONGSIDE the main    ││
  │                          │  container. Proxies, tunnels,││
  │                          │  log shippers, daemons, etc. ││
  │                          └─────────────────────────────┘│
  │                                                         │
  │  Shared: volumes, network (localhost), PVC              │
  └─────────────────────────────────────────────────────────┘
```

## When to Use What

| Approach | Use When |
|----------|----------|
| **Agent-installable tools** ([docs](agent-installable-tools.md)) | Installing CLI tools, binaries, libraries. One prompt, agent handles it. **Start here.** |
| **Init container** | You need tools pre-installed before the agent starts, or you want a deterministic setup that doesn't depend on agent behavior. |
| **Sidecar container** | You need a long-running process alongside the agent — a proxy, tunnel, database, daemon, or service that the agent connects to via localhost. |

## Init Containers

Init containers run **before** the main OpenAB container starts. They share volumes with the main container, so they can pre-install tools to `~/bin/` on the PVC.

### Example: Pre-install a tool

```yaml
agents:
  kiro:
    extraVolumeMounts:
      - name: agent-home
        mountPath: /home/agent
    extraInitContainers:
      - name: install-mytool
        image: curlimages/curl:latest
        command:
          - sh
          - -c
          - |
            mkdir -p /home/agent/bin
            curl -fsSL -o /home/agent/bin/mytool "https://example.com/mytool-linux-$(uname -m)"
            chmod +x /home/agent/bin/mytool
        volumeMounts:
          - name: agent-home
            mountPath: /home/agent
```

> The init container writes to the same PVC that the main container uses. When the agent starts, `~/bin/mytool` is already there.

### When init containers make sense

- **Deterministic setup** — the tool is always there, regardless of agent behavior
- **Heavy downloads** — large tools (hundreds of MB) that you don't want the agent to re-download on every fresh session
- **Team standardization** — ensure every agent pod starts with the same toolset

## Sidecar Containers

Sidecar containers run **alongside** the main container for the lifetime of the pod. They share the pod's network (localhost) and can share volumes.

### Example: Cloudflare Tunnel

```yaml
agents:
  kiro:
    extraContainers:
      - name: cloudflared
        image: cloudflare/cloudflared:latest
        args:
          - tunnel
          - --no-autoupdate
          - run
          - --token
          - "$(CLOUDFLARE_TUNNEL_TOKEN)"
        env:
          - name: CLOUDFLARE_TUNNEL_TOKEN
            valueFrom:
              secretKeyRef:
                name: cloudflare-secret
                key: tunnel-token
```

### When sidecars make sense

- **Network proxies / tunnels** — Cloudflare Tunnel, ngrok, Tailscale
- **Databases** — local Redis, SQLite server, or other data stores the agent needs
- **Log shippers** — Fluent Bit, Vector, or other observability agents
- **Auth proxies** — OAuth2 proxy, IAM auth sidecar

## Helm Values Reference

All fields are under `agents.<name>`:

| Field | Type | Description |
|-------|------|-------------|
| `extraInitContainers` | list | Init containers — run before the main container |
| `extraContainers` | list | Sidecar containers — run alongside the main container |
| `extraVolumes` | list | Additional volumes for the pod |
| `extraVolumeMounts` | list | Additional volume mounts for the main container |

These accept standard Kubernetes container and volume specs. See the [Kubernetes docs](https://kubernetes.io/docs/concepts/workloads/pods/init-containers/) for the full spec.

## Combining Both Patterns

The agent-installable tools pattern and sidecars are complementary:

```
  ┌──────────────────────────────────────────────────────────────┐
  │                                                              │
  │  Agent-Installable Tools          Sidecars / Init Containers │
  │  ─────────────────────            ──────────────────────────│
  │  • CLI tools (aws, glab, ssh)     • Long-running daemons    │
  │  • One prompt to install          • Network tunnels/proxies │
  │  • Agent-driven, on-demand        • Pre-installed toolsets  │
  │  • No Helm/YAML changes           • Requires values.yaml   │
  │  • Persists on PVC                • Recreated each pod start│
  │                                                              │
  │  Start here ──────────────────►  Use when needed            │
  └──────────────────────────────────────────────────────────────┘
```

Most users will never need sidecars. Start with the [agent-installable tools](agent-installable-tools.md) pattern — it covers the vast majority of use cases with zero YAML.
