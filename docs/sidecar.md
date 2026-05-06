# Extending OpenAB with Sidecars and Init Containers

> This is an **advanced** pattern. For most tool installations, start with [agent-installable tools](agent-installable-tools.md) — one prompt, zero YAML.

## Overview

OpenAB's Helm chart supports **init containers** and **sidecar containers** for cases where the agent-installable tools pattern isn't sufficient — when you need a long-running process, a deterministic pre-installed toolset, or a service running alongside the agent.

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

## Common Scenarios

| # | Scenario | Type | Why a sidecar / init container? |
|---|----------|------|---------------------------------|
| 1 | [Expose agent via Cloudflare Tunnel](#1-cloudflare-tunnel) | Sidecar | Long-running tunnel process that must stay alive alongside the agent |
| 2 | [Expose agent via ngrok / Tailscale](#2-ngrok--tailscale) | Sidecar | Same as above — persistent network tunnel |
| 3 | [Pre-install a standard toolset](#3-pre-install-a-standard-toolset) | Init | Deterministic setup — tools are guaranteed before the agent starts |
| 4 | [Local database or cache](#4-local-database-or-cache) | Sidecar | Agent needs a data store accessible via localhost |
| 5 | [Log shipping and observability](#5-log-shipping-and-observability) | Sidecar | Continuous log/metric forwarding independent of the agent |
| 6 | [GPU / ML model serving](#6-gpu--ml-model-serving) | Sidecar | Serve a model via HTTP on localhost; agent calls it as needed |

---

## 1. Cloudflare Tunnel

**Problem:** You want to expose your agent's webhook or API to the internet without a public IP or LoadBalancer.

**Solution:** Run `cloudflared` as a sidecar. It creates an outbound tunnel to Cloudflare's edge, making your agent reachable via a Cloudflare domain.

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
        env:
          - name: TUNNEL_TOKEN
            valueFrom:
              secretKeyRef:
                name: cloudflare-secret
                key: tunnel-token
        resources:
          requests:
            cpu: 50m
            memory: 64Mi
          limits:
            memory: 128Mi
```

**How it works:** The tunnel connects outbound to Cloudflare. Incoming requests are routed to `localhost:<port>` inside the pod, where the agent (or gateway) is listening. No ingress, no LoadBalancer, no firewall rules needed.

---

## 2. ngrok / Tailscale

**Problem:** Same as Cloudflare Tunnel, but you prefer ngrok or Tailscale for your network layer.

### ngrok

```yaml
agents:
  kiro:
    extraContainers:
      - name: ngrok
        image: ngrok/ngrok:latest
        args:
          - http
          - --authtoken=$(NGROK_AUTHTOKEN)
          - "8080"
        env:
          - name: NGROK_AUTHTOKEN
            valueFrom:
              secretKeyRef:
                name: ngrok-secret
                key: authtoken
```

### Tailscale

```yaml
agents:
  kiro:
    extraContainers:
      - name: tailscale
        image: tailscale/tailscale:latest
        env:
          - name: TS_AUTHKEY
            valueFrom:
              secretKeyRef:
                name: tailscale-secret
                key: authkey
          - name: TS_USERSPACE
            value: "true"
        securityContext:
          runAsUser: 1000
```

**When to choose which:**
- **Cloudflare Tunnel** — free, production-grade, custom domains, access policies
- **ngrok** — quick dev/testing, instant public URL, built-in inspection UI
- **Tailscale** — private mesh network, no public exposure, zero-config VPN between your devices and the agent

---

## 3. Pre-install a Standard Toolset

**Problem:** You want every agent pod to start with a specific set of tools already installed, without relying on the agent to install them at runtime.

**Solution:** Use an init container that writes to the agent's PVC before the main container starts.

```yaml
agents:
  kiro:
    extraInitContainers:
      - name: install-tools
        image: curlimages/curl:latest
        command:
          - sh
          - -c
          - |
            set -e
            mkdir -p /home/agent/bin

            # Skip if tools already exist (PVC is persistent)
            if [ -f /home/agent/bin/.tools-installed ]; then
              echo "Tools already installed, skipping"
              exit 0
            fi

            ARCH=$(uname -m)
            if [ "$ARCH" = "aarch64" ]; then ARCH="arm64"; elif [ "$ARCH" = "x86_64" ]; then ARCH="amd64"; fi

            # Install kubectl
            KUBECTL_VERSION=$(curl -fsSL https://dl.k8s.io/release/stable.txt)
            curl -fsSL -o /home/agent/bin/kubectl \
              "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${ARCH}/kubectl"
            chmod +x /home/agent/bin/kubectl

            # Add more tools here...

            touch /home/agent/bin/.tools-installed
        volumeMounts:
          - name: agent-home
            mountPath: /home/agent
```

**Key detail:** The init container checks for a marker file (`.tools-installed`) so it doesn't re-download on every pod restart — the PVC already has the tools.

**When to use this over agent-installable tools:**
- **Team standardization** — every pod gets the same toolset, no variance
- **Offline / air-gapped** — pre-bake tools from a private registry
- **Large tools** — avoid the agent spending time downloading hundreds of MB on first run

---

## 4. Local Database or Cache

**Problem:** Your agent needs a database or cache (Redis, SQLite server, DuckDB, etc.) accessible via localhost.

**Solution:** Run it as a sidecar. The agent connects to `localhost:<port>`.

```yaml
agents:
  kiro:
    extraContainers:
      - name: redis
        image: redis:7-alpine
        ports:
          - containerPort: 6379
        resources:
          requests:
            cpu: 50m
            memory: 64Mi
          limits:
            memory: 256Mi
```

The agent can then use `redis-cli -h localhost` or connect programmatically to `localhost:6379`.

**Use cases:**
- Caching API responses or embeddings
- Session storage for multi-turn workflows
- Temporary data processing (agent writes data, queries it)

---

## 5. Log Shipping and Observability

**Problem:** You want to ship agent logs, metrics, or traces to an external system (Datadog, Grafana, CloudWatch, etc.) without modifying the agent itself.

**Solution:** Run a log shipper as a sidecar that reads from shared volumes or stdout.

```yaml
agents:
  kiro:
    extraContainers:
      - name: fluent-bit
        image: fluent/fluent-bit:latest
        volumeMounts:
          - name: agent-home
            mountPath: /home/agent
            readOnly: true
        env:
          - name: OUTPUT_HOST
            value: "your-log-endpoint.example.com"
```

**Alternatives:** Fluent Bit, Vector, OpenTelemetry Collector, Datadog Agent.

---

## 6. GPU / ML Model Serving

**Problem:** Your agent needs access to a local ML model (e.g., for embeddings, classification, or code analysis) without calling an external API.

**Solution:** Run a model server as a sidecar. The agent calls it via `localhost`.

```yaml
agents:
  kiro:
    extraContainers:
      - name: embedding-server
        image: your-registry/embedding-server:latest
        ports:
          - containerPort: 8000
        resources:
          limits:
            nvidia.com/gpu: 1
```

The agent calls `curl localhost:8000/embed -d '{"text": "..."}'` to get embeddings locally.

> Requires GPU-enabled nodes and the NVIDIA device plugin. This is a niche use case.

---

## Helm Values Reference

All fields are under `agents.<name>`:

| Field | Type | Description |
|-------|------|-------------|
| `extraInitContainers` | list | Init containers — run before the main container |
| `extraContainers` | list | Sidecar containers — run alongside the main container |
| `extraVolumes` | list | Additional volumes for the pod |
| `extraVolumeMounts` | list | Additional volume mounts for the main container |

These accept standard Kubernetes container and volume specs. See the [Kubernetes docs](https://kubernetes.io/docs/concepts/workloads/pods/init-containers/) for the full spec.

## When to Use What

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
