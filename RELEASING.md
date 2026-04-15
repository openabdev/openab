# Releasing

## Version Scheme

Versions follow SemVer (e.g. `0.7.0`). Version bumps are controlled via `workflow_dispatch`:

| Method | 效果 | 範例 |
|---|---|---|
| Auto patch (default) | patch bump + beta | `0.6.0 → 0.6.1-beta.1` |
| Auto minor | minor bump + beta | `0.6.0 → 0.7.0-beta.1` |
| Auto major | major bump + beta | `0.6.0 → 1.0.0-beta.1` |
| Manual | 自行指定 | `0.8.0-beta.1` or `0.8.0` |

## Release Flow (Tag-Driven)

> **核心原則：測過什麼就發什麼 (what you tested is what you ship)**
> stable release 不重新 build，直接 promote pre-release 驗證過的 image。

##### Step 1 — 建立 Release PR

```
  ┌─────────────────────────────────────────────────────────────────┐
  │ Maintainer 到 Actions → Release PR → Run workflow                │
  │                                                                  │
  │   選項 A: 留空 version，選 bump type → 自動算 (e.g. 0.7.0-beta.1) │
  │   選項 B: 手動填 version (e.g. 0.8.0-beta.1 or 0.8.0)            │
  │                                                                  │
  │ → release-pr.yml 觸發                                            │
  │ → 更新 Cargo.toml + Chart.yaml version/appVersion               │
  │ → 建立 Release PR (branch: release/v0.7.0-beta.1)                 │
  └─────────────────────────────────────────────────────────────────┘
```

##### Step 2 — Merge Release PR → 自動打 Tag → Build

```
  ┌─────────────────────────────────────────────────────────────────┐
  │ Maintainer review & merge Release PR                             │
  │                                                                  │
  │ → tag-on-merge.yml 偵測 release/ branch merge                   │
  │ → 自動打 tag (e.g. v0.7.0-beta.1)                                 │
  │ → build.yml 觸發 (is_prerelease=true)                            │
  │ → build-image:    4 variants × 2 platforms (amd64 + arm64)      │
  │ → merge-manifests: image tags = <sha> + 0.7.0-beta.1              │
  │ → release-chart:  helm chart → OCI registry                      │
  └─────────────────────────────────────────────────────────────────┘
        │
        ▼
  ┌─────────────────────────────────────────────────────────────────┐
  │ 部署 pre-release 進行測試：                                       │
  │                                                                  │
  │   helm install openab \                                          │
  │     oci://ghcr.io/openabdev/charts/openab \                      │
  │     --version 0.7.0-beta.1                                         │
  │                                                                  │
  │ 發現 bug？→ 修復 PR merge → 再跑一次 Release PR workflow          │
  │   → 手動指定 v0.7.0-beta.2 → merge → 重新測試                      │
  └─────────────────────────────────────────────────────────────────┘
```

##### Step 3 — Stable Release（Promote）

```
  ┌─────────────────────────────────────────────────────────────────┐
  │ 測試通過後，再跑一次 Release PR workflow                           │
  │ → 手動指定 version: 0.7.0 (不帶 rc)                              │
  │ → merge Release PR                                               │
  │ → tag-on-merge.yml 打 tag v0.7.0                                 │
  │                                                                  │
  │ → build.yml 觸發 (is_prerelease=false)                           │
  │ → promote-stable:                                                │
  │   1. 找到最新的 pre-release tag (v0.7.0-beta.2)                    │
  │   2. 驗證 pre-release image 存在                                  │
  │   3. re-tag 0.7.0-beta.2 → 0.7.0 / 0.7 / latest                  │
  │   ⚠️ 不 rebuild，跟 pre-release 是同一個 artifact                 │
  │ → release-chart: helm chart → OCI registry                       │
  └─────────────────────────────────────────────────────────────────┘
```

##### Step 4 — Chart Release（自動）

```
  ┌─────────────────────────────────────────────────────────────────┐
  │ release.yml 偵測到 Chart.yaml 變更 push to main                   │
  │ → chart-releaser 更新 GitHub Pages helm repo index               │
  │ → 附加 install instructions 到 chart release notes               │
  └─────────────────────────────────────────────────────────────────┘
```

## 快速指令參考

