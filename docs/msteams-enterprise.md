# Microsoft Teams Enterprise Deployment

Deploy OpenAB with MS Teams in an enterprise Kubernetes environment. This guide covers Azure Entra ID configuration, Azure Bot Service setup, Teams app packaging, and Kubernetes deployment.

> **Unified Mode (available since v0.9.0-beta.4; validated with v0.9.0-beta.10):**
> The OAB binary embeds the Teams adapter, so no separate gateway container is
> needed. Complete the prerequisites and Steps 1–3, then follow
> [Step 4A](#step-4a-deploy-in-unified-mode-recommended). The stable `v0.9.0`
> release is not yet available; the commands below pin the validated beta.

## Prerequisites

- An Azure subscription with permissions to create resources
- A Microsoft 365 tenant with Teams enabled (Commercial Cloud Trial works for testing)
- A Kubernetes cluster with an Ingress controller
- A publicly resolvable DNS hostname and valid TLS certificate for the Ingress
- Helm 3 with OCI registry support
- `kubectl` CLI
- IT admin access to Teams Admin Center (for app approval)

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
      "supportsFiles": false,
      "commandLists": [
        {
          "scopes": ["personal"],
          "commands": [
            {"title": "/models", "description": "List the available AI models for the current session."},
            {"title": "/agents", "description": "List the available agent modes for the current session."},
            {"title": "/cancel", "description": "Cancel the current in-flight operation."},
            {"title": "/cancel-all", "description": "Cancel the operation and clear buffered messages in this conversation."},
            {"title": "/reset", "description": "Reset the conversation session and clear buffered messages."},
            {"title": "/usage", "description": "Show private backend account usage for the current session."}
          ]
        },
        {
          "scopes": ["team", "groupChat"],
          "commands": [
            {"title": "/models", "description": "List the available AI models for the current session."},
            {"title": "/agents", "description": "List the available agent modes for the current session."},
            {"title": "/cancel", "description": "Cancel the current in-flight operation."},
            {"title": "/cancel-all", "description": "Cancel the operation and clear buffered messages in this conversation."},
            {"title": "/reset", "description": "Reset the conversation session and clear buffered messages."}
          ]
        }
      ]
    }
  ],
  "validDomains": []
}
```

- `id` — Teams app ID (generate a fresh UUID v4, not the same as `botId`)
- `botId` — Azure Entra ID Application (client) ID from Step 1
- Keep this base profile at `supportsFiles: false`. Inline pasted images still
  work when attachment ingress is enabled. To accept Personal-chat paperclip
  files, create a separate reviewed manifest package with `supportsFiles: true`;
  Microsoft file consent is Personal-only and does not enable group-chat or
  channel files without Graph.
- `commandLists` only adds discovery. Teams submits the selected title as an
  ordinary message, so OpenAB's typed scope, identity, and structured mention
  gates remain authoritative. `/usage` is Personal-only because group/channel
  replies are not ephemeral.
- Do not add `supportsTargetedMessages`, command `triggers`, Graph/RSC, delegated
  permissions, or Adaptive Card permissions for this profile. App-package
  installation or upgrade remains an explicit operator action.
- The schema-validated source is
  [`platforms/examples/teams-manifest-v1.25.json`](platforms/examples/teams-manifest-v1.25.json).

### Icons

- `outline.png` — 32×32 transparent background, white icon
- `color.png` — 192×192 full-color icon

### Package

```bash
zip openab-teams-app.zip manifest.json outline.png color.png
```

## Agent Authentication (Both Modes)

Choose one Kiro authentication method before deploying either mode. For an
unattended enterprise deployment, use `KIRO_API_KEY`. Create
`openab-kiro-secret.yaml` and provision the value through your normal
secret-management workflow:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: openab-kiro
type: Opaque
stringData:
  KIRO_API_KEY: "<YOUR_KIRO_API_KEY>"
```

Apply the Secret in the same namespace where Helm will install OAB:

```bash
kubectl apply -f openab-kiro-secret.yaml
```

Both mode-specific values below use `secretEnv` to inject the key into the OAB
process and `[agent].env` to pass it through the ACP child's environment
allowlist after `env_clear()`. The Kiro child necessarily receives this agent
credential; it does not receive the separate `TEAMS_APP_SECRET`. Do not put
either key in `[agent].inherit_env`, and prefer an external secret controller in
production so plaintext values do not enter local files or shell history.

