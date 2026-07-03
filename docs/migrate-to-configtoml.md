# Migrating to `configToml`

Chart v0.10.0 removes the Helm-rendered config path (`slack.*`, `discord.*`,
`pool.*`, etc. in `values.yaml`). Use `configToml` to write `config.toml`
directly instead.

For the platform-agnostic option (works identically on Kubernetes, ECS,
Zeabur, and AgentCore Runtime — no chart required), see `configUrl` instead:
[`docs/adr/configurl-over-helm-rendering.md`](adr/configurl-over-helm-rendering.md). `configToml` is the
Kubernetes/Helm-only convenience for users not yet on external config storage.

## Why

The old path was a ~350-line TOML serializer written in Helm template language.
Every new config field required a chart release. The rendered output was invisible
to users without running `helm template`. `configToml` is a direct pass-through —
what you write is what gets deployed.

## Two ways to set `configToml`

**Inline** — paste TOML directly into `values.yaml`:

```yaml
agents:
  kiro:
    configToml: |
      [discord]
      bot_token = "${DISCORD_BOT_TOKEN}"
```

**As-is from a standalone file** — keep `config.toml` as a real file (full IDE
syntax highlighting / TOML schema validation) and load it verbatim with Helm's
built-in `--set-file`:

```bash
helm upgrade openab openab/openab \
  --set-file agents.kiro.configToml=./config.toml
```

`--set-file` reads the file's raw content and assigns it as a string to
`agents.kiro.configToml`, merging the same way `--set` does. No chart changes
needed — this is the recommended way to keep `config.toml` WYSIWYG-editable.

## Before → After

### Slack

**Before:**
```yaml
agents:
  claude:
    slack:
      enabled: true
      existingSecret: openab
      assistantMode: true
      allowUserMessages: "mentions"
      allowedChannels:
        - "C01234567"
      allowedUsers:
        - "U01234567"
    pool:
      maxSessions: 10
      sessionTtlHours: 24
    reactions:
      enabled: true
      removeAfterReply: false
    command: claude-agent-acp
    workingDir: /home/node
    secretEnv:
      - name: ANTHROPIC_API_KEY
        secretName: openab
        secretKey: ANTHROPIC_API_KEY
```

**After:**
```yaml
agents:
  claude:
    secretEnv:
      - name: ANTHROPIC_API_KEY
        secretName: openab
        secretKey: ANTHROPIC_API_KEY
      - name: SLACK_BOT_TOKEN
        secretName: openab
        secretKey: slack-bot-token
      - name: SLACK_APP_TOKEN
        secretName: openab
        secretKey: slack-app-token
    configToml: |
      [slack]
      bot_token = "${SLACK_BOT_TOKEN}"
      app_token = "${SLACK_APP_TOKEN}"
      allowed_channels = ["C01234567"]
      allowed_users = ["U01234567"]
      allow_user_messages = "mentions"
      assistant_mode = true

      [agent]
      command = "claude-agent-acp"
      working_dir = "/home/node"
      inherit_env = ["ANTHROPIC_API_KEY"]

      [pool]
      max_sessions = 10
      session_ttl_hours = 24

      [reactions]
      enabled = true
      remove_after_reply = false
```

### Discord

**Before:**
```yaml
agents:
  kiro:
    discord:
      botToken: "${DISCORD_BOT_TOKEN}"
      allowedChannels:
        - "123456789012345678"
      allowedUsers:
        - "987654321098765432"
    command: kiro-cli
```

**After:**
```yaml
agents:
  kiro:
    secretEnv:
      - name: DISCORD_BOT_TOKEN
        secretName: my-secret
        secretKey: discord-bot-token
    configToml: |
      [discord]
      bot_token = "${DISCORD_BOT_TOKEN}"
      allowed_channels = ["123456789012345678"]
      allowed_users = ["987654321098765432"]

      [agent]
      command = "kiro-cli"
```

## Secrets

Secrets must still be injected via `secretEnv` — they render as `valueFrom.secretKeyRef`
in the Deployment and are available as environment variables. Reference them in
`configToml` as `${VAR_NAME}` placeholders exactly as before.

## Tip: see your current rendered config

Before migrating, check what your current `config.toml` looks like:

```bash
kubectl exec -it deployment/<your-agent> -- cat /etc/openab/config.toml
```

Use that output as the starting point for your `configToml` value.
