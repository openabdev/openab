# Agent-Installable Tools

> **Audience:** AI agents (not humans). Feed this doc to your coding agent when it needs to install additional tools inside an OpenAB pod.

## Philosophy

OpenAB keeps its Docker image minimal — only the essentials ship in the Dockerfile. Everything else (AWS CLI, GitLab CLI, OpenSSH, language runtimes, etc.) is installed **at runtime by the agent** into `~/bin/`. This means:

- **No Dockerfile changes** — the image stays lean and universal
- **Persistent across restarts** — `~/bin/` lives on the PVC, so tools survive pod restarts
- **One prompt to install** — tell your agent _"per docs/agent-installable-tools.md, install X for me"_ and it handles the rest
- **Any tool, same pattern** — AWS CLI, glab, wrangler, terraform, kubectl — all follow the same flow

## What Ships in the Image

| Tool | Why it's built-in |
|------|-------------------|
| `curl`, `unzip` | Bootstrap — needed to download everything else |
| `gh` (GitHub CLI) | Core workflow — OpenAB repos are on GitHub; agents push reviews and PRs |
| `ripgrep` | Core workflow — fast code search inside the pod |
| `procps`, `tini` | Runtime — healthcheck and process management |
| `kiro-cli` | Core workflow — the coding agent runtime |

Everything else is **agent-installable**.

## Installation Pattern

All tools follow the same steps:

```
1. Download the prebuilt binary (or .deb / .tar.gz)
2. Extract to ~/bin/
3. Verify it works
```

### Generic Template

```bash
mkdir -p ~/bin
# Download
curl -fsSL -o /tmp/tool.tar.gz "<release-url>"
# Extract (adapt to archive format)
tar -xzf /tmp/tool.tar.gz -C /tmp/tool-extract
cp /tmp/tool-extract/<binary> ~/bin/
chmod +x ~/bin/<binary>
# Verify
export PATH="$HOME/bin:$PATH"
<binary> --version
# Clean up
rm -rf /tmp/tool.tar.gz /tmp/tool-extract
```

### For `.deb` Packages (no sudo needed)

```bash
mkdir -p ~/bin /tmp/deb-extract
curl -fsSL -o /tmp/package.deb "<deb-url>"
dpkg-deb -x /tmp/package.deb /tmp/deb-extract
cp /tmp/deb-extract/usr/bin/<binary> ~/bin/
chmod +x ~/bin/<binary>
export PATH="$HOME/bin:$PATH"
<binary> --version
rm -rf /tmp/package.deb /tmp/deb-extract
```

## Common Tools

