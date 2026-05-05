# Qwen Code

[Qwen Code](https://github.com/QwenLM/qwen-code) supports ACP natively via the `--acp` flag — no adapter needed.

> ⚠️ **Authentication note**: Qwen OAuth free tier was discontinued on April 15, 2026, and the Alibaba Cloud Coding Plan subscription pricing has increased significantly. This guide uses **OpenRouter** as the provider, which offers pay-per-token access to Qwen models (including free tiers) without a subscription.

## Docker Image

```bash
docker build -f Dockerfile.qwen -t openab-qwen:latest .
```

The image installs `@qwen-code/qwen-code` globally via npm.

## Helm Install

### Step 1: Create the API key Secret

```bash
kubectl create secret generic qwen-secrets \
  --from-literal=OPENROUTER_API_KEY="sk-or-v1-..."
```

### Step 2: Install with Helm

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.qwen.discord.enabled=true \
  --set-string 'agents.qwen.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.qwen.image=ghcr.io/openabdev/openab-qwen:latest \
  --set agents.qwen.command=qwen \
  --set 'agents.qwen.args={--acp,--yolo}' \
  --set agents.qwen.workingDir=/home/node \
  --set agents.qwen.secretEnv[0].name=OPENROUTER_API_KEY \
  --set agents.qwen.secretEnv[0].secretName=qwen-secrets \
  --set agents.qwen.secretEnv[0].secretKey=OPENROUTER_API_KEY
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

### Step 3: Write settings.json to the PVC

The PVC is mounted at `/home/node/` by default (`persistence.enabled: true`). Write `settings.json` once — it persists across pod restarts:

```bash
kubectl exec -it deployment/openab-qwen -- sh -c 'mkdir -p ~/.qwen && cat > ~/.qwen/settings.json << EOF
{
  "modelProviders": {
    "openai": [
      {
        "id": "qwen/qwen3-coder",
        "name": "Qwen3 Coder (OpenRouter)",
        "baseUrl": "https://openrouter.ai/api/v1",
        "envKey": "OPENROUTER_API_KEY"
      }
    ]
  },
  "security": {"auth": {"selectedType": "openai"}},
  "model": {"name": "qwen/qwen3-coder"}
}
EOF'
```

### Step 4: Restart the pod

```bash
kubectl rollout restart deployment/openab-qwen
```

That's it. The pod will start with `settings.json` on the PVC and `OPENROUTER_API_KEY` from the Secret.

> **`--yolo`**: Required for headless/ACP operation — auto-approves all tool calls. Only use in isolated environments (containers, K8s pods). See [security note](#security-note) below.

## Manual config.toml

```toml
[agent]
command = "qwen"
args = ["--acp", "--yolo"]
working_dir = "/home/node"
# ⚠️ Passing API keys via env is for LOCAL DEV ONLY. In K8s, use secretEnv
# (valueFrom.secretKeyRef) to avoid storing keys in plaintext config.
env = { OPENROUTER_API_KEY = "${OPENROUTER_API_KEY}" }
```

## Authentication via OpenRouter

Qwen Code uses `~/.qwen/settings.json` to configure model providers. To use OpenRouter:

### 1. Get an OpenRouter API key

Sign up at [openrouter.ai](https://openrouter.ai) and create an API key.

### 2. Create `~/.qwen/settings.json`

The example below uses `qwen/qwen3-coder` (recommended for coding tasks). To start for free, replace both occurrences of `qwen/qwen3-coder` with `qwen/qwen3.6-plus:free` — see the [Alternative models](#alternative-models-on-openrouter) table below.

```json
{
  "modelProviders": {
    "openai": [
      {
        "id": "qwen/qwen3-coder",
        "name": "Qwen3 Coder (OpenRouter)",
        "baseUrl": "https://openrouter.ai/api/v1",
        "description": "Qwen3-Coder via OpenRouter",
        "envKey": "OPENROUTER_API_KEY"
      }
    ]
  },
  "security": {
    "auth": {
      "selectedType": "openai"
    }
  },
  "model": {
    "name": "qwen/qwen3-coder"
  }
}
```

### 3. Set the API key

```bash
export OPENROUTER_API_KEY="sk-or-v1-..."
```

### Alternative models on OpenRouter

| Model ID | Description |
|----------|-------------|
| `qwen/qwen3-coder` | Qwen3-Coder (recommended for coding) |
| `qwen/qwen3.6-plus` | Qwen3.6 Plus |
| `qwen/qwen3.6-plus:free` | Qwen3.6 Plus (free tier, rate-limited) |

### Why OpenRouter instead of native Qwen subscription?

1. **Cost**: Qwen OAuth free tier was discontinued; the Coding Plan subscription price increased significantly
2. **Flexibility**: OpenRouter offers pay-per-token pricing with no monthly commitment
3. **Free tiers**: Some Qwen models on OpenRouter have free tiers for experimentation
4. **Multi-model**: Same API key works for 300+ models if you want to switch later

## Advanced: Declarative settings.json (GitOps)

For GitOps workflows where you want `settings.json` guaranteed on every pod recreate (without manual `kubectl exec`), use an init container + ConfigMap:

```yaml
agents:
  qwen:
    extraInitContainers:
      - name: copy-qwen-settings
        image: "busybox:1.37"
        command: ["sh", "-c", "cp /qwen-config/settings.json /home/node/.qwen/settings.json"]
        volumeMounts:
          - name: qwen-config
            mountPath: /qwen-config
            readOnly: true
    extraVolumes:
      - name: qwen-config
        configMap:
          name: qwen-settings
```

> The PVC at `/home/node/` is already writable, so no `emptyDir` is needed. The init container simply copies from the read-only ConfigMap into the PVC path.

## Persisted Paths (PVC)

| Path | Contents |
|------|----------|
| `~/.qwen/settings.json` | Provider config (OpenRouter endpoint + model) |
| `~/.qwen/` | Session data and cache |
| `/home/node/` | Working directory — project files checked out by the agent |

The PVC at `/home/node/` covers everything. `settings.json` persists across pod restarts after the initial `kubectl exec`.

## Security Note

**`--yolo`**: This flag enables YOLO mode — automatically approves all tool calls (shell commands, file edits, etc.) without interactive confirmation. It is required for headless/ACP operation since there is no interactive user to approve each tool call. Only use this in isolated environments (Docker containers, K8s pods) where the workload and network access are already constrained. Do not use on a shared machine with broad filesystem or network access.
