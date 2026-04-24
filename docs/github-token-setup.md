# GitHub Token Setup for Agents

Step-by-step guide to give your agent secure access to GitHub via `gh` CLI.

## Overview

Agents sometimes need to interact with GitHub — push branches, open PRs, comment on issues. The recommended approach is to store a GitHub fine-grained personal access token in a Kubernetes secret and inject it via the Helm chart's `envFrom`.

## 1. Create a Fine-Grained Personal Access Token

1. Go to [GitHub Settings → Developer settings → Personal access tokens → Fine-grained tokens](https://github.com/settings/tokens?type=beta)
2. Click **Generate new token**
3. Configure:
   - **Token name**: e.g. `openab-masami`
   - **Expiration**: set a reasonable expiry (e.g. 90 days)
   - **Repository access**: select only the repos the agent needs
   - **Permissions**:
     - Contents: Read and write (push branches)
     - Pull requests: Read and write (create/comment on PRs)
     - Issues: Read and write (comment on issues)
     - Workflows: Read and write (if the agent needs to modify workflows)
4. Click **Generate token** and copy it immediately

## 2. Store the Token in a Kubernetes Secret

Create a dedicated secret for the GitHub token:

```bash
kubectl create secret generic gh-token-secret \
  --from-literal=gh-token="<YOUR_GITHUB_TOKEN>"
```

## 3. Inject via Helm Chart

Use `envFrom` in your Helm values to inject the token as `GH_TOKEN`:

```yaml
# values.yaml
envFrom:
  - secretRef:
      name: gh-token-secret
```

> **Recommended**: Use `envFrom` with a separate secret so the token doesn't appear in shell history or Helm release metadata.

As a fallback, you can pass it directly during install — but note this exposes the token in shell history:

```bash
helm install openab openab/openab \
  --set env.GH_TOKEN="<YOUR_GITHUB_TOKEN>"
```

The `gh` CLI automatically picks up `GH_TOKEN` — no additional auth setup needed.

## 4. Install `gh` CLI in the Agent Container

Ensure `gh` is available in your Dockerfile. Note: `gh` is not in the default Debian repos — you need to add the GitHub CLI apt repository first:

```dockerfile
RUN apt-get update && apt-get install -y curl gpg && \
    curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      | gpg --dearmor -o /usr/share/keyrings/githubcli-archive-keyring.gpg && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      | tee /etc/apt/sources.list.d/github-cli.list > /dev/null && \
    apt-get update && apt-get install -y gh && \
    rm -rf /var/lib/apt/lists/*
```

See the [official install docs](https://github.com/cli/cli/blob/trunk/docs/install_linux.md) for other methods.

## 5. Verify

Once the agent pod is running:

```bash
# Check auth status
gh auth status

# Should show:
# ✓ Logged in to github.com as your-agent-user (GH_TOKEN)
```

The agent can now run `gh` commands: `gh pr create`, `gh issue comment`, `gh repo fork`, etc.

## Security Best Practices

- **Fine-grained tokens only** — avoid classic tokens; fine-grained tokens limit access to specific repos and permissions
- **Least privilege** — only grant the permissions the agent actually needs
- **Set expiration** — rotate tokens regularly; don't use non-expiring tokens
- **One token per agent** — if you run multiple agents, give each its own token with its own GitHub account
- **Never log tokens** — ensure your agent doesn't echo `$GH_TOKEN` in responses or logs
- **Dedicated GitHub account** — create a bot account (e.g. `masami-agent`) rather than using a personal account

## Troubleshooting

- **`gh auth status` fails** — check that `GH_TOKEN` env var is set: `echo ${GH_TOKEN:+exists}`
- **Permission denied on push** — the token's repo access doesn't include the target repo, or write permission is missing
- **403 on PR create** — the token needs Pull requests: Read and write permission
- **Token expired** — generate a new one and update the k8s secret
