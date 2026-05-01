# Microsoft Teams Enterprise Deployment

Deploy OpenAB with MS Teams in an enterprise Kubernetes environment. This guide covers Azure Entra ID configuration, Azure Bot Service setup, Teams app packaging, and Kubernetes deployment.

```
Teams Client → Bot Framework → K8s Ingress (HTTPS + TLS) → Gateway pod → OAB pod
                                     ↑
                        Company's existing infrastructure
```

## Prerequisites

- An Azure subscription with permissions to create resources
- A Microsoft 365 tenant with Teams enabled (Commercial Cloud Trial works for testing)
- A Kubernetes cluster with an Ingress controller and TLS
- `kubectl` CLI
- IT admin access to Teams Admin Center (for app approval)

## Architecture Overview

The deployment consists of two components:

```
┌─────────────────────────────────────────────────────────────┐
│  Your Kubernetes Cluster                                    │
│                                                             │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐  │
│  │   Ingress    │───▶│   Gateway    │◀──▶│     OAB      │  │
│  │  (HTTPS/TLS) │    │  (BYO deploy)│ WS │  (Helm chart)│  │
│  └──────┬───────┘    └──────────────┘    └──────────────┘  │
│         │                                                   │
└─────────┼───────────────────────────────────────────────────┘
          │ HTTPS
┌─────────┴───────────┐
│  Bot Framework      │
│  (Microsoft Cloud)  │
└─────────────────────┘
```

| Component | Deployed by | Description |
|---|---|---|
| **Gateway** | You (K8s Deployment or Docker) | Receives Bot Framework webhooks, validates JWT, routes replies. Reads `TEAMS_*` env vars. |
| **OAB** | Helm chart (`openab`) | Connects outbound to Gateway via WebSocket. No inbound ports needed. |

## Step 1: Register an Azure Entra ID Application

