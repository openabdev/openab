# Agent-Installable Tools

## How to Install Extra Tools

You don't need to read this doc yourself. Just ask your agent:

```
per docs/* from OpenAB GitHub repo, how to install <TOOL_NAME> for my OAB agent
```

Your agent will query the relevant docs under `docs/`, find the recommended approach, and guide you through the entire installation — or just do it for you. That's it. One prompt, done.

## How It Works

```
  Human                        Agent                         OpenAB Pod
  ┌─────────────────┐         ┌──────────────────┐          ┌──────────────────────────┐
  │                  │         │                  │          │  Container (read-only)   │
  │ "install glab   │────────►│ reads docs/*     │          │  ┌────────────────────┐  │
  │  for my OAB     │         │ from OpenAB repo │          │  │ curl, gh, rg, tini │  │
  │  agent"         │         │                  │          │  │ (built-in, minimal)│  │
  │                  │         │ finds install    │          │  └────────────────────┘  │
  │                  │         │ steps for glab   │          │                          │
  │                  │         │                  │          │  PVC (persistent ~/):    │
  │                  │         │ executes:        │          │  ┌────────────────────┐  │
  │                  │         │  curl ─► extract │─────────►│  │ ~/bin/             │  │
  │                  │         │  ─► ~/bin/glab   │          │  │  ├── glab    ✅ new│  │
  │                  │         │  ─► verify       │          │  │  ├── aws          │  │
  │                  │         │                  │          │  │  ├── ssh          │  │
  │  "done! glab    │◄────────│ "glab v1.46      │          │  │  ├── terraform    │  │
  │   ready to use" │         │  installed ✅"    │          │  │  └── kubectl      │  │
  │                  │         │                  │          │  │                    │  │
  └─────────────────┘         └──────────────────┘          │  │ ~/.ssh/  ~/.config/│  │
                                                            │  │ ~/.kiro/ ~/aws-cli/│  │
                                                            │  └────────────────────┘  │
                                                            └──────────────────────────┘

  ┌─────────────────────────────────────────────────────────────────────────────┐
  │  Migration: move PVC to new node / cluster / cloud                         │
  │                                                                            │
  │  Old Cluster              PVC                    New Cluster               │
  │  ┌──────────┐     ┌────────────────┐     ┌──────────────┐                 │
  │  │ Pod ──────┼────►│ ~/bin/         │────►│ New Pod      │                 │
  │  │ (delete)  │     │ ~/.ssh/        │     │ (attach PVC) │                 │
  │  └──────────┘     │ ~/.config/     │     │              │                 │
  │                    │ ~/.kiro/       │     │ Everything   │                 │
  │                    │ ~/aws-cli/     │     │ just works™  │                 │
  │                    └────────────────┘     └──────────────┘                 │
  │                                                                            │
  │  Zero reinstallation. All tools, configs, keys, and agent memory persist. │
  └─────────────────────────────────────────────────────────────────────────────┘
```

## Why This Pattern

OpenAB keeps its Docker image minimal — only the essentials ship in the Dockerfile. Everything else is installed **at runtime by the agent** into the home directory (`~/bin/`). This is a deliberate design choice:

- **Lean image, infinite extensibility** — the Dockerfile never grows. Need AWS CLI today, Terraform tomorrow, glab next week? Same image, same pattern. No rebuild, no redeploy.
- **Doc-driven, AI-first** — documentation is written for agents to consume. Humans just say what they need; the agent reads the docs and executes.
- **No gatekeeping** — adding a new tool doesn't require a PR to the Dockerfile, a new Docker build, or a Helm upgrade. Any agent can install any tool at any time.
- **Full persistence on PVC** — everything installed to `~/bin/` and `~/` lives on the Persistent Volume Claim. This means:
  - **Pod restart** — tools are still there
  - **Helm upgrade** — tools are still there
  - **Migrate the PVC to a new node / new cluster** — tools, configs, credentials, SSH keys — everything moves with it. Your agent's entire environment is portable.
  - **Upgrade a tool** — just re-run the install. The old binary is overwritten in place.
- **No Dockerfile sprawl** — if we baked GitLab CLI into the image, we'd have no reason to reject AWS CLI, gcloud, azure CLI, wrangler, kubectl, terraform... The image would bloat endlessly. This pattern keeps the core small and lets each deployment customize itself.

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

## Persistence & Portability

Everything under `~/` is mounted on a PVC — see the migration diagram above. Key directories that persist:

```
~/bin/           → all installed tool binaries
~/aws-cli/       → AWS CLI installation
~/npm-global/    → npm-installed tools (wrangler, etc.)
~/.ssh/          → SSH keys and config
~/.config/       → tool configs (glab, wrangler, etc.)
~/.kiro/         → agent steering docs and memory
```

The only time tools are lost is if the PVC itself is deleted.

## Adding a New Tool Doc

If you're contributing a doc for a new tool (e.g., `docs/gitlab.md`, `docs/cloudflare.md`):

1. **Keep it short** — provide the install commands with architecture detection and a verification step
2. **Reference this doc** — link back here for the general pattern and philosophy
3. **Test the prompt** — verify that asking your agent _"per docs/your-tool.md from OpenAB repo, install X for me"_ actually works end-to-end
