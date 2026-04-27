# WhatsApp Adapter

Connect your OAB agent to WhatsApp using the [Baileys](https://github.com/WhiskeySockets/Baileys) library. Messages flow through a thin Node.js subprocess bridge — no separate service needed.

## Architecture

```
OAB (Rust)
  └── WhatsAppAdapter
        └── spawn: node baileys-bridge.js
            ├── Baileys ←WebSocket→ WhatsApp servers
            └── ←stdin/stdout JSON→ Rust adapter
```

## Prerequisites

- Node.js 18+ (already present in OAB Docker images except kiro)
- A WhatsApp account (personal or business) on your phone

## Setup

### 1. Install bridge dependencies

```bash
cd whatsapp && npm install
```

This generates `package-lock.json` for reproducible builds. Commit it to your repo if you fork.

### 2. Add to config.toml

```toml
[whatsapp]
# bridge_script = "whatsapp/baileys-bridge.js"  # default
# session_dir = "whatsapp/.whatsapp-session"     # default: next to bridge script
# allowed_contacts = []                          # empty = allow all
```

### 3. Start OAB

```bash
openab run
```

On first launch, the bridge will print a QR code in the logs. Scan it with your phone:

**WhatsApp → Settings → Linked Devices → Link a Device**

The session is persisted to `session_dir` — you only need to scan once.

## Docker

Use the sample `Dockerfile.whatsapp` to build a WhatsApp-enabled image:

```bash
docker build -f Dockerfile.whatsapp \
  --build-arg BASE_IMAGE=ghcr.io/openabdev/openab-claude:latest \
  -t openab-whatsapp .
```

Or with volume mount (no custom image needed):

```bash
# Prepare bridge on host
cd whatsapp && npm install && cd ..

# Mount into container
docker run -v ./whatsapp:/home/node/whatsapp \
  -v ./config.toml:/etc/openab/config.toml \
  ghcr.io/openabdev/openab-claude:latest
```

## Configuration

| Key | Default | Description |
|-----|---------|-------------|
| `bridge_script` | `whatsapp/baileys-bridge.js` | Path to the Node.js bridge script |
| `session_dir` | (next to bridge script) | Directory for Baileys session persistence |
| `allowed_contacts` | `[]` (allow all) | WhatsApp JID allowlist, e.g. `["628123456789@s.whatsapp.net"]` |

## Contact Allowlist

When `allowed_contacts` is empty, the bot responds to everyone. To restrict access:

```toml
[whatsapp]
allowed_contacts = [
  "628123456789@s.whatsapp.net",   # individual contact
  "120363012345@g.us",              # entire group
]
```

- Individual contacts use `@s.whatsapp.net` suffix
- Groups use `@g.us` suffix
- In groups, the sender's individual JID is checked (not just the group JID)

## Limitations

- **No threads** — WhatsApp doesn't have threads; all messages go to the same chat
- **No reactions** — Baileys supports them but not yet implemented (follow-up PR)
- **No media** — Images, audio, documents are not yet processed (follow-up PR)
- **No streaming** — Messages are sent once (no edit-streaming like Discord)
- **Account risk** — Baileys uses the unofficial WhatsApp Web protocol; high-volume automation may trigger account restrictions from Meta

## Troubleshooting

### QR code not appearing

Check that Node.js is installed and the bridge script exists:
```bash
node --version
ls whatsapp/baileys-bridge.js
```

### Session expired / logged out

Delete the session directory and restart to re-scan:
```bash
rm -rf whatsapp/.whatsapp-session
openab run
```

### Bridge keeps reconnecting

Check the OAB logs for `baileys` entries. Common causes:
- Another WhatsApp Web session is competing (unlink old devices)
- Network connectivity issues
- WhatsApp account restrictions