If an API key is unavailable, remove the `KIRO_API_KEY` `secretEnv` entry and
`[agent].env` line from the values for your chosen mode. Install OAB, confirm its
PVC is `Bound`, then complete the image-provided device flow:

```bash
kubectl exec -it deployment/openab-kiro -- sh -c '$OPENAB_AGENT_AUTH_COMMAND'
kubectl rollout restart deployment/openab-kiro
```

The PVC mounted at `/home/agent` preserves the resulting `~/.kiro` and
`~/.local/share/kiro-cli` credentials across pod restarts. Device flow requires
this one-time interactive bootstrap and is not a zero-touch first deployment.

## Step 4A: Deploy in Unified Mode (Recommended)

Unified Mode routes Bot Framework webhooks directly to the OAB pod:

```
Teams Client → Bot Framework → K8s Ingress (HTTPS) → OAB Pod (:8080/webhook/teams)
```

### Create the Teams Secret

Create `openab-teams-secret.yaml` and provision the client secret through your
normal secret-management workflow. Do not commit the populated file:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: openab-teams
type: Opaque
stringData:
  TEAMS_APP_SECRET: "<YOUR_CLIENT_SECRET>"
```

For production, prefer an external secret controller or another mechanism that
keeps the secret value out of local files and shell history.

### Helm Values

Create `values.yaml`:

```yaml
agents:
  kiro:
    persistence:
      enabled: true
      size: 1Gi
    env:
      TEAMS_APP_ID: "<YOUR_APPLICATION_ID>"
      TEAMS_OAUTH_ENDPOINT: "https://login.microsoftonline.com/<YOUR_TENANT_ID>/oauth2/v2.0/token"
    secretEnv:
      - name: KIRO_API_KEY
        secretName: openab-kiro
        secretKey: KIRO_API_KEY
      - name: TEAMS_APP_SECRET
        secretName: openab-teams
        secretKey: TEAMS_APP_SECRET
    configToml: |
      [teams]
      app_id = "${TEAMS_APP_ID}"
      app_secret = "${TEAMS_APP_SECRET}"
      oauth_endpoint = "${TEAMS_OAUTH_ENDPOINT}"
      allowed_tenants = ["<YOUR_TENANT_ID>"]
      allowed_users = ["29:1abc..."]
      # Presence of any field below opts into typed L2 scope policy.
      allowed_teams = ["<YOUR_TEAM_ID>"]
      allowed_channels = [] # a Team OR channel match admits channel messages
      allow_personal = true
      allow_group_chats = false
      # reactions_enabled = true # public-preview live-tenant test only
      # processing_indicator = "message" # default off; no Graph/RSC
      # streaming = true # default off; progressive bot-owned content edits
      # inbound_attachments = true # default off; post-trust image/text materialization

      [agent]
      command = "kiro-cli"
      args = ["acp", "--trust-all-tools"]
      working_dir = "/home/agent"
      env = { KIRO_API_KEY = "${KIRO_API_KEY}" }

      [pool]
      max_sessions = 10
      session_ttl_hours = 24