| Tool | Type | Install |
|------|------|---------|
| **OpenSSH** (`ssh`, `scp`, `ssh-keygen`) | `.deb` extract | See [remote-ssh-debugging.md](refarch/remote-ssh-debugging.md) |
| **AWS CLI v2** (`aws`) | Installer | [Install steps](#aws-cli-v2) |
| **GitLab CLI** (`glab`) | `.tar.gz` | [Install steps](#gitlab-cli-glab) |
| **Cloudflare Wrangler** (`wrangler`) | `npm` | [Install steps](#cloudflare-wrangler) |
| **Terraform** (`terraform`) | `.zip` | [Install steps](#terraform) |
| **kubectl** | Binary | [Install steps](#kubectl) |

> All examples below auto-detect architecture (ARM64 / AMD64).

## Install Steps

### OpenSSH Client

Full guide with SSH key setup and remote host config: [docs/refarch/remote-ssh-debugging.md](refarch/remote-ssh-debugging.md)

Quick install:

```bash
mkdir -p ~/bin /tmp/ssh-extract
# Find current .deb URL at: https://packages.debian.org/bookworm/amd64/openssh-client/download
curl -fsSL -o /tmp/openssh-client.deb "<url from download page>"
dpkg-deb -x /tmp/openssh-client.deb /tmp/ssh-extract
cp /tmp/ssh-extract/usr/bin/{ssh,ssh-keygen,scp} ~/bin/
chmod +x ~/bin/{ssh,ssh-keygen,scp}
export PATH="$HOME/bin:$PATH"
ssh -V
rm -rf /tmp/openssh-client.deb /tmp/ssh-extract
```

### AWS CLI v2

```bash
mkdir -p ~/bin /tmp/aws-extract
ARCH=$(uname -m)
curl -fsSL -o /tmp/awscli.zip \
  "https://awscli.amazonaws.com/awscli-exe-linux-${ARCH}.zip"
unzip -q /tmp/awscli.zip -d /tmp/aws-extract
/tmp/aws-extract/aws/install --install-dir ~/aws-cli --bin-dir ~/bin
export PATH="$HOME/bin:$PATH"
aws --version
rm -rf /tmp/awscli.zip /tmp/aws-extract
```

### GitLab CLI (glab)

```bash
mkdir -p ~/bin /tmp/glab-extract
ARCH=$(uname -m)
if [ "$ARCH" = "aarch64" ]; then ARCH="arm64"; elif [ "$ARCH" = "x86_64" ]; then ARCH="amd64"; fi
GLAB_VERSION=$(curl -fsSL https://gitlab.com/api/v4/projects/34675721/releases | grep -o '"tag_name":"v[^"]*"' | head -1 | grep -o '[0-9][0-9.]*')
curl -fsSL -o /tmp/glab.tar.gz \
  "https://gitlab.com/gitlab-org/cli/-/releases/v${GLAB_VERSION}/downloads/glab_${GLAB_VERSION}_linux_${ARCH}.tar.gz"
tar -xzf /tmp/glab.tar.gz -C /tmp/glab-extract
cp /tmp/glab-extract/bin/glab ~/bin/
chmod +x ~/bin/glab
export PATH="$HOME/bin:$PATH"
glab --version
rm -rf /tmp/glab.tar.gz /tmp/glab-extract
```

### Cloudflare Wrangler

```bash
mkdir -p ~/bin
npm install -g wrangler --prefix ~/npm-global
ln -sf ~/npm-global/bin/wrangler ~/bin/wrangler
export PATH="$HOME/bin:$PATH"
wrangler --version
```

> Requires Node.js. If Node.js is not available, install it first with the [generic template](#generic-template) using the [official prebuilt binaries](https://nodejs.org/en/download).

### Terraform

```bash
mkdir -p ~/bin
ARCH=$(uname -m)
if [ "$ARCH" = "aarch64" ]; then ARCH="arm64"; elif [ "$ARCH" = "x86_64" ]; then ARCH="amd64"; fi
TF_VERSION=$(curl -fsSL https://checkpoint-api.hashicorp.com/v1/check/terraform | grep -o '"current_version":"[^"]*"' | grep -o '[0-9][0-9.]*')
curl -fsSL -o /tmp/terraform.zip \
  "https://releases.hashicorp.com/terraform/${TF_VERSION}/terraform_${TF_VERSION}_linux_${ARCH}.zip"
unzip -o /tmp/terraform.zip -d ~/bin
chmod +x ~/bin/terraform
export PATH="$HOME/bin:$PATH"
terraform --version
rm -f /tmp/terraform.zip
```

### kubectl

```bash
mkdir -p ~/bin
ARCH=$(uname -m)
if [ "$ARCH" = "aarch64" ]; then ARCH="arm64"; elif [ "$ARCH" = "x86_64" ]; then ARCH="amd64"; fi
KUBECTL_VERSION=$(curl -fsSL https://dl.k8s.io/release/stable.txt)
curl -fsSL -o ~/bin/kubectl \
  "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${ARCH}/kubectl"
chmod +x ~/bin/kubectl
export PATH="$HOME/bin:$PATH"
kubectl version --client
```

## Persistence

`~/bin/` is part of the agent's home directory, which is mounted on a PVC. This means:

- **Pod restart** — tools are still there
- **Helm upgrade** — tools are still there (PVC is retained)
- **New version of a tool** — just re-run the install commands to overwrite

The only time tools are lost is if the PVC is deleted.

## Adding a New Tool Doc

If you're contributing a doc for a new tool (e.g., `docs/gitlab.md`, `docs/cloudflare.md`), follow this structure:

1. **Reference this doc** — link back to `docs/agent-installable-tools.md` for the general pattern
2. **Provide the exact install commands** — copy-paste ready, with architecture detection
3. **Include a verification step** — `<tool> --version` or equivalent
4. **Keep it short** — the general pattern is already documented here; your doc only needs the tool-specific bits

## Prompt Pattern

Users should be able to install any documented tool with a single prompt:

```
per docs/agent-installable-tools.md from OpenAB repo, install <tool> for me
```

Or for a specific tool doc:

```
per docs/gitlab.md from OpenAB repo, install GitLab CLI for me
```

The agent reads the doc, follows the steps, and the tool is ready to use.
