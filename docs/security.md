# Security Model — Shared Responsibility

OpenAB bridges messaging platforms (Discord, Slack, Telegram, LINE) to coding agent CLIs over ACP. This document defines the security boundaries and who is responsible for what.

## Responsibility Layers

```
┌──────────────────────────────────────────────────────────┐
│  Infrastructure Layer (K8s / VPC / Network)               │
│  Egress/ingress control, network isolation, pod security  │
│  → Responsibility: Platform / Infra team                  │
├──────────────────────────────────────────────────────────┤
│  OpenAB Layer (this project)                              │
│  Message routing, outbound content validation, rate limit │
│  → Responsibility: OpenAB maintainers                     │
├──────────────────────────────────────────────────────────┤
│  Agent CLI Layer (Kiro, Claude Code, Codex, Gemini, etc.) │
│  Filesystem access, tool permissions, sandbox policy      │
│  → Responsibility: User configuration + agent vendor      │
├──────────────────────────────────────────────────────────┤
│  User Behavior Layer                                      │
│  OAuth tokens, API keys, prompts, intentional actions     │
│  → Responsibility: End user                               │
└──────────────────────────────────────────────────────────┘
```

## What OpenAB Controls

**Inbound (user → agent):**
- Channel and user allowlists (`allowed_channels`, `allowed_users`)
- Bot message gating (`allow_bot_messages`, `trusted_bot_ids`)
- Bot turn limits (soft + hard caps to prevent runaway loops)
- Attachment size limits and text file caps

**Outbound (agent → chat):**
- Outbound file attachments are **opt-in** (`outbound.enabled = false` by default)
- When enabled, only files under `~/.oab/outgoing/` are permitted
- **Only image files** are accepted (validated by magic bytes: PNG, JPEG, GIF, WebP, BMP)
- Per-message and per-channel rate limiting prevents flood
- Path traversal and symlink escape blocked via `canonicalize` + `Path::starts_with`

**What OpenAB does NOT control:**
- What the agent CLI does with filesystem access (that's the agent's sandbox policy)
- What the agent sends via its own network calls (e.g. `curl`, API calls)
- How the agent's tools are configured (e.g. `--trust-all-tools`)

## Why Images Only

When an agent produces a file and sends it back through OpenAB to a chat channel, that file crosses a trust boundary — from the agent's local environment to a shared messaging platform. OpenAB is the gatekeeper at this boundary.

**Threat:** a prompt-injected agent could dump environment variables, secrets, or sensitive files into the outgoing directory and request OpenAB to deliver them to the chat channel.

**Mitigation:** OpenAB validates file content via magic bytes. Only files whose headers match known image formats are accepted. This blocks the most common exfiltration vector (text/binary dumps) while preserving the primary use case — screenshots, diagrams, charts, and generated images.

**What this does not prevent:**
- Data hidden in image metadata (EXIF, PNG tEXt chunks) or steganography
- Agent pasting secrets directly in reply text (already possible without outbound attachments)

**Why this is acceptable:** the image-only check raises the attack bar from trivial (`env > leak.txt`) to sophisticated (encoding secrets into valid image bytes). The fundamental truth: if you don't trust your agent's text output, you shouldn't trust its file output either — outbound attachments don't make the existing text-exfil risk worse.

## Non-Image Files: Recommended Pattern

For documents (PDF, Word, Excel, CSV) and other non-image artifacts, the recommended pattern is:

1. Agent uploads the file to an **external storage provider** (Google Drive, S3, SharePoint, etc.)
2. Agent returns a **signed URL with TTL** to the chat
3. User clicks the link to access the file

**Benefits:**
- File never touches the chat platform's servers — no compliance issues
- Access control is handled by the storage provider (OAuth, IAM policies)
- URLs can expire (e.g. S3 presigned URL with 60-second TTL)
- Works across all chat platforms (Discord, Slack, Telegram, LINE)

**Example with S3:**
```
Agent: "Here's your report: https://bucket.s3.amazonaws.com/report.pdf?X-Amz-Expires=60&..."
```

**Example with Google Drive:**
```
Agent: "Report uploaded: https://drive.google.com/file/d/abc123/view (shared with your org)"
```

## Enterprise Deployment

### Q: Can an agent exfiltrate data to external services?

The agent CLI runs inside a container. Network-level controls are the infrastructure team's responsibility:

- **Kubernetes:** use `NetworkPolicy` to restrict pod egress to specific CIDRs or services
- **AWS VPC:** deploy in a private subnet with no internet gateway; use VPC endpoints for allowed AWS services only
- **Service mesh:** use Istio/Linkerd egress policies to allowlist specific external domains

OpenAB provides a container-ready architecture (`Dockerfile` + Helm chart + k8s manifests) that integrates with these controls.

### Q: Can an agent send sensitive files to the chat channel?

- Outbound attachments are **disabled by default** — operators must explicitly opt in
- When enabled, only **image files** are accepted (magic bytes validation)
- Only files in `~/.oab/outgoing/` are permitted — the agent must explicitly copy files there
- Non-image documents should use the signed-URL pattern described above

### Q: How do I audit what the agent sends?

- All outbound attachment operations are logged via `tracing` at INFO/WARN level
- Accepted files: `outbound: attachment accepted path=... size=...`
- Blocked files: `outbound: not an image file`, `outbound: path not in outgoing dir`, `outbound: over size limit`
- Rate limit hits: `outbound: rate-limit hit, dropping excess`
