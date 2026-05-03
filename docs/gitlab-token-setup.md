# GitLab Token Setup for Agents

Step-by-step guide to give your agent secure access to GitLab via `glab` CLI.

## Overview

Agents sometimes need to interact with GitLab — push branches, open MRs, comment on issues. The recommended approach is to store a GitLab personal access token in a Kubernetes secret and inject it via the Helm chart's `envFrom`.

## 1. Create a Personal Access Token

### For GitLab.com

1. Go to [GitLab Settings → Access Tokens](https://gitlab.com/-/user_settings/personal_access_tokens)
2. Click **Add new token**
3. Configure:
   - **Token name**: e.g. `openab-agent`
   - **Expiration date**: set a reasonable expiry (e.g. 90 days)
   - **Scopes**: select the minimum required:
     - `api` — full API access (required for most operations)
     - `read_repository` — read repository contents
     - `write_repository` — push branches and commits
     - `read_user` — read user profile
     - `read_api` — read-only API access (if write not needed)
4. Click **Create personal access token** and copy it immediately

### For Self-Hosted GitLab

1. Go to your GitLab instance → User Settings → Access Tokens
2. Follow the same steps as above
3. Note your GitLab instance URL (e.g., `gitlab.company.com`)

## 2. Store the Token in a Kubernetes Secret

Create a dedicated secret for the GitLab token:

```bash
kubectl create secret generic gitlab-token-secret \
  --from-literal=gitlab-token="<YOUR_GITLAB_TOKEN>"
```

For self-hosted GitLab, also store the hostname:

```bash
kubectl create secret generic gitlab-token-secret \
  --from-literal=gitlab-token="<YOUR_GITLAB_TOKEN>" \
  --from-literal=gitlab-hostname="gitlab.company.com"
```

## 3. Inject via Helm Chart

Use `envFrom` in your Helm values to inject the token as `GITLAB_TOKEN`:

```yaml
# values.yaml
envFrom:
  - secretRef:
      name: gitlab-token-secret
```

> **Recommended**: Use `envFrom` with a separate secret so the token doesn't appear in shell history or Helm release metadata.

As a fallback, you can pass it directly during install — but note this exposes the token in shell history:

```bash
helm install openab openab/openab \
  --set env.GITLAB_TOKEN="<YOUR_GITLAB_TOKEN>"
```

For self-hosted GitLab, also set the hostname:

```bash
helm install openab openab/openab \
  --set env.GITLAB_TOKEN="<YOUR_GITLAB_TOKEN>" \
  --set env.GITLAB_HOSTNAME="gitlab.company.com"
```

The `glab` CLI automatically picks up `GITLAB_TOKEN` — no additional auth setup needed.

## 4. Install `glab` CLI in the Agent Container

Ensure `glab` is available in your Dockerfile. The Dockerfile includes `glab` installation by default:

```dockerfile
# Install glab CLI (GitLab) - supports amd64 and arm64
ARG GLAB_VERSION=1.93.0
RUN ARCH=$(dpkg --print-architecture) && \
    if [ "$ARCH" = "arm64" ]; then \
      GLAB_ARCH="arm64"; \
    else \
      GLAB_ARCH="amd64"; \
    fi && \
    curl -fsSL https://gitlab.com/gitlab-org/cli/-/releases/v${GLAB_VERSION}/downloads/glab_${GLAB_VERSION}_linux_${GLAB_ARCH}.deb \
      -o /tmp/glab.deb && \
    apt-get update && apt-get install -y --no-install-recommends /tmp/glab.deb && \
    rm -f /tmp/glab.deb && \
    rm -rf /var/lib/apt/lists/*
```

This installation:
- Auto-detects system architecture (amd64 or arm64)
- Downloads from the official GitLab CLI releases (gitlab.com/gitlab-org/cli)
- Supports both x86_64 and ARM64 systems
- Uses version variable for easy updates
- Cleans up temporary files to minimize image size

To update to a newer version, change the `GLAB_VERSION` argument (e.g., `1.93.0` → `1.94.0`).

See the [official glab releases](https://gitlab.com/gitlab-org/cli/-/releases) for available versions.

## 5. Verify

Once the agent pod is running:

```bash
# Check auth status
glab auth status

# Should show:
# ✓ Logged in to gitlab.com as your-agent-user (GITLAB_TOKEN)
```

For self-hosted GitLab:

```bash
glab auth status --hostname gitlab.company.com
```

The agent can now run `glab` commands: `glab mr create`, `glab issue comment`, `glab project fork`, etc.

## 6. Configure for Self-Hosted GitLab (Optional)

If using a self-hosted GitLab instance, configure `glab` to use it by default:

```bash
glab config set host gitlab.company.com
glab config set token <YOUR_GITLAB_TOKEN>
```

Or set environment variables:

```bash
export GITLAB_HOST=gitlab.company.com
export GITLAB_TOKEN=<YOUR_GITLAB_TOKEN>
```

## Security Best Practices

- **Scoped tokens only** — use personal access tokens with minimal required scopes, not OAuth tokens
- **Least privilege** — only grant the permissions the agent actually needs (e.g., `write_repository` if pushing, `api` if managing issues)
- **Set expiration** — rotate tokens regularly; don't use non-expiring tokens
- **One token per agent** — if you run multiple agents, give each its own token with its own GitLab account
- **Never log tokens** — ensure your agent doesn't echo `$GITLAB_TOKEN` in responses or logs
- **Dedicated GitLab account** — create a bot account (e.g. `openab-agent`) rather than using a personal account
- **Restrict IP access** — if your GitLab instance supports it, restrict token usage to agent pod IPs

## Common `glab` Commands

Once authenticated, the agent can use:

```bash
# Merge Requests
glab mr create --title "Title" --description "Description"
glab mr view <MR_ID>
glab mr approve <MR_ID>
glab mr merge <MR_ID>
glab mr comment <MR_ID> --message "Comment text"

# Issues
glab issue create --title "Title" --description "Description"
glab issue view <ISSUE_ID>
glab issue comment <ISSUE_ID> --message "Comment text"
glab issue close <ISSUE_ID>

# Projects
glab project fork <PROJECT_ID>
glab project clone <PROJECT_ID>
glab project list

# Pipelines
glab pipeline view <PIPELINE_ID>
glab pipeline status
```

## Troubleshooting

- **`glab auth status` fails** — check that `GITLAB_TOKEN` env var is set: `echo ${GITLAB_TOKEN:+exists}`
- **Permission denied on push** — the token's scopes don't include `write_repository`, or the account lacks write access to the target project
- **403 on MR create** — the token needs `api` scope
- **Token expired** — generate a new one and update the k8s secret
- **Self-hosted GitLab not recognized** — ensure `GITLAB_HOSTNAME` or `GITLAB_HOST` is set correctly
- **Multiple GitLab instances** — use `glab config set host <hostname>` to switch between instances

## Rotating Tokens

To rotate a token without downtime:

1. Generate a new token on GitLab
2. Update the Kubernetes secret: `kubectl patch secret gitlab-token-secret -p '{"data":{"gitlab-token":"<NEW_TOKEN_BASE64>"}}'`
3. Restart the agent pod: `kubectl rollout restart deployment/openab`
4. Revoke the old token on GitLab