```

The example creates a 1 Gi PVC. If the cluster has no default StorageClass, set
`agents.kiro.persistence.storageClass` or reuse a pre-provisioned claim with
`agents.kiro.persistence.existingClaim`; otherwise the PVC and pod remain
`Pending`. Confirm the claim is `Bound` with `kubectl get pvc` before testing.

Kiro ACP runs non-interactively in the pod. `--trust-all-tools` prevents tool
permission prompts from stalling a session, but it also auto-approves every tool
request exposed to the agent. In production, prefer an explicit non-interactive
trust policy that covers only required tools. If broad trust is retained, bound
its authority with pod security controls, service-account/IAM permissions,
network policies, and the tenant/user allowlists below.

`secretEnv` injects the Secret into the OAB process so it can resolve the
`[teams]` configuration. The pinned chart mounts `configToml` verbatim and does
not automatically add `secretEnv` keys to `[agent].inherit_env`. Do **not** add
any `TEAMS_*` keys there: the ACP agent does not need these adapter credentials,
and inheriting them would make the secret accessible to prompts and tools.

### User Trust, Tenant, and Conversation Scope

The recommended configuration restricts the Azure AD tenant, typed conversation surface, and individual Bot Framework sender IDs. `allowed_tenants` is the Gateway L1 tenant boundary; `allowed_teams` / `allowed_channels` and the Personal/group-chat switches are Core L2 policy; `allowed_users` is the independent Core L3 identity gate. Find each user's `29:…` sender ID in OAB logs and add it to `allowed_users`.

With both typed channel lists empty, all Team channels are in L2 scope. If either list is non-empty, a Team **or** channel ID match admits the conversation. Personal requires no mention; groupChat and channel messages require a structured Bot Framework mention targeting `recipient.id`. Text that merely looks like `@OpenAB` is not authority.

Omitting all four typed scope fields preserves and logs the legacy conversation-ID allowlist for rolling Core/Gateway upgrades. It does not replace the outbound conversation ID with a Team or channel ID.

To allow every user in the configured tenant instead, replace `allowed_users`
with the explicit broad-access opt-in below. Keep `allowed_tenants`; otherwise,
activities from other tenants also pass the tenant gate.

```toml
[teams]
allowed_tenants = ["<YOUR_TENANT_ID>"]
allow_all_users = true
```

### Service and Ingress

Create `openab-teams-networking.yaml`. The selector must include the chart name,
Helm release, and agent component. This example assumes the release name
`openab` and agent key `kiro` from `agents.kiro`, used by the install command
below. If you change either value, update the instance or component label here
and in the verification selector to match.

```yaml
apiVersion: v1
kind: Service
metadata:
  name: openab-teams
spec:
  selector:
    app.kubernetes.io/name: openab
    app.kubernetes.io/instance: openab
    app.kubernetes.io/component: kiro
  ports:
    - port: 8080
      targetPort: 8080
---
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: openab-teams
spec:
  ingressClassName: <YOUR_INGRESS_CLASS>
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
                name: openab-teams
                port:
                  number: 8080
```

Set `<YOUR_INGRESS_CLASS>` to the installed controller class (for example,
`nginx`, `alb`, or `traefik`). If the controller requires annotations, add its
specific annotations as well; do not assume that a multi-class cluster will
select this Ingress automatically.

Apply the resources and install the validated chart version:

```bash
kubectl apply -f openab-teams-secret.yaml
helm upgrade --install openab oci://ghcr.io/openabdev/charts/openab \
  --version 0.9.0-beta.10 \
  -f values.yaml
kubectl apply -f openab-teams-networking.yaml
```

### Verify Unified Mode

1. Confirm exactly one pod matches the Service selector:

   ```bash
   kubectl get pods \
     -l app.kubernetes.io/name=openab,app.kubernetes.io/instance=openab,app.kubernetes.io/component=kiro
   ```

2. Check startup logs with `kubectl logs deployment/openab-kiro` and verify the
   Teams adapter is listening on `0.0.0.0:8080`.
3. Send a message from an allowed Teams user and confirm a reply. A user or
   tenant outside the configured allowlists must be rejected.

## Step 4B: Deploy in Standalone Gateway Mode (Legacy)

Use this mode only when you need the separate gateway architecture:

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

### Deploy the Gateway

The Gateway is deployed separately from the OAB Helm chart. Save the following Secret, Deployment/Service, and Ingress manifests together as `openab-gateway.yaml`:

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
  TEAMS_ALLOWED_TENANTS: "<YOUR_TENANT_ID>"
  # Optional, default off. Core must enable the same switch below.
  TEAMS_INBOUND_ATTACHMENTS: "true"
```

> **⚠️ Single Tenant bots must set `TEAMS_OAUTH_ENDPOINT`** to the tenant-specific endpoint. The default (`botframework.com`) only works for Multi Tenant bots and will cause `401 Unauthorized` errors. This is the #1 setup pitfall.

### Gateway Deployment

