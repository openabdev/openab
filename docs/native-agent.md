# Native Agent (openab-agent)

A lightweight, native Rust coding agent with built-in ACP support and ChatGPT subscription authentication. No Node.js, no Python, no adapter layer.

## Quick Start

```bash
# Build
cd openab-agent && cargo build --release

# Authenticate (browser flow — recommended)
openab-agent auth codex-oauth

# Headless server (paste callback URL)
openab-agent auth codex-oauth --no-browser

# Run as ACP server (used by openab core)
openab-agent
```

## Configuration

```toml
[agent]
# command defaults from OPENAB_AGENT_COMMAND="openab-agent"
# working_dir = "/home/agent"  # optional — defaults to $HOME
env = { OPENAB_AGENT_OPENAI_MODEL = "gpt-5.4-mini" }
```

### Configuration file (config.json)

A small **valid JSON** file next to `auth.json` declares the default model and
parameters, so a deployment can set them in a file instead of only via
environment variables. The default path is `$HOME/.openab/agent/config.json`
(`/home/agent/.openab/agent/config.json` for the `agent` user). Set
`OPENAB_CONFIG_PATH` to override the whole path. **Secrets never go here** —
credentials stay in the locked `auth.json` store.

For example, an xAI deployment can use:

```json
{
  "model": "xai/grok-4.5",
  "max_tokens": 8192
}
```

The supported fields are `model` (a `provider/model` string) and `max_tokens`.
A missing file is fine (empty config); malformed JSON is logged and ignored, so
the agent falls back to environment variables and built-in defaults. Unknown
keys are tolerated for forward compatibility.

Provider selection and value precedence are separate but related:

1. `OPENAB_AGENT_PROVIDER` explicitly selects `anthropic`, `openai`, `codex`,
   `xai`, or `grok`.
2. Otherwise, a provider prefix in `OPENAB_AGENT_MODEL` wins (for example,
   `xai/grok-4.5`).
3. Otherwise, a provider prefix in `config.json`'s `model` is used.
4. With no explicit provider, auto-detection remains Anthropic → Codex;
   xAI is **not** selected merely because an `xai-oauth` token exists.

