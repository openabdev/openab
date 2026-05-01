# Microsoft Teams Enterprise Deployment

Deploy OpenAB with MS Teams in an enterprise Kubernetes environment using Helm. This guide covers Azure Entra ID configuration, Azure Bot Service setup, Teams app packaging, and Helm-based deployment.

```
Teams Client → Bot Framework → K8s Ingress (HTTPS + TLS) → Gateway pod → OAB pod
                                     ↑
                        Company's existing infrastructure
```

## Prerequisites

- An Azure subscription with permissions to create resources
- A Microsoft 365 tenant with Teams enabled
- A Kubernetes cluster with an Ingress controller and TLS (e.g. AKS, EKS, GKE, on-prem)
- `kubectl` and `helm` CLI tools
- IT admin access to Teams Admin Center (for app approval)

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│  Your Kubernetes Cluster                                    │
│                                                             │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐  │
│  │   Ingress    │───▶│   Gateway    │◀──▶│     OAB      │  │
│  │  (HTTPS/TLS) │    │     Pod      │ WS │     Pod      │  │
│  └──────┬───────┘    └──────────────┘    └──────────────┘  │
│         │                                                   │
└─────────┼───────────────────────────────────────────────────┘
          │ HTTPS
┌─────────┴───────────┐
│  Bot Framework      │
│  (Microsoft Cloud)  │
└─────────────────────┘
```

- **Ingress** terminates TLS and routes `/webhook/teams` to the Gateway pod
- **Gateway pod** validates JWT, normalizes events, routes replies via Bot Framework REST API
- **OAB pod** connects outbound to Gateway via WebSocket — no inbound ports needed

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

> **Note**: Multi-tenant bot creation was deprecated by Microsoft on July 31, 2025. Single Tenant is the recommended path. Cross-tenant access is achieved via Teams App Store publishing.

### Configure the Messaging Endpoint

1. Go to the Bot resource → **Configuration**
2. Set **Messaging endpoint** to your Kubernetes Ingress URL:
   ```
   https://<YOUR_INGRESS_HOST>/webhook/teams
   ```

### Enable the Teams Channel

1. Go to **Channels** → click **Microsoft Teams**
2. Accept the terms of service → **Save**

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

## Step 4: Deploy with Helm

### Install the Gateway + OAB

```bash
helm install openab oci://ghcr.io/openabdev/charts/openab \
  --set agents.kiro.gateway.enabled=true \
  --set agents.kiro.gateway.url="ws://openab-gateway:8080/ws" \
  --set agents.kiro.gateway.platform="teams" \
  --set agents.kiro.gateway.teams.appId="<YOUR_TEAMS_APP_ID>" \
  --set-literal agents.kiro.gateway.teams.appSecret="<YOUR_TEAMS_APP_SECRET>" \
  --set agents.kiro.gateway.teams.oauthEndpoint="https://login.microsoftonline.com/<YOUR_TENANT_ID>/oauth2/v2.0/token" \
  --set-string agents.kiro.gateway.teams.allowedTenants[0]="<YOUR_TENANT_ID>"
```

> **Single Tenant bots must set `oauthEndpoint`** to the tenant-specific endpoint. The default (`botframework.com`) only works for Multi Tenant bots and will cause `401 Unauthorized` errors.

> **Use `--set-literal` for `appSecret`** — the secret may contain `.` characters that Helm interprets as nested key separators.

### Helm Values Reference

```yaml
agents:
  kiro:
    gateway:
      enabled: true
      url: "ws://openab-gateway:8080/ws"
      platform: "teams"
      teams:
        appId: ""                    # Azure Entra ID application (client) ID
        appSecret: ""                # Azure Entra ID client secret
        oauthEndpoint: ""            # Required for Single Tenant
        openidMetadata: ""           # Override for sovereign clouds
        allowedTenants: []           # Restrict to specific tenants
        webhookPath: "/webhook/teams"
```

### Ingress Configuration

Route Bot Framework webhooks to the Gateway pod using your existing Ingress controller:

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

## Step 5: IT Admin — Approve the Teams App

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

### Verify

After policy propagation (may take up to 24 hours):

1. Users go to **Apps** → **Built for your org** → find OpenAB → **Add**
2. For personal chat: open the app and start chatting
3. For channels: add the app to a team → use `@OpenAB` to mention the bot

## Tenant Allowlist

Restrict which Azure AD tenants can interact with the bot:

```bash
--set-string agents.kiro.gateway.teams.allowedTenants[0]="<TENANT_ID_1>" \
--set-string agents.kiro.gateway.teams.allowedTenants[1]="<TENANT_ID_2>"
```

If not set, all tenants are allowed.

## Sovereign Cloud Configuration

For Azure Government or Azure China deployments:

| Cloud | `oauthEndpoint` | `openidMetadata` |
|---|---|---|
| Public (default) | `login.microsoftonline.com/<TENANT>/...` | `login.botframework.com/...` |
| Azure Government | `login.microsoftonline.us/<TENANT>/...` | `login.botframework.azure.us/...` |
| Azure China (21Vianet) | `login.partner.microsoftonline.cn/<TENANT>/...` | `login.botframework.azure.cn/...` |

```bash
# Azure Government example
--set agents.kiro.gateway.teams.oauthEndpoint="https://login.microsoftonline.us/<TENANT_ID>/oauth2/v2.0/token" \
--set agents.kiro.gateway.teams.openidMetadata="https://login.botframework.azure.us/v1/.well-known/openidconfiguration"
```

## Environment Variables Reference

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

**Fix**: Verify `oauthEndpoint` is set to `https://login.microsoftonline.com/<YOUR_TENANT_ID>/oauth2/v2.0/token`

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

- **Credentials in Kubernetes Secrets** — Helm chart stores `TEAMS_APP_SECRET` in a K8s Secret, not in ConfigMap
- **Rotate client secrets** before expiration — set a reminder based on the expiration chosen in Step 1
- **Use tenant allowlist** in production — restrict to your organization's tenant ID
- **Network policies** — consider restricting Gateway pod egress to Bot Framework endpoints
- **OAB pod has no inbound exposure** — connects outbound to Gateway only

## References

- [Azure Bot Service documentation](https://learn.microsoft.com/en-us/azure/bot-service/)
- [Register a bot with Azure](https://learn.microsoft.com/en-us/azure/bot-service/bot-service-quickstart-registration)
- [Teams app permission policies](https://learn.microsoft.com/en-us/microsoftteams/teams-app-permission-policies)
- [Teams custom app policies](https://learn.microsoft.com/en-us/microsoftteams/teams-custom-app-policies-and-settings)
- [Bot Framework authentication](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication)
- [Teams app manifest schema](https://learn.microsoft.com/en-us/microsoftteams/platform/resources/schema/manifest-schema)