```yaml
---
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
          image: ghcr.io/openabdev/openab-gateway:0.5.4
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
---
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: openab-gateway
  annotations:
    # Adjust for your Ingress controller (nginx, ALB, Traefik, etc.)
    nginx.ingress.kubernetes.io/ssl-redirect: "true"
spec:
  ingressClassName: <YOUR_INGRESS_CLASS>
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

Set `<YOUR_INGRESS_CLASS>` to the installed controller class and replace or
remove the nginx annotation to match that controller.

> Bot Framework requires HTTPS. Your Ingress controller handles TLS termination — the Gateway pod listens on plain HTTP (:8080).

Apply the standalone Gateway resources before installing OAB:

```bash
kubectl apply -f openab-gateway.yaml
```

### Deploy OAB with Helm

OAB connects outbound to the Gateway via WebSocket. Create
`legacy-values.yaml` with the required raw `configToml` and an explicit user
allowlist:

```yaml
agents:
  kiro:
    secretEnv:
      - name: KIRO_API_KEY
        secretName: openab-kiro
        secretKey: KIRO_API_KEY
    configToml: |
      [gateway]
      url = "ws://openab-gateway:8080/ws"
      platform = "teams"

      [teams]
      allowed_teams = ["<YOUR_TEAM_ID>"]
      allowed_channels = []
      allow_personal = true
      allow_group_chats = false
      processing_indicator = "off" # set "message" after Gateway capability validation
      streaming = false # enable only after Gateway capability validation
      inbound_attachments = true # must match the Standalone Gateway env switch
      allowed_users = ["29:1abc..."]

      [agent]
      command = "kiro-cli"
      args = ["acp", "--trust-all-tools"]
      working_dir = "/home/agent"
      env = { KIRO_API_KEY = "${KIRO_API_KEY}" }

      [pool]
      max_sessions = 10
      session_ttl_hours = 24
```

The chart mounts `configToml` verbatim. The sample uses first-class `[teams]` typed L2 policy plus an explicit L3 `allowed_users` list for least privilege; find each user's `29:…` sender ID in OAB logs. To admit every user that passed the Gateway's tenant and Core scope checks, use the explicit `[teams].allow_all_users = true` opt-in instead.

For a rolling deployment with an older Gateway, absent additive scope falls back to the legacy `[gateway].allowed_channels` conversation-ID policy. Do not put Team IDs into that legacy field: they are not Bot Connector conversation IDs. Upgrade the Gateway before relying exclusively on Team/channel typed allowlists.

Attachment ingress must be explicitly enabled on both processes. Core downloads nothing unless a valid Gateway hello advertises `supports_attachment_materialization`; Gateway stores only bounded metadata until Core has admitted structural, typed L2, and L3 trust. Do not pass Microsoft download URLs or bot credentials through Core configuration.

Install the chart version validated with Gateway 0.5.4:

```bash
helm upgrade --install openab oci://ghcr.io/openabdev/charts/openab \
  --version 0.9.0-beta.10 \
  -f legacy-values.yaml
```

Do not set `agents.kiro.gateway.enabled` here: that option asks the chart to
deploy another Gateway, while this guide already created `openab-gateway`
separately. For platform connectivity, the OAB pod needs only the WebSocket URL
and trust configuration; it still needs the Kiro credential configured in
[Agent Authentication](#agent-authentication-both-modes), but it does not need
the Teams client secret.

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

Unified Mode uses `[teams].allowed_tenants` as shown in Step 4A. In Standalone Gateway Mode, restrict which Azure AD tenants can interact with the bot by adding this value to the Gateway Secret:

```yaml
stringData:
  TEAMS_ALLOWED_TENANTS: "<YOUR_TENANT_ID>"
