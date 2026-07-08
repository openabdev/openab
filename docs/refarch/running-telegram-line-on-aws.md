# Running Telegram / LINE Bots on AWS

Three production-grade reference architectures for deploying OpenAB bots with Telegram
and/or LINE inbound webhooks on AWS ECS Fargate — without requiring Kubernetes.

Each option includes cost estimates, security group configuration, webhook URL setup,
and pitfalls the authors actually hit in production.

---

## Prerequisite Concept: Outbound vs Inbound

OpenAB started as a Discord bot framework. Discord uses **outbound WebSocket** — the bot
connects *to* Discord. No ingress needed. Your ECS task only needs egress to the internet.

Telegram and LINE are different. They send **inbound HTTP POST** (webhooks) *to your bot*.
Your ECS task must accept HTTPS traffic from the public internet on `:8080`.

```
Discord (outbound):          Telegram / LINE (inbound):

  Bot ──WS──▶ Discord GW        Platform ──HTTPS POST──▶ Your Bot
       (no ingress)                   (needs public endpoint)
```

This is why the architectures below all solve one problem: **getting external HTTPS
traffic to your Fargate task's `:8080` without paying for an ALB.**

---

## Architecture Overview

| # | Option | Monthly Cost | Best For |
|---|--------|-------------|----------|
| 1 | **API Gateway HTTP API + VPC Link + Cloud Map** | ~$12–17 | Single/multi bot, no ALB, lowest AWS-native cost |
| 2 | **ALB + ECS Fargate** | ~$20+ | Health checks, auto-scaling, enterprise |
| 3 | **Cloudflare Tunnel sidecar** | ~$5–10 | Already have Cloudflare, simplest setup |

> **Version note**
> OpenAB v0.9.0-beta.4+ ships **unified webhook mode** by default — set
> `TELEGRAM_BOT_TOKEN` or `LINE_CHANNEL_SECRET` + `LINE_CHANNEL_ACCESS_TOKEN`
> as env vars directly on your bot container, no separate gateway service
> needed. The infra paths below work the same regardless of how the platform
> adapter is wired in; only the deployment shape changes.

---

## Option 1: API Gateway HTTP API + VPC Link + Cloud Map (Recommended)

This is the cheapest AWS-native path. It replaces a $16+/month ALB with a ~$1/month
API Gateway HTTP API and uses Cloud Map for service discovery instead of hardcoded IPs.

