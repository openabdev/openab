# Codex

Codex uses the [@zed-industries/codex-acp](https://github.com/zed-industries/codex-acp) adapter for ACP support.
The recommended working directory for the Codex image is `/home/node`; this is
also the container `HOME`, so Codex auth, sessions, generated images, and skills
live under `/home/node/.codex/`.

## Docker Image

```bash
docker build -f Dockerfile.codex -t openab-codex:latest .
```

The image installs `@zed-industries/codex-acp` and `@openai/codex` globally via npm.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.codex.discord.enabled=true \
  --set agents.codex.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.codex.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.codex.image=ghcr.io/openabdev/openab-codex:latest \
  --set agents.codex.command=codex-acp \
  --set agents.codex.workingDir=/home/node
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

## Manual config.toml

```toml
[agent]
# command defaults from OPENAB_AGENT_COMMAND="codex"
# Only override if you need non-default behavior
```

## Authentication

```bash
kubectl exec -it deployment/openab-codex -- sh -c "$OPENAB_AGENT_AUTH_COMMAND"
```

Follow the device code flow in your browser, then restart the pod:

```bash
kubectl rollout restart deployment/openab-codex
```

### Persisted Paths (PVC)

| Path | Contents |
|------|----------|
| `/home/node/.codex/auth.json` | Codex login credentials |
| `/home/node/.codex/config.toml` | Codex CLI settings and feature flags |
| `/home/node/.codex/sessions/` | Session history |
| `/home/node/.codex/generated_images/` | Built-in image generation outputs |
| `/home/node/.codex/skills/` | User-created Codex skills |

## Image Generation

Codex built-in image generation uses the **`gpt-image-2`** model under the hood.
It is controlled by the Codex CLI feature flag `image_generation`. Enable it
once inside the pod:

```bash
kubectl exec -it deployment/openab-codex -- \
  codex features enable image_generation
```

This writes the following to `/home/node/.codex/config.toml`:

```toml
[features]
image_generation = true
```

You can verify it with:

```bash
kubectl exec -it deployment/openab-codex -- \
  codex features list | grep image_generation
```

Generated images are saved by Codex under
`/home/node/.codex/generated_images/...`. If the user needs a stable path, ask
Codex to copy the selected output into `/home/node`, for example
`/home/node/sky-birds.png`.

> Note: Codex image generation may return a model-native size rather than the
> exact dimensions requested in the prompt. If exact dimensions matter, resize
> only when the user explicitly asks for it.

### Quick Imagegen Smoke Test

```bash
kubectl exec -it deployment/openab-codex -- \
  codex exec \
    --dangerously-bypass-approvals-and-sandbox \
    --enable image_generation \
    --skip-git-repo-check \
    -C /home/node \
    "Use the imagegen skill and the built-in image_gen tool. Generate a simple image of birds flying across a bright sky. Save or copy the final PNG to /home/node/sky-birds.png. Report the output path and dimensions."
```

Then check for output:

```bash
kubectl exec -it deployment/openab-codex -- \
  sh -lc 'ls -lh /home/node/sky-birds.png /home/node/.codex/generated_images/*/* 2>/dev/null | tail'
```

## Sending Generated Images Back to Discord

OpenAB streams text over ACP only. It does **not** relay image attachments from
Codex back to Discord. To send a generated image, Codex must call the Discord
REST API directly. See [sendimages.md](sendimages.md) for the full protocol.

The agent should:

1. Read `thread_id` from OpenAB's `<sender_context>` and use it as the Discord
   target channel. If `thread_id` is absent, fall back to `channel_id`.
2. Upload the file with `POST /channels/{id}/messages` using multipart form
   data.
3. Read the token from `DISCORD_FILE_BOT_TOKEN` if available, otherwise
   `DISCORD_BOT_TOKEN`.

Example upload from inside the pod:

```bash
THREAD_ID="1499442140172910654"
IMAGE="/home/node/sky-birds.png"

curl -X POST "https://discord.com/api/v10/channels/${THREAD_ID}/messages" \
  -H "Authorization: Bot ${DISCORD_FILE_BOT_TOKEN:-$DISCORD_BOT_TOKEN}" \
  -F "content=Here is the generated image" \
  -F "files[0]=@${IMAGE}"
```

### Agent Environment for Uploads

The Discord bot token configured under `[discord]` is consumed by OpenAB itself.
For safety, OpenAB clears the inherited environment before spawning the agent and
only passes variables listed in `[agent].env`. If Codex should upload images
itself, explicitly expose an upload token to the agent:

```toml
[agent]
# command defaults from OPENAB_AGENT_COMMAND="codex"
# Only override if you need non-default behavior
env = { DISCORD_FILE_BOT_TOKEN = "${DISCORD_FILE_BOT_TOKEN}" }
```

For production, prefer a dedicated "File Deliverer" Discord bot with only
`Send Messages`, `Send Messages in Threads`, and `Attach Files` permissions.
For small personal deployments, using the same bot token is simpler but gives
the agent the same Discord permissions as the main OpenAB bot.

## Recommended Skill

For repeated image requests, save the imagegen + Discord upload workflow as a
Codex skill under `/home/node/.codex/skills/`, for example:

```text
/home/node/.codex/skills/discord-imagegen-deliver/
+-- SKILL.md
`-- scripts/
    `-- send-discord-image.sh
```

The skill should instruct Codex to:

- Use the built-in `imagegen` skill and `image_gen` tool for raster images.
- Keep the generated image size as-is unless the user explicitly asks for
  resizing.
- Copy the selected file from `/home/node/.codex/generated_images/...` to a
  stable path under `/home/node`.
- Upload it to `thread_id` or `channel_id` using the Discord REST API.
- Avoid printing token values.

Example user prompt after creating such a skill:

```text
Use $discord-imagegen-deliver to generate a warm hand-painted sky with birds and send it back to this Discord thread.
```

## Approval Policy & Auto-review

Codex offers three approval modes that control what happens when the agent
tries to act outside the sandbox (network calls, running scripts, etc.):

| Mode | Behaviour | Best for |
|------|-----------|----------|
| **Manual** (`approval_policy = "on-request"`) | Every out-of-sandbox action waits for a human to approve | Interactive, attended sessions |
| **Auto-review** (`approval_policy = "auto-review"`) | A separate reviewer agent (GPT-5.4 Thinking) approves or denies automatically | **OpenAB / unattended agents** |
| **Full Access** (`approval_policy = "full-access"`) | No sandbox enforcement at all | Trusted, isolated environments only |

For OpenAB deployments, **Auto-review is the recommended mode**. OpenAB agents
run as long-lived background processes with no human watching the terminal, so
manual approval is impractical and Full Access removes all guardrails.

Enable Auto-review in `/home/node/.codex/config.toml`:

```toml
approval_policy = "auto-review"
```

> `approval_policy` is a **top-level** key in `config.toml`, not under a
> `[sandbox]` section. Codex silently ignores it if nested.

Or mount a `ConfigMap` containing the codex `config.toml` into the agent via the
chart's `extraVolumes` / `extraVolumeMounts` (the chart does not expose a
dedicated `extraConfig` value — anything written into the codex config file has
to come in as a mounted file or be pre-seeded into the PVC):

