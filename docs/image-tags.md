# Docker Image Tagging Convention

## Unified Image Repository (`ghcr.io/openabdev/openab`)

All agent variants are published under a single image repository using tag-based variants.
**There is no default agent** — every image tag must explicitly specify the agent:

```
ghcr.io/openabdev/openab:<version>-<agent>
```

### Tag format

| Tag | Example | Points to | Updated when |
|-----|---------|-----------|--------------|
| `<version>-<agent>` | `0.9.0-beta.1-kiro` | Exact pre-release build | Pre-release tag pushed |
| `beta-<agent>` | `beta-claude` | Latest pre-release | Every pre-release build |
| `<version>-<agent>` | `0.9.0-codex` | Promoted stable build | Stable tag pushed |
| `<major.minor>-<agent>` | `0.9-gemini` | Latest patch in minor | Stable promotion |
| `stable-<agent>` | `stable-grok` | Latest stable | Stable promotion |

> **No `latest` tag.** Use `beta-<agent>` or `stable-<agent>` for floating tags,
> or pin to an exact version like `0.9.0-beta.1-kiro`.

Available agents: `kiro`, `claude`, `codex`, `copilot`, `cursor`, `gemini`, `grok`, `hermes`, `kimi`, `mimocode`, `opencode`, `antigravity`, `pi`, `native`, `agentcore`

### Migration from per-repo images (deprecated)

Previously, each agent had its own image repository (`ghcr.io/openabdev/openab-codex:beta`).
These are now replaced by the unified tag format.

| Old (deprecated) | New |
|------------------|-----|
| `ghcr.io/openabdev/openab:beta` | `ghcr.io/openabdev/openab:beta-kiro` |
| `ghcr.io/openabdev/openab:latest` | `ghcr.io/openabdev/openab:stable-kiro` (no more `latest`) |
| `ghcr.io/openabdev/openab-claude:beta` | `ghcr.io/openabdev/openab:beta-claude` |
| `ghcr.io/openabdev/openab-codex:0.8.5-beta.13` | `ghcr.io/openabdev/openab:0.8.5-beta.13-codex` |

## Gateway (`ghcr.io/openabdev/openab-gateway`)

| Tag | Points to | Updated when |
|-----|-----------|--------------|
| `0.5.1` | Exact release | `gateway-v*` tag pushed |
| `v0.5.1` | Same as above (v-prefixed alias) | Same |
| `latest` | Latest release | Every release |

## Which tag to use

| Use case | Recommended tag |
|----------|----------------|
| Production (pinned) | Exact version (`0.9.0-beta.1-claude`) |
| Helm chart default | `beta` or `stable` — chart auto-appends `-<agent>` |
| Local dev / quick test | `beta-<agent>` |
| CI | Exact version or `<sha>-<agent>` |

## Release flow

```
release PR merged → tag-on-merge → v0.9.0-beta.1
                                         │
                                         ▼
                                  build-operator.yml
                                         │
                              ┌──────────┴──────────┐
                              │ is_prerelease=true   │
                              ▼                      │
                    openab:0.9.0-beta.1-kiro         │
                    openab:0.9.0-beta.1-claude       │
                    openab:0.9.0-beta.1-codex        │
                    openab:beta-kiro                 │
                    openab:beta-claude               │
                    openab:beta-codex                │
                    ... (all agents)                 │
                                                    │
                              ┌──────────────────────┘
                              │ is_prerelease=false (stable)
                              ▼
                    promote most recent beta images →
                    openab:0.9.0-kiro
                    openab:0.9.0-claude
                    openab:0.9-kiro
                    openab:0.9-claude
                    openab:stable-kiro
                    openab:stable-claude
                    ... (all agents, no `latest`)
```