1. Go to [Azure Portal → Microsoft Entra ID → App registrations](https://portal.azure.com/#blade/Microsoft_AAD_RegisteredApps/ApplicationsListBlade)
2. Click **New registration**
3. Configure:
   - **Name**: `openab-teams-bot` (or your preferred name)
   - **Supported account types**: **Single tenant** (Accounts in this organizational directory only)
   - **Redirect URI**: leave empty
4. Click **Register**

After creation, note from the **Overview** page:

| Value | Used As |
|---|---|
| Application (client) ID | `TEAMS_APP_ID` |
| Directory (tenant) ID | `<YOUR_TENANT_ID>` in OAuth endpoint |

### Create a Client Secret

1. Go to **Certificates & secrets** → **Client secrets** → **New client secret**
2. Set a description and expiration (recommended: 12 or 24 months)
3. Click **Add**
4. **Copy the Value immediately** — it is only shown once → `TEAMS_APP_SECRET`

> **Security note**: Store the client secret in a Kubernetes Secret. Never commit it to source control. Set a calendar reminder to rotate before expiration.

> **Note**: Multi-tenant bot creation was deprecated by Microsoft on July 31, 2025. Single Tenant is the only supported path for new bots.

## Step 2: Create an Azure Bot Resource

1. Go to [Azure Portal → Create a resource](https://portal.azure.com/#create/hub) → search **Azure Bot** → **Create**
2. Configure:
   - **Bot handle**: a unique name (e.g. `openab-prod`)
   - **Subscription / Resource group**: your enterprise subscription
   - **Pricing tier**: F0 (free) for testing, S1 for production
   - **Type of App**: **Single Tenant**
   - **Creation type**: **Use existing app registration**
   - **App ID**: paste `TEAMS_APP_ID` from Step 1
   - **App tenant ID**: paste your Directory (tenant) ID
3. Click **Review + Create** → **Create**

### Configure the Messaging Endpoint

1. Go to the Bot resource → **Configuration**
2. Set **Messaging endpoint** to your Kubernetes Ingress URL:
   ```
   https://<YOUR_INGRESS_HOST>/webhook/teams
   ```

### Enable the Teams Channel

1. Go to **Channels** → click **Microsoft Teams**
2. Accept the terms of service → **Save**

> **Testing tip**: After enabling the Teams channel, use the **Open in Teams** link (Azure Bot → Channels → Teams) for quick testing without uploading an app package. This link only works for people who have it — it does not make the bot discoverable org-wide.

> **⚠️ Do not use "Test in Web Chat"** for outbound reply testing. Azure Portal's Web Chat uses `webchat.botframework.com` which returns 403 for Single Tenant bot replies. Only real Teams clients (`smba.trafficmanager.net`) work for outbound.

## Step 3: Build a Teams App Manifest

Create a directory with three files:

### `manifest.json`

```json
{
  "$schema": "https://developer.microsoft.com/en-us/json-schemas/teams/v1.25/MicrosoftTeams.schema.json",
  "manifestVersion": "1.25",
  "version": "1.0.0",
  "id": "<GENERATE_A_UUID_V4>",
  "developer": {
    "name": "<YOUR_ORGANIZATION_NAME>",
    "websiteUrl": "https://<YOUR_COMPANY_WEBSITE>",
    "privacyUrl": "https://<YOUR_COMPANY_WEBSITE>/privacy",
    "termsOfUseUrl": "https://<YOUR_COMPANY_WEBSITE>/terms"
  },
  "name": {
    "short": "OpenAB",
    "full": "OpenAB AI Assistant"
  },
  "description": {
    "short": "AI coding assistant powered by OpenAB",
    "full": "Connect to an AI coding assistant through Microsoft Teams."
  },
  "icons": {
    "outline": "outline.png",
    "color": "color.png"
  },
  "accentColor": "#ffffff",
  "bots": [
    {
      "botId": "<YOUR_TEAMS_APP_ID>",
      "scopes": ["personal", "team", "groupChat"],
      "isNotificationOnly": false,
      "supportsFiles": false
    }
  ],
  "validDomains": []
}
```

- `id` — Teams app ID (generate a fresh UUID v4, not the same as `botId`)
- `botId` — Azure Entra ID Application (client) ID from Step 1

### Icons

- `outline.png` — 32×32 transparent background, white icon
- `color.png` — 192×192 full-color icon

### Package

```bash
zip openab-teams-app.zip manifest.json outline.png color.png
```

## Step 4: Deploy the Gateway

The Gateway is deployed separately from the OAB Helm chart. Use a standard Kubernetes Deployment:

### Gateway Secret

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: openab-gateway-teams
type: Opaque
stringData:
  TEAMS_APP_ID: "<YOUR_APPLICATION_ID>"
  TEAMS_APP_SECRET: "<YOUR_CLIENT_SECRET>"
  TEAMS_OAUTH_ENDPOINT: "https://login.microsoftonline.com/<YOUR_TENANT_ID>/oauth2/v2.0/token"
```

> **⚠️ Single Tenant bots must set `TEAMS_OAUTH_ENDPOINT`** to the tenant-specific endpoint. The default (`botframework.com`) only works for Multi Tenant bots and will cause `401 Unauthorized` errors. This is the #1 setup pitfall.

### Gateway Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: openab-gateway
spec:
  replicas: 1
  selector:
    matchLabels:
      app: openab-gateway
  template:
    metadata:
      labels:
        app: openab-gateway
    spec:
      containers:
        - name: gateway
          image: ghcr.io/openabdev/openab-gateway:latest
          ports:
            - containerPort: 8080
          envFrom:
            - secretRef:
                name: openab-gateway-teams
          env:
            - name: RUST_LOG
              value: "info"
          livenessProbe:
            httpGet:
              path: /health
              port: 8080
---
apiVersion: v1
kind: Service
metadata:
  name: openab-gateway
spec:
  selector:
    app: openab-gateway
  ports:
    - port: 8080
      targetPort: 8080
```

### Ingress

Route Bot Framework webhooks to the Gateway using your existing Ingress controller:

```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: openab-gateway
  annotations:
    # Adjust for your Ingress controller (nginx, ALB, Traefik, etc.)
    nginx.ingress.kubernetes.io/ssl-redirect: "true"
spec:
  tls:
    - hosts:
        - <YOUR_INGRESS_HOST>
      secretName: <YOUR_TLS_SECRET>
  rules:
    - host: <YOUR_INGRESS_HOST>
      http:
        paths:
          - path: /webhook/teams
            pathType: Prefix
            backend:
              service:
                name: openab-gateway
                port:
                  number: 8080
```

> Bot Framework requires HTTPS. Your Ingress controller handles TLS termination — the Gateway pod listens on plain HTTP (:8080).

## Step 5: Deploy OAB with Helm

OAB connects outbound to the Gateway via WebSocket:

```bash
helm install openab oci://ghcr.io/openabdev/charts/openab \
  --set agents.kiro.gateway.enabled=true \
  --set agents.kiro.gateway.url="ws://openab-gateway:8080/ws" \
  --set agents.kiro.gateway.platform="teams"
```

The OAB pod does not need any Teams credentials — it only needs the Gateway WebSocket URL.

## Step 6: IT Admin — Approve the Teams App

Enterprise tenants typically restrict custom app installation. An IT admin must approve the app.

### Upload the App Package

1. Go to [Teams Admin Center](https://admin.teams.microsoft.com/) → **Teams apps** → **Manage apps**
2. Click **Upload new app** → select `openab-teams-app.zip`
3. The app appears with status **Blocked** (default for new custom apps)

### Configure Permission Policies

1. Go to **Teams apps** → **Permission policies**
2. Edit the **Global (Org-wide default)** policy or create a new one:
   - Under **Custom apps**, allow the OpenAB app
3. If using a custom policy, assign it to target users or groups

### Configure Setup Policies (Optional)

To pin the app for users automatically:

1. Go to **Teams apps** → **Setup policies**
2. Edit the relevant policy → **Installed apps** → **Add apps** → select OpenAB
3. Optionally add to **Pinned apps** for sidebar visibility

### Bot Discovery Methods

| Method | Who can find it | Best for |
|---|---|---|
| **Open in Teams link** | Only people with the link | Quick testing |
| **Teams Admin Center upload** | Everyone in the org | Enterprise deployment |
| **App Store publish** | Everyone worldwide | Commercial bots |

### Verify

After policy propagation (may take up to 24 hours):

1. Users go to **Apps** → **Built for your org** → find OpenAB → **Add**
2. For personal chat: open the app and start chatting
3. For channels: add the app to a team → use `@OpenAB` to mention the bot

## Tenant Allowlist

Restrict which Azure AD tenants can interact with the bot by adding to the Gateway Secret:

```yaml
stringData:
  TEAMS_ALLOWED_TENANTS: "<YOUR_TENANT_ID>"
```

Multiple tenants: `"<TENANT_ID_1>,<TENANT_ID_2>"`. If not set, all tenants are allowed.

## Sovereign Cloud Configuration

For Azure Government or Azure China deployments, add to the Gateway Secret:

| Cloud | `TEAMS_OAUTH_ENDPOINT` | `TEAMS_OPENID_METADATA` |
|---|---|---|
| Public (default) | `login.microsoftonline.com/<TENANT>/...` | `login.botframework.com/...` |
| Azure Government | `login.microsoftonline.us/<TENANT>/...` | `login.botframework.azure.us/...` |
| Azure China (21Vianet) | `login.partner.microsoftonline.cn/<TENANT>/...` | `login.botframework.azure.cn/...` |

## Environment Variables Reference (Gateway)

| Variable | Required | Default | Description |
|---|---|---|---|
| `TEAMS_APP_ID` | Yes | — | Azure Entra ID application (client) ID |
| `TEAMS_APP_SECRET` | Yes | — | Azure Entra ID client secret |
| `TEAMS_OAUTH_ENDPOINT` | Yes (Single Tenant) | `https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token` | Tenant-specific OAuth endpoint |
| `TEAMS_OPENID_METADATA` | No | `https://login.botframework.com/v1/.well-known/openidconfiguration` | OpenID metadata for JWT validation |
| `TEAMS_ALLOWED_TENANTS` | No | (allow all) | Comma-separated tenant IDs |
| `TEAMS_WEBHOOK_PATH` | No | `/webhook/teams` | Webhook endpoint path |

## Troubleshooting

### 401 Unauthorized when bot tries to reply

OAuth endpoint mismatch. Single Tenant bots must use the tenant-specific endpoint.

**Fix**: Verify `TEAMS_OAUTH_ENDPOINT` is set to `https://login.microsoftonline.com/<YOUR_TENANT_ID>/oauth2/v2.0/token`

### "Test in Web Chat" works but Teams doesn't reply

Web Chat uses Direct Line (`webchat.botframework.com`), which has different auth than Teams (`smba.trafficmanager.net`). Web Chat may accept inbound but reject outbound for Single Tenant bots.

**Fix**: Always test with a real Teams client. Do not rely on Web Chat for outbound reply testing.

### Bot doesn't appear in Teams

IT admin has not approved the custom app, or permission policy hasn't propagated.

**Fix**:
1. Verify the app is uploaded in Teams Admin Center → Manage apps
2. Check Permission policies allow the custom app
3. Wait up to 24 hours for policy propagation

### Gateway receives webhook but no reply in Teams

Check Gateway pod logs:
```bash
kubectl logs deployment/openab-gateway --tail=50
```

Look for: `teams → gateway` (received) → `gateway → teams` (sent) → `teams activity sent` (success) or `teams send error` (failure).

### JWT validation failed

The Gateway auto-refreshes JWKS on cache miss. If persistent, verify OpenID metadata is reachable:
```bash
kubectl exec deployment/openab-gateway -- curl -s https://login.botframework.com/v1/.well-known/openidconfiguration
```

## Security Considerations

- **Credentials in Kubernetes Secrets** — never in ConfigMaps or Deployment manifests
- **Rotate client secrets** before expiration — set a reminder based on the expiration chosen in Step 1
- **Use tenant allowlist** in production — restrict `TEAMS_ALLOWED_TENANTS` to your organization's tenant ID
- **Network policies** — consider restricting Gateway pod egress to Bot Framework endpoints
- **OAB pod has no inbound exposure** — connects outbound to Gateway only

## References

- [Azure Bot Service documentation](https://learn.microsoft.com/en-us/azure/bot-service/)
- [Register a bot with Azure](https://learn.microsoft.com/en-us/azure/bot-service/bot-service-quickstart-registration)
- [Teams app permission policies](https://learn.microsoft.com/en-us/microsoftteams/teams-app-permission-policies)
- [Teams custom app policies](https://learn.microsoft.com/en-us/microsoftteams/teams-custom-app-policies-and-settings)
- [Bot Framework authentication](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication)
- [Teams app manifest schema](https://learn.microsoft.com/en-us/microsoftteams/platform/resources/schema/manifest-schema)
