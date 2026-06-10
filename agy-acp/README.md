# agy-acp

ACP (Agent Client Protocol) adapter for [Antigravity CLI](https://github.com/google-antigravity/antigravity-cli). Bridges `agy` into OpenAB's stdio JSON-RPC protocol.

## How it works

```
openab ──JSON-RPC──► agy-acp ──spawns──► agy -p "prompt"
                        │
                        ├─ Tracks conversation IDs via SQLite .db files
                        ├─ Extracts responses from protobuf step_payload (field 20.1)
                        └─ Persists session state for multi-turn conversations
```

## Build

```bash
cargo build --release
```

## Tests

```bash
# Unit tests
cargo test

# All tests including filesystem I/O tests
cargo test -- --include-ignored

# E2E test (requires agy in PATH + auth)
cargo test e2e -- --ignored --nocapture
```

### E2E requirements

The E2E test spawns `agy-acp` → `agy` and verifies a full round-trip prompt/response.

| Requirement | Local dev | CI |
|---|---|---|
| `agy` binary | `~/.local/bin/agy` | Downloaded from GitHub release |
| Auth | macOS Keychain (existing login) | `GEMINI_API_KEY` env var |

**Local setup:**
```bash
# Install agy v1.0.4+
gh release download 1.0.4 --repo google-antigravity/antigravity-cli \
  --pattern "agy_cli_mac_arm64.tar.gz" --dir /tmp
tar -xzf /tmp/agy_cli_mac_arm64.tar.gz -C ~/.local/bin/
ln -sf ~/.local/bin/antigravity ~/.local/bin/agy

# Run e2e
export PATH="$HOME/.local/bin:$PATH"
cargo test e2e -- --ignored --nocapture
```

**CI:** The GitHub Actions workflow (`.github/workflows/e2e-agy-acp.yml`) handles everything automatically. It uses the `GEMINI_API_KEY` repo secret.

### Updating the API key

```bash
gh secret set GEMINI_API_KEY --repo openabdev/openab
```

Get a free key from https://aistudio.google.com/apikey — the e2e sends one short prompt per run so cost is negligible.
