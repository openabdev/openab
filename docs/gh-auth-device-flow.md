# GitHub CLI Authentication in Agent Environments

How to authenticate `gh` (GitHub CLI) when the agent runs in a headless container and the user may be on mobile.

## Why `gh` auth matters

`gh` is one of the most common tools agents use to interact with GitHub — reviewing PRs, creating issues, commenting, approving, merging, etc. Before the agent can do any of this, `gh` must be authenticated.

## Challenges

This isn't a typical `gh login` scenario. Three things make it tricky:

1. **The agent runs in a K8s pod with no browser** — `gh auth login --web` can't open a browser, so device flow (code + URL) is the only option
2. **The user might be on mobile, not at a desktop** — they're chatting via Discord on their phone, so the agent must send the URL and code as a clickable message
3. **The user authorizes on their phone** — they tap the link, enter the code in mobile Safari/Chrome, and the agent's background process picks up the token automatically

```
┌───────────┐  "review PR #108"  ┌───────────┐  gh pr view  ┌───────────┐
│  Discord   │──────────────────►│  OpenAB    │────────────►│  GitHub   │
│  User      │                   │  + Agent   │◄────────────│  API      │
└───────────┘                    └─────┬─────┘  401 🚫      └───────────┘
                                       │
                                       │ needs gh auth login first!
                                       ▼
                                 ┌───────────┐  device flow  ┌───────────┐
                                 │  Agent     │─────────────►│  GitHub   │
                                 │  (nohup)   │  code+URL    │  /login/  │
                                 └─────┬─────┘◄─────────────│  device   │
                                       │                     └─────┬─────┘
                                       │ sends code+URL            │
                                       ▼                           │
                                 ┌───────────┐  authorize    ┌─────▼─────┐
                                 │  Discord   │─────────────►│  Browser  │
                                 │  User      │  enters code │  (mobile) │
                                 └───────────┘               └───────────┘
```

## The problem with naive approaches

`gh auth login --web` uses device flow: it prints a one-time code + URL, then polls GitHub until the user authorizes. In an agent environment the shell is synchronous — it blocks until the command finishes:

| Approach | What happens |
|---|---|
| Run directly | Blocks forever. User never sees the code. |
| `timeout N gh auth login -w` | Code appears only after timeout kills the process — token is never saved. |

## Solution: `nohup` + background + read log

```bash
nohup gh auth login --hostname github.com --git-protocol https -p https -w > /tmp/gh-login.log 2>&1 &
sleep 3 && cat /tmp/gh-login.log
```

How it works:
1. `nohup ... &` runs `gh` in the background so the shell returns immediately
2. `sleep 3 && cat` reads the log after `gh` has printed the code + URL
3. The agent sends the code + URL to the user (via Discord)
4. The user opens the link (even on mobile), enters the code
5. `gh` detects the authorization and saves the token
6. Done — `gh auth status` confirms login

## Verify

```bash
gh auth status
```

## Steering / prompt snippet (Kiro CLI only)

> **Note:** This section applies only to [Kiro CLI](https://kiro.dev) agents. Other agent backends (Claude Code, Codex, Gemini) have their own prompt/config mechanisms.

To make your Kiro agent always handle `gh login` correctly, create `~/.kiro/steering/gh.md`:

```bash
mkdir -p ~/.kiro/steering
cat > ~/.kiro/steering/gh.md << 'EOF'
# GitHub CLI

## Device Flow Login

When asked to "gh login", always use nohup + background + read log:

```bash
nohup gh auth login --hostname github.com --git-protocol https -p https -w > /tmp/gh-login.log 2>&1 &
sleep 3 && cat /tmp/gh-login.log
```

Never use `timeout`. The shell tool is synchronous — it blocks until the command finishes, so stdout won't be visible until then. `nohup` runs it in the background, `sleep 3 && cat` grabs the code immediately.
EOF
```

Kiro CLI automatically picks up `~/.kiro/steering/*.md` files as persistent context, so the agent will remember this across all sessions.