```bash
# Create the codex config as a ConfigMap.
kubectl create configmap codex-config \
  --from-literal=config.toml='approval_policy = "auto-review"'

# values.yaml — mount it over /home/node/.codex/config.toml
agents:
  codex:
    extraVolumes:
      - name: codex-config
        configMap:
          name: codex-config
    extraVolumeMounts:
      - name: codex-config
        mountPath: /home/node/.codex/config.toml
        subPath: config.toml
```

> Mounting `config.toml` from a ConfigMap makes the file read-only inside the
> pod. If you also need codex to write back to it (e.g. `codex features enable`
> persisting flags), pre-seed the config on the PVC instead.

### What Auto-review does

- Approves ~99% of legitimate out-of-sandbox actions automatically.
- Blocks actions that could exfiltrate data, expose secrets, delete data, or
  weaken security settings.
- When it rejects an action, it gives the agent a rationale so Codex can find a
  safer alternative (succeeds >50% of the time without human input).
- Stops the trajectory after repeated denials to prevent gaming.

### Limitations

Auto-review is **not** a security guarantee. It can be misled by adversarial
inputs and cannot detect a model that hides malicious intent within the sandbox.
Treat it as a strong default, not a replacement for network-level controls and
secret management.

For more details, see the [OpenAI Alignment Blog post on Auto-review](https://alignment.openai.com/auto-review).

## Troubleshooting

### `bwrap: No permissions to create a new namespace`

Some Kubernetes environments do not allow unprivileged user namespaces, which can
block Codex's default sandbox when running nested `codex exec` commands. For
manual smoke tests inside an already isolated pod, use:

```bash
codex exec --dangerously-bypass-approvals-and-sandbox ...
```

Do not use this flag on an untrusted host.

### `bubblewrap is unavailable: no system bwrap was found on PATH`

Codex's Linux sandbox modes (read-only / workspace-write) rely on `bwrap`
(bubblewrap) to create an inner sandbox. If the runtime image does not include
bubblewrap, even basic commands like `pwd` or `ls` will fail before execution
with this error.

This commonly happens in OpenAB deployments where Codex already runs inside an
isolated container or VM — the outer runtime provides the desired isolation, so
the inner sandbox is redundant.

**Solution — Disable Codex's inner sandbox** (recommended when the outer OpenAB
runtime already provides isolation):

