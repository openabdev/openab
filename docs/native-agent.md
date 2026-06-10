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

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `OPENAB_AGENT_OPENAI_MODEL` | `gpt-5.4-mini` | Model to use (must be supported by your ChatGPT plan — see [Supported Models](#supported-models-chatgpt-subscription)) |
| `OPENAB_AGENT_OPENAI_BASE_URL` | `https://chatgpt.com/backend-api` | API base URL |
| `OPENAB_AGENT_PROVIDER` | auto-detect | Force provider (`anthropic`, `openai`, `codex`) |
| `OPENAB_AGENT_MAX_TOKENS` | `8192` | Max output tokens |
| `OPENAB_AGENT_OAUTH_CLIENT_ID` | Pi's client | Custom OAuth client ID |
| `ANTHROPIC_API_KEY` | — | Anthropic API key (alternative to OAuth) |

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

### API Key (Anthropic)

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

No login needed — set the env var and the agent auto-detects it.

## Custom System Prompt

Place an `AGENTS.md` file in the working directory (`cwd`). It will be prepended to the default system prompt at session creation.

```
/home/agent/
├── AGENTS.md        ← read at session start
├── .openab/
│   └── agent/
│       └── auth.json
│   └── skills/      ← skill directories
│       └── my-skill/
│           └── SKILL.md
└── (your project files)
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