```bash
# ── Pre-release ───────────────────────────────────────
# 到 Actions → Release PR → Run workflow
# 留空 version，選 patch → 自動算 0.7.0-beta.1
# 或手動填 version: 0.7.0-beta.1
# → merge 產生的 Release PR → 自動打 tag → build

# ── 第二輪 pre-release（beta.1 有 bug 時）─────────────
# 修 bug → PR merge to main
# 再跑 Release PR workflow，手動填 version: 0.7.0-beta.2
# → merge → 自動打 tag → build

# ── Stable release ────────────────────────────────────
# 跑 Release PR workflow，手動填 version: 0.7.0
# → merge → 自動打 tag → promote beta image (不 rebuild)

# ── 手動重跑（build 失敗時）──────────────────────────
gh workflow run build.yml -f tag=v0.7.0-beta.1
gh workflow run build.yml -f tag=v0.7.0
```

## GitHub Releases

| Release | Tag 格式 | 內容 |
|---|---|---|
| chart-releaser | `openab-0.7.0` | Version Info + Installation instructions |

## Workflow 對應表

| Workflow | 觸發條件 | 用途 |
|---|---|---|
| `ci.yml` | pull_request (src/Cargo/Dockerfile) | cargo check + clippy + test |
| `release-pr.yml` | workflow_dispatch | 建立 Release PR（更新版本檔案） |
| `tag-on-merge.yml` | release/ PR merge to main | 自動打 tag |
| `build.yml` | tag push `v*` | pre-release: 完整 build / stable: promote |
| `release.yml` | Chart.yaml 變更 push to main | chart-releaser 更新 GitHub Pages index |

## Version 同步

release-pr.yml 在 Release PR 中自動更新以下檔案的版本：

| 檔案 | 欄位 |
|---|---|
| `Cargo.toml` | `version` |
| `charts/openab/Chart.yaml` | `version` |
| `charts/openab/Chart.yaml` | `appVersion` |

三者統一為同一個 semver（e.g. `0.7.0`）。

## Image Variants

每次 build 產出 6 個 multi-arch image (linux/amd64 + linux/arm64)：

```
ghcr.io/openabdev/openab          # default (kiro-cli)
ghcr.io/openabdev/openab-codex    # codex
ghcr.io/openabdev/openab-claude   # claude
ghcr.io/openabdev/openab-gemini   # gemini
ghcr.io/openabdev/openab-opencode # opencode
ghcr.io/openabdev/openab-copilot  # copilot
ghcr.io/openabdev/openab-cursor   # cursor
```

Image tags 依 release 類型不同：

| Tag | Stable (`v0.7.0`) | Pre-release (`v0.7.0-beta.1`) |
|---|---|---|
| `<sha>` | v (from pre-release) | v |
| `0.7.0` / `0.7.0-beta.1` | v | v |
| `0.7` | v | x |
| `latest` | v | x |

## Installation

##### Helm Repository (GitHub Pages)

```bash
helm repo add openab https://openabdev.github.io/openab
helm repo update
helm install openab openab/openab --version 0.7.0
```

##### OCI Registry

```bash
helm install openab oci://ghcr.io/openabdev/charts/openab --version 0.7.0
```

## 手動操作

| 時機 | 做什麼 |
|---|---|
| 準備 release | Actions → Release PR → Run workflow |
| 需要 beta 測試 | 指定 version 如 `0.7.0-beta.1` |
| 測試通過 | 指定 stable version 如 `0.7.0` → promote |
| build 失敗或需重跑 | `gh workflow run build.yml -f tag=<tag>` |

## GitHub App 權限

release-pr.yml 和 tag-on-merge.yml 使用 GitHub App token 來建立 PR 和推送 tag。App 需要以下 Repository permissions：

| Permission | Access |
|---|---|
| Contents | Read and write |
| Metadata | Read-only (mandatory) |
| Pull requests | Read and write |

對應的 secrets：`APP_ID`（Client ID）、`APP_PRIVATE_KEY`。

## 限制與注意事項

- **Stable release 必須先有 pre-release**：promote-stable 會查找 `v{version}-*` 的 pre-release tag，找不到就失敗
- **promote 用 version tag 找 image**：不依賴 commit SHA，pre-release 和 stable 可以在不同 commit 上
- **外部用戶不會裝到 pre-release**：`helm install` 預設只拿 stable 版本，pre-release 需明確指定 `--version`