```toml
# /home/node/.codex/config.toml
sandbox_mode = "danger-full-access"
approval_policy = "auto-review"
```

> `sandbox_mode` and `approval_policy` are **top-level** keys in `config.toml`.
> A `[sandbox]` section header is silently ignored by Codex 0.137+ — verified
> empirically: with the nested form in place, `codex exec` still fails with
> `bwrap: No permissions to create new namespace`; moving the same keys to the
> top level makes `codex exec` report `sandbox: danger-full-access` and run.

> **Do NOT pair `danger-full-access` with `approval_policy = "on-request"` on
> an OpenAB deployment.** `on-request` pauses each tool call to wait for an
> interactive human approval, and OpenAB agents have no terminal attached —
> every tool call hangs in `in_progress` until openab's 1800 s hard timeout
> fires. Use `"auto-review"` (recommended, see
> [§Approval Policy](#approval-policy--auto-review)) or `"never"` for trusted
> and already-isolated pods (`"never"` removes all per-call guardrails — the
> outer pod isolation is the only remaining boundary).

Or launch with:

```bash
codex --sandbox danger-full-access
```

Or mount a ConfigMap via the chart's `extraVolumes` / `extraVolumeMounts`:

```bash
kubectl create configmap codex-config --from-file=config.toml=/path/to/config.toml
```

```yaml
# values.yaml
agents:
  codex:
    extraVolumes:
      - name: codex-config
        configMap:
          name: codex-config
    extraVolumeMounts:
      - name: codex-config
        mountPath: /home/node/.codex/config.toml
        subPath: config.toml
```

> **Important:** `danger-full-access` disables only Codex's *inner* sandbox. It
> does **not** remove the outer OpenAB container/VM isolation. The agent remains
> confined by the runtime's own security boundary. Ensure the outer runtime is a
> non-privileged container (no `--privileged` flag or excessive capabilities) for
> this security model to hold.

### Imagegen appears to hang

Check whether an image was generated even if the CLI has not returned yet:

```bash
find /home/node/.codex/generated_images -type f -name '*.png' -printf '%T@ %p %s\n' | sort -n | tail
```

If a file exists, copy it to a stable path and upload it manually with the
Discord API command above.

### No image upload appears in Discord

Verify the agent can see an upload token:

```bash
kubectl exec -it deployment/openab-codex -- \
  sh -lc 'test -n "$DISCORD_FILE_BOT_TOKEN$DISCORD_BOT_TOKEN" && echo token-present || echo token-missing'
```

Also confirm the bot has `Send Messages`, `Send Messages in Threads`, and
`Attach Files` permissions in the target channel or thread.
