# Local Development

> ⚠️ **Security disclaimer:** OpenAB is designed to run in Kubernetes with Pod-level isolation (NetworkPolicy, resource limits, read-only root filesystem). Native binaries — including Windows builds — are provided for **temporary local development and debugging only**, not for daily use. Running directly on a host without container isolation is **not recommended for production** — the agent subprocess inherits full host permissions and is not protected by any sandbox, which is a meaningful attack surface for a bot accepting messages from arbitrary users.

```bash
cp config.toml.example config.toml
# Edit config.toml with your bot token and channel ID

export DISCORD_BOT_TOKEN="your-token"
cargo run
```

## Remote Config

Config can be loaded from a local file or a remote URL via the `--config` / `-c` flag:

```bash
# Local file
openab run --config config.toml
openab run -c config.toml

# Remote URL (http:// or https://)
openab run --config https://example.com/config.toml
openab run -c https://example.com/config.toml

# Default (no flag → config.toml)
openab run
```

This is useful for containerized or multi-node deployments where config is hosted on a central server (e.g. S3, Git raw URL, internal HTTP service).

> **Security best practice:** Never hardcode secrets in remote config files. Use environment variable references like `bot_token = "${DISCORD_BOT_TOKEN}"` and inject the actual values via local environment variables or Kubernetes Secrets. OpenAB expands `${VAR}` identically for both local and remote config.

## Build & Push

```bash
docker build -t openab:latest .
docker tag openab:latest <your-registry>/openab:latest
docker push <your-registry>/openab:latest
```

## Project Structure

```
├── Dockerfile          # multi-stage: rust build + debian-slim runtime with kiro-cli
├── config.toml.example # example config with all agent backends
├── k8s/                # Kubernetes manifests
└── src/
    ├── main.rs         # entrypoint: multi-adapter startup, cleanup, shutdown
    ├── adapter.rs      # ChatAdapter trait, AdapterRouter (platform-agnostic)
    ├── config.rs       # TOML config + ${ENV_VAR} expansion
    ├── discord.rs      # DiscordAdapter: serenity EventHandler + ChatAdapter impl
    ├── slack.rs        # SlackAdapter: Socket Mode + ChatAdapter impl
    ├── media.rs        # shared image resize/compress + STT download
    ├── format.rs       # message splitting, thread name shortening
    ├── reactions.rs    # status reaction controller (debounce, stall detection)
    └── acp/
        ├── protocol.rs # JSON-RPC types + ACP event classification
        ├── connection.rs # spawn CLI, stdio JSON-RPC communication
        └── pool.rs     # session key → AcpConnection map
```
