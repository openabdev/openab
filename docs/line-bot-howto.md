# LINE Bot Setup Guide

Step-by-step guide to create and configure a LINE Messaging API bot for OpenAB.

## Architecture Overview

LINE requires a webhook URL with HTTPS (TLS 1.2+) and a certificate from a public CA. The recommended architecture uses CloudFront for TLS termination in front of an ALB:

```
LINE Platform → CloudFront (HTTPS/TLS) → ALB (HTTP) → K8s Service → Pod :8080
```

Unlike Discord (WebSocket gateway) and Slack (Socket Mode), LINE uses a **push model** — LINE sends HTTP POST requests to your webhook URL whenever a user sends a message.

OpenAB's LINE adapter uses **push messages** (not reply messages) because agent processing can exceed LINE's 1-minute reply token expiry.

---

## 1. Create a LINE Messaging API Channel

1. Go to the [LINE Developers Console](https://developers.line.biz/console/)
2. Log in with your LINE account (or create one)
3. Click **Create a new provider** (or select an existing one)
   - Provider name: e.g. `OpenAB`
4. Click **Create a Messaging API channel**
5. Fill in the required fields:
   - Channel name: e.g. `OpenAB Agent`
   - Channel description: e.g. `AI coding agent powered by OpenAB`
   - Category / Subcategory: pick the closest match
6. Click **Create**

## 2. Get the Channel Secret

1. In the [LINE Developers Console](https://developers.line.biz/console/), select your channel
2. Go to the **Basic settings** tab
3. Find **Channel secret** — click to reveal and copy it
4. Save it securely — you'll need this for webhook signature validation

```bash
# Example: save to a file
echo -n "YOUR_CHANNEL_SECRET" > ~/.kiro/line-channel-secret.key
```

## 3. Issue a Channel Access Token

1. In the LINE Developers Console, go to the **Messaging API** tab
2. Scroll to **Channel access token (long-lived)**
3. Click **Issue** to generate a new token
4. Copy the token — this is your `LINE_CHANNEL_ACCESS_TOKEN`

```bash
# Example: save to a file
echo -n "YOUR_CHANNEL_ACCESS_TOKEN" > ~/.kiro/line-channel-access-token.key
```

> **Note:** Long-lived tokens don't expire, but you can revoke and reissue them at any time from this page.

## 4. Get the Channel ID

1. On the **Basic settings** tab, find **Channel ID** near the top
2. Copy it — you may need this for reference, though OpenAB doesn't require it in config

## 5. Configure the Webhook URL

1. Go to the **Messaging API** tab
2. Under **Webhook settings**, click **Edit** next to Webhook URL
3. Enter your webhook URL:
   ```
   https://your-cloudfront-domain.cloudfront.net
   ```
4. Click **Update**
5. Toggle **Use webhook** to ON
6. (Optional) Click **Verify** to test connectivity — your endpoint must return HTTP 200

> **Tip:** If your backend isn't ready yet, you can set up a temporary CloudFront Function that returns 200 for POST requests to pass LINE's verification. Replace it with the real origin later.

## 6. Disable Auto-Reply Messages

By default, LINE channels have auto-reply and greeting messages enabled. These interfere with OpenAB:

1. On the **Messaging API** tab, find **LINE Official Account features**
2. Click **Edit** next to Auto-reply messages (opens LINE Official Account Manager)
3. Disable **Auto-reply messages**
4. Disable **Greeting messages**

---

## Configuration Reference

```toml
[line]
channel_access_token = "${LINE_CHANNEL_ACCESS_TOKEN}"
channel_secret = "${LINE_CHANNEL_SECRET}"
webhook_port = 8080
allowed_users = []       # LINE user IDs (empty = allow all)
allowed_groups = []      # LINE group/room IDs (empty = allow all)
```

Set the environment variables:

```bash
export LINE_CHANNEL_ACCESS_TOKEN="eCDLjq..."
export LINE_CHANNEL_SECRET="0f3650f5..."
```

### `allowed_users` / `allowed_groups`

| `allowed_users` | `allowed_groups` | Result |
|---|---|---|
| empty | empty | All users, all groups (default) |
| set | empty | Only these users, all groups |
| empty | set | All users, only these groups |
| set | set | **AND** in groups — must be in allowed group AND allowed user |

- In 1:1 chats, only `allowed_users` is checked
- In group/room chats, both `allowed_groups` and `allowed_users` are checked
- LINE user IDs start with `U` (e.g. `Uab1234...`), group IDs with `C`, room IDs with `R`

### `webhook_port`

The port the built-in HTTP server listens on inside the container. Default: `8080`. This is the port your K8s Service should target.

---

## Deployment

### Infrastructure Setup

The LINE adapter requires a publicly reachable HTTPS endpoint. The recommended setup:

1. **EKS cluster** with the AWS Load Balancer Controller installed
2. **ALB** (Application Load Balancer) created via K8s Ingress — internet-facing, HTTP listener on port 80
3. **CloudFront** distribution in front of the ALB — provides HTTPS with a public CA certificate

> **Why ALB, not NLB?** LINE sends standard HTTPS POST requests with JSON bodies. ALB is designed for HTTP traffic and integrates natively with the AWS Load Balancer Controller's Ingress resource. NLB is for TCP/UDP passthrough and would require additional TLS configuration.

> **Why CloudFront?** LINE requires HTTPS with a certificate from a public CA. CloudFront provides this automatically with its default `*.cloudfront.net` certificate, avoiding the need to provision your own domain and ACM certificate.

### EKS Prerequisites

Before deploying, ensure your cluster has:

```bash
# 1. EBS CSI driver (for PersistentVolumeClaims)
eksctl create addon --name aws-ebs-csi-driver --cluster YOUR_CLUSTER

# 2. OIDC provider (for IAM Roles for Service Accounts)
eksctl utils associate-iam-oidc-provider --cluster YOUR_CLUSTER --approve

# 3. AWS Load Balancer Controller
#    Create IAM service account, then install via Helm:
helm repo add eks https://aws.github.io/eks-charts
helm install aws-load-balancer-controller eks/aws-load-balancer-controller \
  -n kube-system \
  --set clusterName=YOUR_CLUSTER \
  --set serviceAccount.create=false \
  --set serviceAccount.name=aws-load-balancer-controller
```

### K8s Service and Ingress

Create a Service and ALB Ingress for the webhook:

```yaml
apiVersion: v1
kind: Service
metadata:
  name: openab-line-webhook
spec:
  type: ClusterIP
  ports:
    - port: 8080
      targetPort: 8080
      protocol: TCP
  selector:
    app.kubernetes.io/name: openab
    app.kubernetes.io/instance: openab-line
    app.kubernetes.io/component: kiro
---
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: openab-line-ingress
  annotations:
    alb.ingress.kubernetes.io/scheme: internet-facing
    alb.ingress.kubernetes.io/target-type: ip
    alb.ingress.kubernetes.io/listen-ports: '[{"HTTP": 80}]'
spec:
  ingressClassName: alb
  rules:
    - http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: openab-line-webhook
                port:
                  number: 8080
```

### CloudFront Configuration

Create a CloudFront distribution with:

- **Origin:** ALB DNS name (e.g. `k8s-default-openabli-xxx.us-east-1.elb.amazonaws.com`)
- **Origin protocol policy:** HTTP only (CloudFront → ALB is internal)
- **Viewer protocol policy:** HTTPS only (LINE → CloudFront must be HTTPS)
- **Allowed HTTP methods:** GET, HEAD, OPTIONS, PUT, POST, PATCH, DELETE
- **Cache policy:** CachingDisabled (`4135ea2d-6df8-44a3-9df3-4b5a84be39ad`)
- **Origin request policy:** AllViewerExceptHostHeader (`b689b0a8-53d0-40ab-baf2-68738e2966ac`)
- **Origin read timeout:** 60 seconds (agent processing can take time)

### Helm Install

```bash
# Create K8s secret for agent credentials
kubectl create secret generic openab-line-kiro-keys \
  --from-literal="KIRO_API_KEY=$(cat ~/.kiro/my-kiro-worker1.key)" \
  --from-literal="GH_TOKEN=$(cat ~/.kiro/github-pat.key)"

# Deploy with Helm
helm upgrade --install openab-line ./charts/openab \
  --set image.repository="YOUR_ECR_URI" \
  --set image.tag="line-latest" \
  --set image.pullPolicy="Always" \
  --set agents.kiro.discord.enabled=false \
  --set agents.kiro.line.enabled=true \
  --set agents.kiro.line.channelAccessToken="$(cat ~/.kiro/line-channel-access-token.key)" \
  --set agents.kiro.line.channelSecret="$(cat ~/.kiro/line-channel-secret.key)" \
  --set agents.kiro.line.webhookPort=8080 \
  --set agents.kiro.persistence.storageClass=gp2 \
  --set 'agents.kiro.envFrom[0].secretRef.name=openab-line-kiro-keys'
```

> **⚠️ envFrom secret key naming:** When using `envFrom.secretRef`, Kubernetes
> injects secret keys directly as environment variable names. Keys must use
> underscores (e.g. `KIRO_API_KEY`), not dashes (`kiro-api-key`), because:
> 1. Dashed names aren't valid env vars and can't be referenced by `${...}` expansion
> 2. openab's config uses `${KIRO_API_KEY}` expansion — the env var name must match exactly
>
> If you need a different env var name than the secret key (e.g. `KIRO_API_KEY_OVERRIDE`
> from `KIRO_API_KEY`), set it via `agents.kiro.env`:
> ```
> --set 'agents.kiro.env.KIRO_API_KEY_OVERRIDE=${KIRO_API_KEY}'
> ```

### Authenticate the Agent

After the pod is running, authenticate kiro-cli:

```bash
kubectl exec -it deployment/openab-line-kiro -- kiro-cli login --use-device-flow
kubectl rollout restart deployment/openab-line-kiro
```

---

## Message Behavior

LINE's messaging model differs from Discord and Slack:

| Feature | Discord | Slack | LINE |
|---|---|---|---|
| Connection type | WebSocket gateway | Socket Mode (WebSocket) | Webhook (HTTP POST) |
| Threading | Discord threads | Slack threads | No threads — same chat |
| @mention required | Yes (configurable) | Yes (configurable) | No — all messages are delivered |
| Reply mechanism | Channel message | Thread reply | Push message |
| Message limit | 2000 chars | ~40,000 chars | 5000 chars |
| Reactions | ✅ Emoji reactions | ✅ Emoji reactions | ❌ Not supported |
| Bot-to-bot | Configurable | Configurable | N/A (no bot messages in LINE) |

### Key differences

- **No threads:** LINE doesn't have threads. All messages in a 1:1 chat or group go to the same conversation. OpenAB maps each LINE chat (user/group/room) to a single agent session.
- **No @mention needed:** In 1:1 chats, every message is delivered to the bot. In groups, the bot receives all messages (no @mention filtering).
- **Push messages:** OpenAB uses push messages instead of reply messages. Reply tokens expire after 1 minute, which isn't enough for agent processing. Push messages have no time limit.
- **No reactions:** LINE doesn't support adding emoji reactions to messages, so the reactions feature is a no-op for LINE.

### Source types

LINE messages come from three source types:

| Source | `channel_id` used | Allowlist checked |
|---|---|---|
| User (1:1 chat) | User ID (`U...`) | `allowed_users` |
| Group | Group ID (`C...`) | `allowed_groups` + `allowed_users` |
| Room | Room ID (`R...`) | `allowed_groups` + `allowed_users` |

---

## Security

### Webhook Signature Validation

Every webhook request from LINE includes an `x-line-signature` header. OpenAB validates this signature using HMAC-SHA256 with your channel secret. Requests with invalid signatures are silently dropped (after returning HTTP 200 to LINE).

This is the primary security mechanism — LINE's documentation explicitly recommends **not** restricting by source IP, as LINE's IP ranges can change without notice.

### Token Security

- **Channel secret:** Used for webhook signature validation. Keep it secret.
- **Channel access token:** Used to call LINE APIs (send messages, get profiles). Keep it secret.
- If either is compromised, reissue from the LINE Developers Console immediately.

---

## Finding User and Group IDs

LINE doesn't expose user/group IDs in the UI like Discord or Slack. To find them:

1. **User IDs:** Check the pod logs when a user sends a message:
   ```
   kubectl logs deployment/openab-line-kiro | grep "sender_id"
   ```
   User IDs look like `Uab4574bee71f04bb38df4732c514a0a1f`

2. **Group IDs:** Add the bot to a group and send a message. Check logs for `groupId`:
   ```
   kubectl logs deployment/openab-line-kiro | grep "groupId"
   ```
   Group IDs look like `C1234567890abcdef...`

3. **Room IDs:** Similar to group IDs but start with `R`

---

## Troubleshooting

### Bot doesn't respond

1. **Check webhook URL** — verify it's set and "Use webhook" is ON in LINE Developers Console
2. **Check pod logs** — look for `LINE webhook server listening port=8080`
3. **Check signature warnings** — `invalid LINE webhook signature` means the request body or signature doesn't match. Verify your channel secret is correct
4. **Check agent credentials** — `KIRO_API_KEY` and `GH_TOKEN` must be set as env vars in the pod
5. **Check ALB health** — `kubectl get ingress` should show an ALB address
6. **Check CloudFront origin** — must point to the ALB, not a placeholder

### "Connection Lost" / "connection closed"

The agent CLI (`kiro-cli`) failed to start or crashed immediately. Common causes:

1. **Missing `KIRO_API_KEY`** — check `kubectl exec deployment/openab-line-kiro -- env | grep KIRO`
2. **Agent not authenticated** — run `kiro-cli login --use-device-flow` inside the pod
3. **Agent binary missing** — check `kubectl exec deployment/openab-line-kiro -- which kiro-cli`

### "invalid LINE webhook signature" flooding logs

ALB health checks send GET requests without a signature, which trigger this warning. This is cosmetic — real LINE messages with valid signatures still get processed. To reduce noise, you can change the health check path to a dedicated endpoint or increase the health check interval.

### Webhook verification fails in LINE Console

1. Ensure CloudFront is deployed and the origin points to the ALB
2. Ensure the ALB target group is healthy: `kubectl get pods` should show Running
3. Test directly: `curl -X POST https://your-cloudfront-domain.cloudfront.net/`
   - Should return HTTP 200

### PVC stuck in Pending

The EBS CSI driver may not be installed:

```bash
eksctl create addon --name aws-ebs-csi-driver --cluster YOUR_CLUSTER
```

### Pod can't pull image from ECR

Ensure the node IAM role has `AmazonEC2ContainerRegistryReadOnly` policy, or create an ECR pull secret.

---

## Production Scaling

The default deployment runs a single pod with an in-memory session pool. This section covers how the system behaves under load and options for scaling to production.

### How the session pool works

Each unique LINE chat (user or group) gets its own `kiro-cli` process, managed by the session pool:

| Config | Default | Description |
|---|---|---|
| `pool.max_sessions` | 10 | Max concurrent kiro-cli processes |
| `pool.session_ttl_hours` | 24 | Idle sessions are reaped after this |

- **Within the pool limit:** each user gets a dedicated process, fully concurrent.
- **Pool exhausted:** the oldest idle session is **suspended** (session ID saved), and the new user gets a fresh process. The evicted user's session can be resumed later via `session/load`.
- **Same user sends while busy:** messages queue internally — the existing connection is reused.

### Single-pod limits

The default architecture is:

```
LINE Platform → CloudFront → ALB → 1 Pod (openab + N kiro-cli processes)
```

Bottlenecks:
- One pod handles all webhook traffic
- Each kiro-cli process consumes memory — a small node may OOM at high session counts
- No horizontal pod autoscaling built-in

### Scaling options

#### Vertical scaling (low effort)

Increase `max_sessions` and use a larger node instance type. Suitable for moderate traffic (~50-100 concurrent users).

```toml
[pool]
max_sessions = 50
```

#### Queue-based decoupling (production-grade)

For hundreds to thousands of concurrent users, decouple the webhook handler from agent processing:

```
LINE → ALB → webhook handler (lightweight, horizontally scalable)
                 ↓
              SQS / EventBridge
                 ↓
         worker pods (kiro-cli, autoscaled via KEDA/HPA)
                 ↓
         reply via LINE Push API
```

**Why this works well with LINE:**

LINE's Push API is stateless — any worker can send a reply to any user at any time. Unlike Discord (WebSocket) or Slack (Socket Mode), there is no persistent connection to maintain. The `userId` from the webhook payload serves as both the session lookup key and the reply delivery target:

```json
{
  "events": [{
    "source": { "type": "user", "userId": "U1234abc" },
    "message": { "type": "text", "text": "explain VPC peering" }
  }]
}
```

The flow:
1. Webhook handler validates the signature, enqueues `{ userId, text, sessionId }`, returns 200 immediately
2. Worker picks up the message, looks up or creates a kiro-cli session keyed by `userId`
3. Worker processes the request and replies via `POST https://api.line.me/v2/bot/message/push` with `"to": "U1234abc"`

**Required infrastructure:**
- SQS or EventBridge for message queue
- Redis or DynamoDB for shared session state (keyed by `userId`)
- KEDA or HPA to autoscale workers based on queue depth

#### Comparison

| Approach | Effort | Concurrent users | Notes |
|---|---|---|---|
| Vertical (bigger node) | Low | ~50-100 | Increase `max_sessions`, use larger instance |
| Queue + external session store | High | ~thousands | Requires rearchitecting the pool layer |