```

Multiple tenants: `"<TENANT_ID_1>,<TENANT_ID_2>"`. If not set, all tenants are allowed.

## Sovereign Cloud Limitation

This guide currently supports the public Azure cloud only. Although the
configuration exposes OAuth and OpenID metadata endpoint overrides, the Teams
adapter in `v0.9.0-beta.10` still validates the public Bot Framework issuer and
requests the public Bot Framework OAuth scope internally. Changing only
`TEAMS_OAUTH_ENDPOINT` and `TEAMS_OPENID_METADATA` is therefore insufficient for
Azure Government or Azure China. Do not use this deployment recipe in a
sovereign cloud until the runtime makes the issuer and scope cloud-aware.

## Teams Adapter Environment Variables

In Unified Mode, the `[teams]` section resolves these variables from the OAB
process; inject `TEAMS_APP_SECRET` with `secretEnv`. In Standalone Gateway Mode,
set transport variables on the Gateway through `openab-gateway-teams`. Typed scope and user trust remain in the OAB `configToml` (or Core process environment) shown for each mode; setting them only on a Standalone Gateway does not configure Core routing policy.

| Variable | Required | Default | Description |
|---|---|---|---|
| `TEAMS_APP_ID` | Yes | — | Azure Entra ID application (client) ID |
| `TEAMS_APP_SECRET` | Yes | — | Azure Entra ID client secret |
| `TEAMS_OAUTH_ENDPOINT` | Yes (Single Tenant) | `https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token` | Tenant-specific OAuth endpoint |
| `TEAMS_OPENID_METADATA` | No | `https://login.botframework.com/v1/.well-known/openidconfiguration` | OpenID metadata for JWT validation |
| `TEAMS_ALLOWED_TENANTS` | No | (allow all) | Comma-separated tenant IDs |
| `TEAMS_WEBHOOK_PATH` | No | `/webhook/teams` | Webhook endpoint path |
| `TEAMS_DEDUPE_TTL_SECS` | No | `600` | Process-local duplicate suppression window |
| `TEAMS_ROUTE_TTL_SECS` | No | `3600` | Authenticated ephemeral route lifetime |
| `TEAMS_MAX_ROUTE_ENTRIES` | No | `10000` | Independent capacity bound for route, dedupe, and bot-owned activity caches |
| `TEAMS_REACTIONS_ENABLED` | No | `false` | Opt in to public-preview Bot Connector add/remove reactions; no Graph/RSC grant required |
| `TEAMS_PROCESSING_INDICATOR` | No | `off` | Core processing UX: `off` or `message`; malformed values fail closed to `off` |
| `TEAMS_STREAMING` | No | `false` | Core progressive-content opt-in; accepts only `true`/`false` or `1`/`0`, malformed values fail closed |
| `TEAMS_INBOUND_ATTACHMENTS` | No | `false` | Core/Gateway metadata-first image/text opt-in; set identically on both Standalone processes. Only `true`/`false` or `1`/`0`; malformed values fail closed. |
| `TEAMS_ALLOWED_TEAMS` | No | (empty) | Core typed L2 Team-ID allowlist, comma-separated; Team OR channel match |
| `TEAMS_ALLOWED_CHANNELS` | No | (empty) | Core typed L2 channel-ID allowlist, comma-separated; both lists empty means all Team channels |
| `TEAMS_ALLOW_PERSONAL` | No | `true` | Core typed L2 Personal-chat switch |
| `TEAMS_ALLOW_GROUP_CHATS` | No | `true` | Core typed L2 group-chat switch |
| `TEAMS_ALLOW_ALL_USERS` | No | `false` | Core L3 broad user opt-in |
| `TEAMS_ALLOWED_USERS` | No | (empty) | Core L3 Bot Framework sender IDs, comma-separated |

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

### Unified Mode receives webhook but no reply in Teams

Check the Ingress and embedded adapter logs:

```bash
kubectl describe ingress openab-teams
kubectl logs deployment/openab-kiro --tail=50
```

Confirm that `/webhook/teams` routes to Service `openab-teams`, the Service has
one ready endpoint, and the activity matches `[teams].allowed_tenants`, the typed Team/channel/Personal/group-chat policy, structured mention rules, and `[teams].allowed_users`.

### Standalone Gateway receives webhook but no reply in Teams

Check Gateway pod logs:

```bash
kubectl logs deployment/openab-gateway --tail=50
```

Look for: `teams → gateway` (received) → `gateway → teams` (sent) →
`teams activity sent` (success), `teams reply rejected` (definite failure), or
`teams reply delivery is unknown; not retrying` (the POST may have completed).
New Core↔Gateway peers require a structured send ACK and return the real Teams
activity ID; legacy peers retain fire-and-forget missing-ACK behavior when no
valid hello is negotiated.

If logs report `route_not_found` or `Teams ingress route is missing or expired`,
the Gateway restarted, the bounded route was evicted, or the route TTL elapsed
before the response. Have the user send a new activity; never bypass the route
with a manually supplied `serviceUrl`.

For `message_not_owned`, `target_origin_not_found`, or `target_scope_*`, verify
that the target is a bot activity created by the same Gateway process and that
its origin event, tenant, and conversation still match. Restart, TTL expiry,
capacity eviction, or a user-message target intentionally fails closed.

For the reaction preview, set `[teams].reactions_enabled = true` in Unified mode
or `TEAMS_REACTIONS_ENABLED=true` on the Standalone Gateway. Restart both peers,
then verify `gateway → teams reaction` logs and visible status transitions in
personal, group-chat, and channel scopes. No Graph/RSC consent is needed. A
`reaction_target_not_known` rejection indicates expired or cross-scope route
evidence; inbound user reaction events remain deferred.

