# Releasing

## Version Scheme

Chart versions follow SemVer with beta pre-releases:

- **Beta**: `0.2.1-beta.12345` — auto-generated on every push to main
- **Stable**: `0.2.1` — manually triggered, visible to `helm install`

Users running `helm install` only see stable versions. Beta versions require `--devel` or explicit `--version`.

## Development Flow

```
  PR merged to main
        │
        ▼
  ┌─────────────┐     ┌──────────────────┐     ┌─────────────────────┐
  │ CI: Build   │────>│ CI: Bump PR      │────>│ Merge bump PR       │
  │ 3 images    │     │ 0.2.1-beta.12345 │     │ → publishes beta    │
  └─────────────┘     └──────────────────┘     └─────────────────────┘
                                                        │
        ┌───────────────────────────────────────────────┘
        ▼
  helm install ... --version 0.2.1-beta.12345   (explicit only)
  helm install ...                               (still gets 0.2.0 stable)
```

## Stable Release

```
  Actions → Build & Release → Run workflow
  [bump: patch] [✅ Stable release]
        │
        ▼
  ┌─────────────┐     ┌──────────────────┐     ┌─────────────────────┐
  │ CI: Build   │────>│ CI: Bump PR      │────>│ Merge bump PR       │
  │ 3 images    │     │ 0.2.1            │     │ → publishes stable  │
  └─────────────┘     └──────────────────┘     └─────────────────────┘
                                                        │
        ┌───────────────────────────────────────────────┘
        ▼
  helm install ...                               (gets 0.2.1 🎉)
```

## Image Tags

Each build produces three multi-arch images tagged with the git short SHA:

```
ghcr.io/thepagent/agent-broker:<sha>        # kiro-cli
ghcr.io/thepagent/agent-broker-codex:<sha>   # codex
ghcr.io/thepagent/agent-broker-claude:<sha>  # claude
```

The `latest` tag always points to the most recent build.
