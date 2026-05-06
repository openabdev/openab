# Debugging and Reproducing Issues via Remote SSH

This document describes how an OpenAB agent running inside a pod can debug, troubleshoot, and reproduce GitHub issues against a remote host over SSH — and validate proposed fixes before posting results.

## Why This Matters

An AI agent that only reads code and speculates about bugs is limited. By giving the agent SSH access to a real environment, it can:

- **Reproduce issues end-to-end** — deploy, configure, and observe the actual failure instead of guessing from source code
- **Validate fixes before posting** — confirm the proposed solution works, not just that it looks correct
- **Provide evidence-based triage** — post reproduction logs and fix verification to GitHub, giving maintainers confidence to act

### Use Cases

- **Helm / Kubernetes issues** — install a chart, inspect rendered manifests, check pod logs
- **Configuration bugs** — apply a config, start a service, verify behavior
- **Infrastructure issues** — test networking, DNS, storage, permissions on a real host
- **Documentation gaps** — follow documented steps exactly and confirm they work (or don't)
- **Regression testing** — deploy a specific version, trigger the reported bug, upgrade, confirm it's fixed

## ASCII Flow

```text
┌─────────────────────────────────────────────────────────────┐
│  Discord / Slack                                            │
│                                                             │
│  Maintainer: "validate issue #N"                            │
│       │                                                     │
│       ▼                                                     │
│  ┌──────────┐                                               │
│  │ OAB Bot  │  (OpenAB agent)                               │
│  └────┬─────┘                                               │
└───────┼─────────────────────────────────────────────────────┘
        │
        ▼
┌───────────────────────────────────────────┐
│  OpenAB Pod                               │
│                                           │
│  ┌─────────────────────────────────────┐  │
│  │  Agent Runtime                      │  │
│  │                                     │  │
│  │  1. gh issue view <N>               │  │
│  │  2. gh api → read source on main    │  │
│  │  3. ssh k8s "<reproduce>"           │──┼──── SSH ────┐
│  │  4. ssh k8s "<inspect logs/state>"  │──┼──── SSH ────┤
│  │  5. ssh k8s "<apply fix>"           │──┼──── SSH ────┤
│  │  6. gh issue comment <N>            │  │             │
│  │                                     │  │             │
│  │  Tools: gh, ssh (~/bin/), curl      │  │             │
│  └─────────────────────────────────────┘  │             │
│                                           │             │
│  No sudo · read-only rootfs               │             │
│  SSH installed via dpkg-deb extract       │             │
└───────────────────────────────────────────┘             │
                                                          │
                                                          ▼
                                            ┌─────────────────────────┐
                                            │  Remote Host            │
                                            │                         │
                                            │  Any environment:       │
                                            │  • Kubernetes cluster   │
                                            │  • Docker host          │
                                            │  • Bare metal / VM      │
                                            │                         │
                                            │  Agent reproduces the   │
                                            │  issue here, then       │
                                            │  validates the fix.     │
                                            └─────────────────────────┘

Flow:
  ① Maintainer triggers triage via chat message
  ② Agent reads issue + relevant source code via GitHub API
  ③ Agent SSHs into remote host to reproduce the reported problem
  ④ Agent inspects logs, config, or state to confirm the error
  ⑤ Agent applies the proposed fix and verifies it resolves the issue
  ⑥ Agent posts structured results back to the GitHub issue
```

## Why This Pattern

- **The agent pod is sandboxed.** It cannot run Kubernetes, Docker, or other infrastructure locally. SSH to a remote host gives the agent access to a real environment without requiring privileged containers.
- **GitHub API avoids cloning.** The agent reads issue details, source code, and config files via `gh api` / `gh issue view`, then posts results with `gh issue comment`. No local clone needed for triage.
- **Reproducibility over speculation.** The agent follows the exact steps a user would — deploy, configure, observe the error, apply the fix. This catches real-world gaps that code review alone misses.

## Prerequisites

### SSH on the Agent Pod

The agent pod typically has no `sudo` and a read-only root filesystem. Install OpenSSH by extracting the official Debian deb package.

Find the current `.deb` URL at https://packages.debian.org/bookworm/amd64/openssh-client/download, then:

```bash
mkdir -p ~/bin /tmp/ssh-extract
curl -fsSL -o /tmp/openssh-client.deb "<url from the download page>"
dpkg-deb -x /tmp/openssh-client.deb /tmp/ssh-extract
cp /tmp/ssh-extract/usr/bin/{ssh,ssh-keygen,scp} ~/bin/
chmod +x ~/bin/{ssh,ssh-keygen,scp}
export PATH="$HOME/bin:$PATH"
```

### SSH Key and Config

Generate a key pair and configure the remote host:

```bash
ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519 -N ""
```

Add the public key to the remote host's `~/.ssh/authorized_keys`, then create `~/.ssh/config`:

```
Host k8s
    HostName <remote-ip>
    User <user>
    IdentityFile ~/.ssh/id_ed25519
```

### Remote Host

The remote host can be any environment relevant to the issue:

- A Kubernetes cluster (any distribution) with `kubectl` and `helm`
- A Docker host
- A bare VM or EC2 instance

The only requirement is that the SSH user has the permissions needed to reproduce the issue.

## Triage Steps

### 1. Read the Issue

```bash
gh issue view <N> --repo <org>/<repo>
```

Understand the claim: what's broken, what error is reported, what fix is proposed.

### 2. Verify on Main

Read the relevant source code via GitHub API to confirm the claim before touching the remote host:

```bash
gh api repos/<org>/<repo>/contents/<path> --jq '.content' | base64 -d
```

This step often reveals the root cause without deploying anything.

### 3. Reproduce

SSH into the remote host and follow the reporter's steps (or the documented steps) to trigger the error:

```bash
ssh k8s "<commands to reproduce the issue>"
```

Capture the error output — logs, config state, exit codes — as evidence.

### 4. Validate the Fix

Apply the proposed fix on the same host and confirm the error is resolved:

```bash
ssh k8s "<commands to apply the fix>"
ssh k8s "<commands to verify the fix worked>"
```

Capture the post-fix output as evidence.

### 5. Post Results to GitHub

Post a structured comment with the verdict up front and details collapsed:

```bash
gh issue comment <N> --repo <org>/<repo> --body '
## ✅ Bug validated — proposed fix verified

### Bug confirmed
<one paragraph: what was tested, what error occurred, root cause>

### Proposed fix (validated)
<diff or command showing the fix + post-fix output>

<details>
<summary>Full reproduction steps</summary>
<step-by-step commands and outputs>
</details>
'
```

### 6. Clean Up

Remove any test resources created on the remote host during reproduction.