For the generally available processing-message path, set
`[teams].processing_indicator = "message"` on Core. The default is `off`.
Standalone Core enables it only after a valid Gateway hello advertises real-ID
send plus bot-owned edit/delete ACK support. Each turn creates at most one
status activity, marks it terminal before final delivery, and deletes it after
all final chunks succeed. Do not count a request-access echo as a processing
indicator; L3 `[teams].allowed_users` remains an independent prerequisite.

For the progressive-content path, set `[teams].streaming = true` on Core. It is
independent of the processing indicator and reaction preview, and remains off
unless Standalone hello (or Unified Teams) proves required send/edit/delete ACK,
real target IDs, and placeholder support. One admitted turn owns at most one
content placeholder. Unknown write outcomes are intentionally not retried and
do not trigger a fresh warning/final activity. The implementation uses only Bot
Connector POST/PUT/DELETE; Microsoft 365 live validation remains pending.

> **Release validation:** Before marking Teams PR 3／PR 4 complete, record
> personal, group-chat, channel-root, channel-reply, explicit-quote, and
> bot-owned update/delete behavior in the
> [Microsoft 365 live-tenant matrix](msteams-discord-parity-requirements.md#202-live-microsoft-365-tenant-matrix).
> HTTP mocks and successful Connector activity IDs do not close this gate.

### JWT validation failed

The Teams adapter auto-refreshes JWKS on cache miss. For the supported public
Azure cloud, verify that the metadata endpoint is reachable from the Kubernetes
namespace without assuming that the OpenAB or Gateway image contains `curl`:

```bash
kubectl run openab-metadata-check --rm -i --restart=Never \
  --image=curlimages/curl:8.14.1 -- \
  https://login.botframework.com/v1/.well-known/openidconfiguration
```

## Security Considerations

- **Credentials in Kubernetes Secrets** — never in ConfigMaps or Deployment manifests
- **Rotate client secrets** before expiration — set a reminder based on the expiration chosen in Step 1
- **Use a tenant allowlist** in production — configure `[teams].allowed_tenants` in Unified Mode or `TEAMS_ALLOWED_TENANTS` in Standalone Gateway Mode
- **Bound Core scope and identity independently** — configure the typed Team/channel/Personal/group-chat policy and an explicit `[teams].allowed_users` list; a scope match never bypasses L3 sender trust
- **Network policies** — start from default-deny and allow cluster DNS plus
  the minimum outbound destinations. The M0 public-cloud profile permits the
  `login.microsoftonline.com` token endpoint, `login.botframework.com` metadata
  and JWKS, and validated HTTPS Bot Connector service URLs on
  `smba.trafficmanager.net`. Attachment-enabled Gateway pods additionally need
  HTTPS egress to the compiled commercial Microsoft file-host profile:
  `api.asm.skype.com`, `files.teams.microsoft.com`, and the corresponding root
  or subdomains under `sharepoint.com`, `sharepointonline.com`, `1drv.com`,
  `onedrive.com`, and `blob.core.windows.net`. Sovereign-cloud and custom proxy
  hosts are rejected. The OAB/ACP pod does not fetch Microsoft attachments; it
  still needs the authentication/API endpoints for the selected agent backend
  and any model or tool services it uses. In Unified Mode these
  rules apply to the OAB pod. In Standalone Gateway Mode, give the Gateway the
  Microsoft egress, allow OAB to reach `openab-gateway:8080`, and give only OAB
  the agent/model/tool egress.
- **Minimize inbound exposure** — Unified Mode should expose only `/webhook/teams` through the TLS Ingress; in Standalone Gateway Mode, the OAB pod remains private and connects outbound to the Gateway only

## References

- [Azure Bot Service documentation](https://learn.microsoft.com/en-us/azure/bot-service/)
- [Register a bot with Azure](https://learn.microsoft.com/en-us/azure/bot-service/bot-service-quickstart-registration)
- [Teams app permission policies](https://learn.microsoft.com/en-us/microsoftteams/teams-app-permission-policies)
- [Teams custom app policies](https://learn.microsoft.com/en-us/microsoftteams/teams-custom-app-policies-and-settings)
- [Bot Framework authentication](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication)
- [Teams app manifest schema](https://learn.microsoft.com/en-us/microsoftteams/platform/resources/schema/manifest-schema)
