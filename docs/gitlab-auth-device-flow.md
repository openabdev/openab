# GitLab CLI Authentication in Agent Environments

How to authenticate `glab` (GitLab CLI) when the agent runs in a headless container and the user may be on mobile.

## Why `glab` auth matters

`glab` is one of the most common tools agents use to interact with GitLab — reviewing MRs, creating issues, commenting, approving, merging, etc. Before the agent can do any of this, `glab` must be authenticated.

## Challenges

This isn't a typical `glab auth login` scenario. Three things make it tricky:

1. **The agent runs in a K8s pod with no browser** — `glab auth login` can't open a browser in a headless environment, so device flow (code + URL) is the only option
2. **The user might be on mobile, not at a desktop** — they're chatting via Discord on their phone, so the agent must send the URL and code as a clickable message
3. **The user authorizes on their phone** — they tap the link, enter the code in mobile Safari/Chrome, and the agent's background process picks up the token automatically

```
┌───────────┐  "review MR #108"  ┌───────────┐  glab mr view  ┌───────────┐
│  Discord   │──────────────────►│  OpenAB    │────────────►│  GitLab   │
│  User      │                   │  + Agent   │◄────────────│  API      │
└───────────┘                    └─────┬─────┘  401 🚫      └───────────┘
                                       │
                                       │ needs glab auth login first!
                                       ▼
                                 ┌───────────┐  device flow  ┌───────────┐
                                 │  Agent     │─────────────►│  GitLab   │
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

`glab auth login` is interactive: it prompts for hostname, token, and protocol. In an agent environment the shell is synchronous — it blocks until the command finishes:

| Approach | What happens |
|---|---|
| Run directly | Blocks forever. User never sees the prompt. |
| `timeout N glab auth login` | Prompt appears only after timeout kills the process — token is never saved. |
| Piping input | Works but requires pre-generating token, defeating the purpose of device flow. |

## Solution: `nohup` + background + read log + stdin automation

For GitLab's interactive auth, use a combination of `nohup` and automated input:

```bash
nohup bash -c 'echo -e "gitlab.com\nhttps\n" | glab auth login' > /tmp/glab-login.log 2>&1 &
sleep 3 && cat /tmp/glab-login.log
```

How it works:
1. `nohup ... &` runs `glab` in the background so the shell returns immediately
2. `echo -e "gitlab.com\nhttps\n" |` pre-answers the hostname and protocol prompts
3. `sleep 3 && cat` reads the log after `glab` has printed the auth prompt
4. The agent sends the auth prompt/URL to the user (via Discord)
5. The user opens the link (even on mobile), authorizes the application
6. `glab` detects the authorization and saves the token
7. Done — `glab auth status` confirms login

## Alternative: Pre-generated Personal Access Token

If device flow is not feasible, you can use a pre-generated personal access token:

```bash
glab auth login --hostname gitlab.com --token <YOUR_GITLAB_TOKEN> --protocol https
```

This is simpler but requires the token to be available beforehand (see [GitLab Token Setup](gitlab-token-setup.md)).

## Verify

```bash
glab auth status
```

Should output:
```
✓ Logged in to gitlab.com as your-username
```

## Steering / prompt snippet (Kiro CLI only)

> **Note:** This section applies only to [Kiro CLI](https://kiro.dev) agents. Other agent backends (Claude Code, Codex, Gemini) have their own prompt/config mechanisms.

To make your Kiro agent always handle `glab login` correctly, create `~/.kiro/steering/glab.md`:

```bash
mkdir -p ~/.kiro/steering
cat > ~/.kiro/steering/glab.md << 'EOF'
# GitLab CLI

## Device Flow Login

When asked to "glab login" or "glab auth login", always use nohup + background + read log:

```bash
nohup bash -c 'echo -e "gitlab.com\nhttps\n" | glab auth login' > /tmp/glab-login.log 2>&1 &
sleep 3 && cat /tmp/glab-login.log
```

Never use `timeout`. The shell tool is synchronous — it blocks until the command finishes, so stdout won't be visible until then. `nohup` runs it in the background, `sleep 3 && cat` grabs the prompt immediately.

If a personal access token is available, use direct login instead:

```bash
glab auth login --hostname gitlab.com --token $GITLAB_TOKEN --protocol https
```
EOF
```

Kiro CLI automatically picks up `~/.kiro/steering/*.md` files as persistent context, so the agent will remember this across all sessions.

## Troubleshooting

- **`glab auth status` fails** — check that authentication was completed: `glab auth status --hostname gitlab.com`
- **"Invalid credentials"** — ensure the token or authorization is valid on GitLab
- **Timeout during login** — increase the `sleep` duration if the GitLab auth server is slow
- **Multiple GitLab instances** — use `--hostname` flag to specify custom GitLab instances (e.g., `gitlab.company.com`)
