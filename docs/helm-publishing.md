# Helm Chart Publishing

OpenAB publishes the Helm chart to two channels automatically via the `Release Charts` workflow (`.github/workflows/release.yml`).

## Channels

| Channel | URL | Install command |
|---------|-----|-----------------|
| GitHub Pages | `https://openabdev.github.io/openab` | `helm repo add openab https://openabdev.github.io/openab && helm install openab openab/openab` |
| OCI (GHCR) | `oci://ghcr.io/openabdev/charts/openab` | `helm install openab oci://ghcr.io/openabdev/charts/openab` |

## How it works

```
charts/openab/Chart.yaml changed on main
        │
        ▼
┌─────────────────────────────┐
│  Release Charts workflow    │
│  .github/workflows/         │
│  release.yml                │
│                             │
│  1. helm package            │
│  2. helm push → OCI (GHCR)  │
│  3. cr upload → GH Release  │
│  4. cr index → gh-pages     │
│  5. Update release notes    │
└─────────────────────────────┘
        │
        ▼
  Both channels updated
```

### Trigger

The workflow runs when `charts/openab/Chart.yaml` is pushed to `main`. This happens automatically when the `Build & Release` workflow merges a chart bump PR.

### OCI Registry

`helm push` publishes the packaged chart to `oci://ghcr.io/openabdev/charts`. The GHCR packages must be **public** (configured at org level) for unauthenticated pulls.

### GitHub Pages

The [`chart-releaser`](https://github.com/helm/chart-releaser) (`cr`) tool uploads the `.tgz` as a GitHub Release asset, then updates `index.yaml` on the `gh-pages` branch. GitHub Pages serves this as a standard Helm repository.

## Version flow

```
PR merged to main (src/ or Dockerfile changes)
  → Build & Release workflow
    → Builds Docker images (all 4 variants)
    → Creates chart bump PR (patch/minor/major)
    → App token merges the PR
      → Chart.yaml change triggers Release Charts
        → Publishes to OCI + GitHub Pages
```

## Stable vs beta

The `Build & Release` workflow accepts two inputs via `workflow_dispatch`:

| Input | Description |
|-------|-------------|
| `chart_bump` | `patch`, `minor`, or `major` |
| `release` | `true` for stable (e.g. `0.5.1`), omit for beta (e.g. `0.5.1-beta.34`) |

Push-triggered builds always produce beta versions. Use `workflow_dispatch` with `release=true` for stable releases.

Note: Helm hides beta versions by default. Use `--devel` to see them:

```bash
helm search repo openab/openab --devel --versions
```