> **✅ Recommended: use `oabctl`** — one `oabctl apply -f` command provisions this
> entire Option 1 stack (Cloud Map → VPC Link → API Gateway → routes → stage) in
> ~2.5 minutes, replacing all 7 manual steps below. Tested and verified on
> `us-east-1` (2026-07-07).
>
> ```bash
> # Minimal manifest (save as bot.yaml, then oabctl apply -f bot.yaml):
> spec:
>   ingress:
>     type: apigateway
>     paths:
>       - /webhook/telegram
> ```
>
> See [`docs/oabctl.md`](../oabctl.md#ingress--inbound-webhooks-telegram--line)
> for the full ingress reference. Known pitfalls:
> - `us-east-1e` (use1-az3) does **not** support API Gateway VPC Link — avoid
>   subnets in that AZ.
> - `configFrom` (S3 config path) is required even for infra-only testing.
>
> The manual CLI steps below are preserved for readers who want to understand
> the underlying AWS plumbing or who are not using `oabctl`.

### Architecture Diagram

```
                   Internet
                      │
          ┌───────────┴───────────┐
          │  Telegram / LINE      │
          │  Platform             │
          └───────────┬───────────┘
                      │ HTTPS POST
                      ▼
          ┌───────────────────────┐
          │  API Gateway          │  ~$1.00/million requests
          │  (HTTP API)           │
          │  routes:              │
          │   /webhook/telegram   │
          │   /webhook/line       │
          └───────────┬───────────┘
                      │ VPC Link (~$0.01/hr/ENI)
                      ▼
          ┌───────────────────────┐
          │  AWS Cloud Map        │  ~$0 (private DNS)
          │  namespace: oab       │
          │  service: your-bot    │
          │  → DNS SRV record     │
          │    resolves to IP:port │
          └───────────┬───────────┘
                      │
                      ▼
          ┌───────────────────────┐
          │  ECS Fargate Task     │
          │  OpenAB :8080         │
          │  SG: allow inbound    │
          │  from VPC Link prefix │
          └───────────────────────┘
```

### Prerequisites

- ECS Fargate cluster with an OpenAB task running (unified binary)
- VPC with subnets for the ECS tasks (private or public both work; the VPC Link uses its own private subnet ENIs internally)
- The task's security group
- AWS CLI or Console access

> **DNS record type: use SRV, not A**
> API Gateway HTTP API VPC Link integrations require the Cloud Map service **ARN**
> (not an `http://` URL) as the integration URI. The port comes from the **SRV** record —
> A records do **not** carry port information and will fail with
> `BadRequestException: integration uri should be a valid Cloud Map service ARN`.
> Use `{Type="SRV"}` in your Cloud Map service configuration.

### Step-by-Step

#### 1. Create a Cloud Map Namespace & Service

```bash
# Private DNS namespace (one-time per VPC)
aws servicediscovery create-private-dns-namespace \
  --name oab \
  --vpc vpc-abc123 \
  --region us-east-1

# Service with DNS SRV record (auto-registers task IP + port)
aws servicediscovery create-service \
  --name your-bot \
  --namespace-id ns-xxx \
  --dns-config 'NamespaceId="ns-xxx",DnsRecords=[{Type="SRV",TTL="60"}]' \
  --region us-east-1 \
  --query 'Service.Id' --output text  # save this as SERVICE_ID
```

#### 2. Enable Service Discovery on Your ECS Service

ECS service discovery can only be set at **service creation time**, not via update.
If your service already exists, delete and recreate it:

```bash
# Delete existing service (safe when desiredCount=0)
aws ecs delete-service --cluster oab --service your-bot --region us-east-1

# Recreate with service discovery
aws ecs create-service \
  --cluster oab \
  --service-name your-bot \
  --task-definition your-bot \
  --desired-count 0 \
  --launch-type FARGATE \
  --network-configuration \
    "awsvpcConfiguration={subnets=[subnet-xxx,subnet-yyy],securityGroups=[sg-zzz],assignPublicIp=DISABLED}" \
  --service-registries "registryArn=arn:aws:servicediscovery:us-east-1:123456:service/srv-xxx" \
  --region us-east-1

# Start the bot (service was created with desired count 0)
aws ecs update-service --cluster oab --service your-bot --desired-count 1 --region us-east-1
```

When the task starts, ECS auto-registers its private IP and port as the DNS SRV record
for `your-bot.oab`. The SRV record carries both address and port, which the VPC Link
uses to route traffic.

> **ECS task-def naming**: Use the family name (resolves to latest ACTIVE revision)
> or `family:revision` (e.g., `your-bot:1`). ECS does not support Docker-style
> `:latest` tags.

#### 3. Create a VPC Link

```bash
aws apigatewayv2 create-vpc-link \
  --name oab-vpc-link \
  --subnet-ids subnet-xxx subnet-yyy \
  --security-group-ids sg-zzz \
  --region us-east-1
```

> **Important**: The VPC Link needs subnets in the same VPC as your ECS tasks.
> It does NOT need public IPs — it uses private connectivity through AWS Hyperplane.

#### 4. Create an API Gateway HTTP API

```bash
# Create the API
API_ID=$(aws apigatewayv2 create-api \
  --name oab-webhook \
  --protocol-type HTTP \
  --region us-east-1 \
  --query 'ApiId' --output text)

# Get the Cloud Map service ARN (needed for the integration URI)
SERVICE_ARN=$(aws servicediscovery get-service \
  --id $SERVICE_ID \
  --region us-east-1 \
  --query 'Service.Arn' --output text)

# Create the integration FIRST: VPC Link → Cloud Map service ARN
# The integration URI must be the Cloud Map service ARN (not an http:// URL)
INTEGRATION_ID=$(aws apigatewayv2 create-integration \
  --api-id $API_ID \
  --integration-type HTTP_PROXY \
  --integration-method POST \
  --integration-uri "$SERVICE_ARN" \
  --connection-type VPC_LINK \
  --connection-id $VPC_LINK_ID \
  --payload-format-version "1.0" \
  --region us-east-1 \
  --query 'IntegrationId' --output text)

# Add Telegram route (uses the integration created above)
aws apigatewayv2 create-route \
  --api-id $API_ID \
  --route-key "POST /webhook/telegram" \
  --target "integrations/$INTEGRATION_ID" \
  --region us-east-1

# Add LINE route (same VPC Link, different path)
aws apigatewayv2 create-route \
  --api-id $API_ID \
  --route-key "POST /webhook/line" \
  --target "integrations/$INTEGRATION_ID" \
  --region us-east-1

# Deploy
aws apigatewayv2 create-stage \
  --api-id $API_ID \
  --stage-name prod \
  --auto-deploy \
  --region us-east-1
```

Your webhook URL is now:
```
https://{api-id}.execute-api.us-east-1.amazonaws.com/prod/webhook/telegram
https://{api-id}.execute-api.us-east-1.amazonaws.com/prod/webhook/line
```

> **Path passthrough is automatic**: API Gateway HTTP API appends the route path to the
> integration target. The route `POST /webhook/telegram` results in
> `POST /webhook/telegram` hitting the container. No path rewriting needed.
>
> When the Cloud Map service is registered via ECS service discovery, the SRV record
> carries the container port — API Gateway does not need to specify it separately.

This URL **never changes** — even when tasks restart and get new private IPs, Cloud Map
auto-updates the DNS SRV record and the VPC Link resolves the new IP transparently.

#### 5. Security Group: Inbound Rules

The biggest pitfall when moving from Discord-only to Telegram/LINE.

**Before (Discord-only — outbound only):**

| Type | Protocol | Port | Source | Purpose |
|------|----------|------|--------|---------|
| — | — | — | — | No inbound rules needed |

**After (Telegram/LINE ready — inbound webhook):**

| Type | Protocol | Port | Source | Purpose |
|------|----------|------|--------|---------|
| HTTP | TCP | 8080 | Self (sg-xxx) | Inter-service WS (bot ↔ gateway) |
| HTTP | TCP | 8080 | VPC Link prefix list | API Gateway → task traffic |

> **Pitfall**: If you use self-referencing SG rules (same SG as source), the VPC Link
> traffic arrives from the VPC Link's ENI — which is in your SG. A self-referencing
> inbound rule on `:8080` covers both inter-service and VPC Link traffic in most cases.
> If not, add the VPC Link's prefix list explicitly.

#### 6. Set Webhook URLs

**Telegram (BotFather / setWebhook):**

Generate a random webhook secret first, then use the same value for both the env
var and the `secret_token` query parameter — Telegram echoes it back as the
`X-Telegram-Bot-Api-Secret-Token` header on every webhook, and OpenAB rejects
requests whose header doesn't match (see the **Security** callout in §7 below).

```bash
TELEGRAM_SECRET_TOKEN="$(openssl rand -hex 32)"

curl "https://api.telegram.org/bot<TOKEN>/setWebhook?url=https://{api-id}.execute-api.us-east-1.amazonaws.com/prod/webhook/telegram&secret_token=${TELEGRAM_SECRET_TOKEN}"
```

**LINE (LINE Developers Console):**
```
Webhook URL: https://{api-id}.execute-api.us-east-1.amazonaws.com/prod/webhook/line
```

#### 7. OpenAB Environment Variables

> **Security: webhook signature validation**
> Exposing `:8080` to the public internet means anyone who discovers your webhook
> URL can POST forged events. Both platforms ship built-in defenses, but they
> require different config:
>
> - **LINE**: OpenAB automatically verifies the `X-Line-Signature` header using
>   `LINE_CHANNEL_SECRET` (HMAC-SHA256). No extra config needed — just set the
>   env var.
> - **Telegram**: Set `TELEGRAM_SECRET_TOKEN` (1–256 chars, alphanumeric + `-_`).
>   Telegram sends it back as `X-Telegram-Bot-Api-Secret-Token` on every webhook;
>   OpenAB rejects requests whose header doesn't match. The same value **must**
>   be passed as `secret_token` to the `setWebhook` call above.

```bash
# Telegram (secret_token must match the setWebhook call in §6)
TELEGRAM_BOT_TOKEN=123:abc
TELEGRAM_SECRET_TOKEN="$(openssl rand -hex 32)"

# LINE (LINE_CHANNEL_SECRET is used for automatic webhook signature verification)
LINE_CHANNEL_SECRET=xxx
LINE_CHANNEL_ACCESS_TOKEN=yyy

# Discord (unchanged)
DISCORD_BOT_TOKEN=zzz
```

With unified webhook mode (v0.9.0-beta.4+), the OpenAB binary auto-detects which
env vars are set and starts the corresponding platform adapters. No `[gateway]`
config block needed for Telegram/LINE.

### Cost Breakdown

| Resource | Monthly Cost |
|----------|-------------|
| API Gateway HTTP API | ~$1.00/million requests (effectively $0 for small bots) |
| VPC Link | ~$0.01/hr per ENI (~$7/month for a single ENI) |
| Cloud Map | $0 (private DNS, minimal queries) |
| ECS Fargate (256/512, 12h/day) | ~$5–10/month |
| **Total** | **~$12–17/month** |

> VPC Link pricing is based on ENI-hour billing (approximately $0.01/hour per ENI).
> REST API VPC Links are more expensive. This makes API Gateway the cheapest
> AWS-native ingress option for low-traffic bots, though not as close to zero as
> Cloudflare Tunnel.

### Pitfalls We Hit

1. **SG inbound for VPC Link traffic**
   VPC Link traffic originates from the VPC Link's ENI in your subnet. A
   self-referencing SG rule on `:8080` usually covers it. If you see 503 from API
   Gateway, check your SG inbound rules first.

2. **VPC Link subnets should be private (recommended)**
   VPC Link ENIs don't need public IPs, so private subnets are preferred.
   Public subnets also work, but your VPC Link will still use private ENIs internally. If your ECS task uses public subnets
   with `assignPublicIp=ENABLED`, that's fine — the VPC Link still uses private
   subnets for its ENIs.

3. **ECS service discovery is create-only**
   You cannot add `--service-registries` to an existing ECS service via
   `update-service`. If your service already exists, delete it (safe when
   `desiredCount=0`) and recreate with the registry ARN.

4. **LINE webhook verification**
   LINE sends an initial verification request when you set the webhook URL. OpenAB
   handles signature verification automatically — just make sure the container has the
   correct `LINE_CHANNEL_SECRET` env var.

### Pros & Cons

| Pros | Cons |
|------|------|
| Cheapest AWS-native path (~$12–17/mo total) | VPC Link adds an extra network hop |
| Webhook URL never changes (API Gateway endpoint is static) | One extra service to manage (API Gateway) |
| Same infra for Telegram + LINE + Discord | Initial setup has more moving parts |
| No ALB, no Kubernetes, no Cloudflare dependency | Debugging requires checking VPC Link logs |

---

## Option 2: ALB + ECS Fargate

> **Disclaimer**: The authors evaluated this option but did not deploy it in production.
> The comparison below is based on AWS public pricing and architecture best practices.

### High-Level Architecture

```
Telegram / LINE ──HTTPS──▶ ALB (public) ──HTTP──▶ ECS Fargate :8080
                                │
                          Target Group
                          health check: /health
```

### Cost

| Resource | Monthly |
|----------|---------|
| ALB (fixed) | ~$16.20 |
| ALB LCU (low traffic) | ~$2–5 |
| **Total** | **~$18–21/month** |

### When to Choose This

- You need native health checks and auto-scaling
- You run multiple bots behind a single ALB with path-based routing
- Enterprise compliance requires AWS WAF or Shield integration
- You already have an ALB in the account

### Why We Didn't Choose It

For a 1–4 bot personal deployment, the ALB's fixed cost (~$16/month) alone is **triple**
the total cost of the API Gateway + VPC Link + Cloud Map path. The feature gap (health
checks, auto-scaling) doesn't justify the cost increase at this scale.

---

## Option 3: Cloudflare Tunnel Sidecar

We use this in production for our LINE bots. If you already use
Cloudflare, this is the simplest path.

> **Cross-reference**: For a Kubernetes-based Cloudflare Tunnel setup, see the existing
> [`docs/refarch/telegram-cloudflare-tunnel.md`][cf-k3s]. This section covers the
> **ECS Fargate specific** approach.

[cf-k3s]: ../telegram-cloudflare-tunnel.md

### Cost Breakdown

| Resource | Monthly Cost |
|----------|-------------|
| Cloudflare Tunnel (cloudflared sidecar) | $0 (free tunnel, no ingress charges) |
| ECS Fargate (256/512, 12h/day) | ~$5–10/month |
| **Total** | **~$5–10/month** |

> Cheaper than Option 1 (~$12–17) — the difference is Cloudflare (free tunnel) vs
> AWS VPC Link (~$7/month) for the ingress layer. Choose based on which ecosystem
> you already use.

### Architecture (ECS Fargate)

```
Telegram / LINE ──HTTPS──▶ Cloudflare Edge
                              │
                         Tunnel (cloudflared)
                              │
                     ┌────────┴────────┐
                     │  ECS Fargate    │
                     │  sidecar:       │
                     │  cloudflared    │
                     │  → localhost:   │
                     │    8080         │
                     │                 │
                     │  main: OpenAB   │
                     │  :8080          │
                     └─────────────────┘
```

### Key Differences from K3s Version

| Aspect | K3s (existing doc) | ECS Fargate (this guide) |
|--------|-------------------|-------------------------|
| Tunnel sidecar | Separate Pod or external | Sidecar container in same task-def |
| Network | Pod-to-Pod (localhost) | Container-to-container (localhost) |
| Tunnel config | ConfigMap / env var | ECS Secrets Manager or env var |
| QUIC vs HTTP2 | QUIC (default) | **Use `--protocol http2`** (QUIC unstable on 256 CPU) |

### Step-by-Step (ECS Fargate)

#### 1. Create a Cloudflare Tunnel

In Cloudflare Zero Trust dashboard: create a tunnel, note the tunnel token.

#### 2. Store Token in AWS Secrets Manager

```bash
aws secretsmanager create-secret \
  --name openab/cloudflare-tunnel-token \
  --secret-string '{"token":"your-tunnel-token"}' \
  --region us-east-1
```

#### 3. Add cloudflared Sidecar to Task Definition

```json
{
  "containerDefinitions": [
    {
      "name": "cloudflared",
      "image": "cloudflare/cloudflared:latest",
      "command": ["tunnel", "--no-autoupdate", "run", "--protocol", "http2"],
      "secrets": [
        {
          "name": "TUNNEL_TOKEN",
          "valueFrom": "arn:aws:secretsmanager:us-east-1:123456:secret:openab/cloudflare-tunnel-token:token::"
        }
      ],
      "essential": true
    },
    {
      "name": "openab",
      "image": "ghcr.io/openabdev/openab:0.9.0-beta.6-opencode",
      "portMappings": [{"containerPort": 8080}],
      "secrets": [
        {"name": "TELEGRAM_BOT_TOKEN", "valueFrom": "arn:aws:secretsmanager:us-east-1:123456:secret:openab/telegram-bot-token:token::"},
        {"name": "TELEGRAM_SECRET_TOKEN", "valueFrom": "arn:aws:secretsmanager:us-east-1:123456:secret:openab/telegram-bot-token:TELEGRAM_SECRET_TOKEN::"},
        {"name": "LINE_CHANNEL_SECRET", "valueFrom": "arn:aws:secretsmanager:us-east-1:123456:secret:openab/line-shared:LINE_CHANNEL_SECRET::"},
        {"name": "LINE_CHANNEL_ACCESS_TOKEN", "valueFrom": "arn:aws:secretsmanager:us-east-1:123456:secret:openab/line-shared:LINE_CHANNEL_ACCESS_TOKEN::"}
      ],
      "essential": true
    }
  ]
}
```

#### 4. Set Public Hostname in Cloudflare Dashboard

Point your tunnel's public hostname (e.g., `bot.example.com`) to `localhost:8080`.

#### 5. Set Webhook URLs

```
Telegram: https://bot.example.com/webhook/telegram
LINE:     https://bot.example.com/webhook/line
```

> **Network requirement**: The cloudflared sidecar must reach the internet (it connects
> **outbound** to Cloudflare's edge). Use `assignPublicIp=ENABLED` (public subnet) or a
> NAT Gateway (private subnet). Without internet egress, the tunnel will not establish.

### Pitfalls (ECS-Specific)

1. **QUIC on low CPU is unstable**
   On 256 CPU Fargate, cloudflared's default QUIC protocol causes intermittent tunnel
   disconnections. Always add `--protocol http2`.

2. **Tunnel token rotation**
   If you regenerate the tunnel token in Cloudflare, update the Secrets Manager secret.
   You must force a new ECS deployment (stop existing task) for the sidecar to pick up
   the new token.

3. **Sidecar essential = true**
   If cloudflared crashes, the entire task stops. This is intentional — a bot with no
   ingress is useless. Set `essential: true` on both containers.

### Pros & Cons

| Pros | Cons |
|------|------|
| Simplest setup (just add a sidecar) | Requires Cloudflare account |
| Free tunnel (no AWS ingress costs) | QUIC instability on low CPU |
| Cloudflare DDoS protection included | Tunnel token must be in Secrets Manager |
| Same pattern works for any platform | Extra ~50MB RAM for cloudflared |

---

## Multi-Platform: Telegram + LINE + Discord on One Task

OpenAB's unified binary can handle all three platforms simultaneously from a single
ECS task:

```bash
# Environment variables — set all three
TELEGRAM_BOT_TOKEN=123:abc
LINE_CHANNEL_SECRET=xxx
LINE_CHANNEL_ACCESS_TOKEN=yyy
DISCORD_BOT_TOKEN=zzz
```

The binary auto-detects which env vars are present and starts each platform adapter:

| Env Var Present | Adapter | Webhook Path |
|----------------|---------|-------------|
| `DISCORD_BOT_TOKEN` | Discord | (outbound WS, no path) |
| `TELEGRAM_BOT_TOKEN` | Telegram | `/webhook/telegram` |
| `TELEGRAM_SECRET_TOKEN` | (Telegram only) | **Recommended.** Verifies `X-Telegram-Bot-Api-Secret-Token` on inbound webhooks; must match the `secret_token` passed to `setWebhook`. |
| `LINE_CHANNEL_SECRET` + `LINE_CHANNEL_ACCESS_TOKEN` | LINE | `/webhook/line` |

**API Gateway routing**: Add one route per platform, all pointing to the same VPC Link:

```
POST /webhook/telegram  ─┐
POST /webhook/line      ─┼── same VPC Link → same Cloud Map → same task :8080
```

**Cost efficiency**: One task replaces three. The API Gateway + VPC Link is shared
across all platforms. Total: **~$12–17/month for Telegram + LINE + Discord on a single
Fargate task.**

---

## Summary

| Factor | Option 1 (API GW + CM) | Option 2 (ALB) | Option 3 (CF Tunnel) |
|--------|----------------------|----------------|---------------------|
| Monthly cost | ~$12–17 | ~$18–21 | ~$5–10 |
| Setup complexity | Medium | Low | Low |
| AWS-native | ✅ Yes | ✅ Yes | ❌ No (CF dependency) |
| Health checks | Manual | ✅ Built-in | Manual |
| Multi-platform | ✅ Single VPC Link | ✅ Single ALB | ✅ Single tunnel |
| Best for | **Budget + AWS-native** | Enterprise | **Cloudflare users** |

---