`OPENAB_AGENT_MODEL` and `OPENAB_AGENT_MAX_TOKENS` override their config-file
values, so environment variables injected into a pod remain authoritative.
`OPENAB_AGENT_XAI_MODEL` controls the xAI model after xAI has been selected; it
does not by itself enable xAI auto-detection.

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `OPENAB_AGENT_MODEL` | — (required for Anthropic) | Model to use, optionally `provider/`-qualified (for example, `anthropic/claude-opus-4-8` or `xai/grok-4.5`). Anthropic has no hardcoded default and fails loud if unset; xAI falls back to `grok-4.5`. Overrides `model` in [config.json](#configuration-file-configjson). |
| `OPENAB_AGENT_OPENAI_MODEL` | `gpt-5.4-mini` | Model to use (must be supported by your ChatGPT plan — see [Supported Models](#supported-models-chatgpt-subscription)) |
| `OPENAB_AGENT_OPENAI_BASE_URL` | `https://chatgpt.com/backend-api` | API base URL |
| `OPENAB_AGENT_XAI_MODEL` | `grok-4.5` | xAI model to use (see [xAI credentials](#xai-credentials-supergrok--x-premium)) |
| `OPENAB_AGENT_XAI_BASE_URL` | `https://api.x.ai/v1` | xAI API base URL. Must be an `https://` URL on an `x.ai` host — the OAuth bearer is never sent elsewhere. |
| `OPENAB_AGENT_PROVIDER` | auto-detect | Force provider (`anthropic`, `openai`, `codex`, `xai`, `grok`) |
| `OPENAB_AGENT_MAX_TOKENS` | `8192` | Max output tokens. Overrides `max_tokens` in config.json. |
| `OPENAB_AGENT_OAUTH_CLIENT_ID` | Pi's client | Custom Codex OAuth client ID |
| `OPENAB_AGENT_ANTHROPIC_CLIENT_ID` | Claude Code's client | Custom Anthropic OAuth client ID |
| `OPENAB_AGENT_XAI_CLIENT_ID` | grok CLI's client | Custom xAI OAuth client ID |
| `OPENAB_AGENT_MAX_TOOL_LOOPS` | `50` | Max tool-call iterations per prompt before the agent gives up |
| `ANTHROPIC_API_KEY` | — | Anthropic API key. Highest-precedence Anthropic credential (see [Anthropic credentials](#anthropic-credentials)). |
| `CLAUDE_CODE_OAUTH_TOKEN` | — | Pre-provisioned long-lived Claude Pro/Max subscription token (from `claude setup-token`). Fleet route — no interactive login, no `auth.json` write. |
| `OPENAB_CONFIG_PATH` | `<auth dir>/config.json` | Override the config-file path. |

## Authentication

### Browser PKCE Flow (recommended)

```bash
openab-agent auth codex-oauth
```

Opens browser to authenticate with your ChatGPT Plus/Pro subscription.

### Headless Server (paste flow)

```bash
openab-agent auth codex-oauth --no-browser
```

1. Prints an authorization URL
2. Open it in any browser and approve
3. Browser redirects to `localhost:1455` (fails on remote server)
4. Copy the full URL from the browser address bar
5. Paste it back into the terminal

### Device Code Flow

```bash
openab-agent auth codex-device
```

Note: Device flow currently has limited scopes and may not work with all models.

### Anthropic credentials

Three ways to authenticate Anthropic, resolved in this **precedence** (ADR §5.3):

1. **API key** — `export ANTHROPIC_API_KEY=sk-ant-...`. No login; auto-detected.
2. **Pre-provisioned subscription token (fleet route)** — `export CLAUDE_CODE_OAUTH_TOKEN=...`
   (mint once with `claude setup-token`; ~1-year Claude Pro/Max token). Sent as a
   `Bearer` subscription token with the Claude Code identity headers — no
   interactive login, no `auth.json` write, no refresh. Recommended for pods (inject
   as a k8s secret).
3. **Interactive Claude Pro/Max OAuth** — browser PKCE login, refreshed from the
   stored `anthropic-oauth` tenant in `auth.json`:

   ```bash
   openab-agent auth anthropic-oauth            # browser
   openab-agent auth anthropic-oauth --no-browser  # paste code#state
   ```

A higher-precedence source's own errors (e.g. a key set but no model) surface
rather than silently falling through to a lower one.

### xAI credentials (SuperGrok / X Premium)

Sign in with a SuperGrok or X Premium subscription via the RFC 8628 device-code
flow — no `XAI_API_KEY` to provision, and headless-friendly (approve on any
device; ideal for pods via `kubectl exec`):

```bash
openab-agent auth xai
```

Prints a user code and an `https://auth.x.ai/...` verification link (prefilled
when the server provides one). Tokens are stored under the `xai-oauth` namespace
in `auth.json` and refreshed automatically. The default auth file is
`$HOME/.openab/agent/auth.json`; run `openab-agent auth status` to list each
stored provider, a masked token, and its expiry without printing secrets.

Select xAI explicitly with `OPENAB_AGENT_PROVIDER=xai` (or `grok`) or a
`xai/`- or `grok/`-prefixed model, for example:

```bash
OPENAB_AGENT_PROVIDER=xai openab-agent
# or use model/config selection:
OPENAB_AGENT_MODEL=xai/grok-4.5 openab-agent
```

A config file can make that selection persistent:

```json
{
  "model": "xai/grok-4.5",
  "max_tokens": 8192
}
```

xAI is not part of auto-detection: an `xai-oauth` entry by itself does not make
the agent choose xAI. `OPENAB_AGENT_XAI_MODEL` controls the model after xAI has
been selected; it does not enable xAI selection on its own. The OAuth token and
refresh token stay in `auth.json`, never in `config.json`.

### Adding an OAuth vendor

Subscription-OAuth providers are declared as a single `OAuthVendor` descriptor
(`auth.rs`, ADR §5.1) — namespace, client id, authorize/token URLs, redirect,
scope, token-body encoding. The shared PKCE/device/refresh driver reads the
descriptor, so a new vendor is a new descriptor, not a new hand-rolled flow.

## Custom System Prompt

Place an `AGENTS.md` file in the working directory (`cwd`). It will be prepended to the default system prompt at session creation.

```
/home/agent/
├── AGENTS.md        ← read at session start
└── .openab/
    └── agent/
        ├── config.json  ← optional model/provider defaults
        ├── auth.json    ← OAuth credentials; permissions should remain private
        └── skills/      ← skill directories
            └── my-skill/
                └── SKILL.md
```

## Skills

openab-agent supports on-demand skills following the [Agent Skills standard](https://agentskills.io). Skills are directories containing a `SKILL.md` with YAML frontmatter.

### Skill Locations

Scanned in order (first occurrence of a name wins):

1. `<working_dir>/.openab/skills/` — project-local skills
2. `~/.openab/agent/skills/` — global skills

### SKILL.md Format

```markdown
---
name: my-skill
description: What this skill does and when to use it
---

# Instructions

Steps the agent should follow when using this skill.
```

### How It Works

1. At session start, openab-agent scans skill directories
2. Skill names and descriptions are injected into the system prompt
3. When a task matches, the agent uses `read` to load the full SKILL.md
4. The agent follows the instructions using its built-in tools (bash, read, write, edit)

### Example

```
.openab/skills/
└── brave-search/
    ├── SKILL.md
    └── search.sh
```

```markdown
---
name: brave-search
description: Web search via Brave Search API. Use when the user needs current information from the web.
---

# Brave Search

## Usage

\`\`\`bash
./search.sh "query"
\`\`\`
```

### Compatibility

Skills written for Pi (`~/.pi/agent/skills/`) or Claude Code (`~/.claude/skills/`) use the same SKILL.md format. Copy or symlink them into `~/.openab/agent/skills/` to reuse.

## Docker

```bash
docker build -f Dockerfile.native -t openab-native:latest .
```

Image is ~20MB (debian-slim + static Rust binaries). No runtime dependencies.

## Memory Usage

~7MB per session — 28x lighter than Pi, 55x lighter than Kiro CLI.

## Supported Models (ChatGPT Subscription)

- `gpt-5.2`
- `gpt-5.3-codex`
- `gpt-5.3-codex-spark`
- `gpt-5.4`
- `gpt-5.4-mini`
- `gpt-5.5`

## Tools

4 built-in tools:
- `read` — file contents or directory listing
- `write` — create/overwrite file
- `edit` — string replacement
- `bash` — shell execution with process group isolation
