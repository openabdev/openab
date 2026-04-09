# Releasing

## Version Scheme

Versions follow SemVer (e.g. `0.7.0`)。tagpr 根據 PR label 自動決定版本號：

| Label | 效果 | 範例 |
|---|---|---|
| （無） | patch bump | `0.6.0 → 0.6.1` |
| `tagpr:minor` | minor bump | `0.6.0 → 0.7.0` |
| `tagpr:major` | major bump | `0.6.0 → 1.0.0` |

## Release Flow (Tag-Driven)

> **核心原則：測過什麼就發什麼 (what you tested is what you ship)**
> stable release 不重新 build，直接 promote pre-release 驗證過的 image。

##### Step 1 — 累積變更

```
  ┌─────────────────────────────────────────────────────────────────┐
  │ 貢獻者 PR merge to main                                          │
  │ → tagpr.yml 觸發                                                │
  │ → tagpr 累積 commits，自動開 Release PR                           │
  │   (更新 Cargo.toml + Chart.yaml version/appVersion + CHANGELOG)  │
  └─────────────────────────────────────────────────────────────────┘
```

##### Step 2 — Merge Release PR

```
  ┌─────────────────────────────────────────────────────────────────┐
  │ Maintainer review Release PR                                     │
  │ 確認版本號 / changelog 後 merge                                   │
  │ → tagpr 自動打 tag (e.g. v0.7.0) + 建立 GitHub Release           │
  │ → build.yml 觸發 (is_prerelease=false)                           │
  │ → promote-stable 失敗（預期中，還沒有 pre-release image）          │
  └─────────────────────────────────────────────────────────────────┘
```

##### Step 3 — Pre-release（在同一個 commit 上）

```
  ┌─────────────────────────────────────────────────────────────────┐
  │ 在 tagpr 的 stable tag 同一個 commit 上打 pre-release tag：       │
  │                                                                  │
  │   git tag v0.7.0-rc.1 v0.7.0                                    │
  │   git push origin v0.7.0-rc.1                                   │
  │                                                                  │
  │ → build.yml 觸發 (is_prerelease=true)                            │
  │ → build-image:    4 variants × 2 platforms (amd64 + arm64)      │
  │ → merge-manifests: image tags = <sha> + 0.7.0-rc.1              │
  │ → release-chart:  helm chart → OCI registry                      │
  └─────────────────────────────────────────────────────────────────┘
```

> **關鍵**：`git tag v0.7.0-rc.1 v0.7.0` 用 stable tag 當 ref，
> 確保 pre-release 跟 stable 在完全相同的 commit 上。

##### Step 4 — 內部測試

```
  ┌─────────────────────────────────────────────────────────────────┐
  │ 部署 pre-release 進行測試：                                       │
  │                                                                  │
  │   helm install openab \                                          │
  │     oci://ghcr.io/openabdev/charts/openab \                      │
  │     --version 0.7.0-rc.1                                         │
  │                                                                  │
  │ 發現 bug？→ 修復 → 回到 Step 1（下一輪 release cycle）             │
  └─────────────────────────────────────────────────────────────────┘
```

##### Step 5 — 測試通過，Promote to Stable

```
  ┌─────────────────────────────────────────────────────────────────┐
  │ 重新觸發 stable build：                                           │
  │                                                                  │
  │   gh workflow run build.yml -f tag=v0.7.0                        │
  │                                                                  │
  │ → promote-stable: 驗證 pre-release image 存在                    │
  │   re-tag <sha> → 0.7.0 / 0.7 / latest                          │
  │   ⚠️ 不 rebuild，跟 pre-release 是同一個 artifact                 │
  │ → release-chart: helm chart → OCI registry                       │
  └─────────────────────────────────────────────────────────────────┘
```

##### Step 6 — Chart Release

```
  ┌─────────────────────────────────────────────────────────────────┐
  │ release.yml 偵測到 Chart.yaml 變更 push to main                   │
  │ → chart-releaser 更新 GitHub Pages helm repo index               │
  │ → 附加 install instructions 到 chart release notes               │
  └─────────────────────────────────────────────────────────────────┘
```

## 快速指令參考

```bash
# ── Pre-release（Step 3）──────────────────────────────
git tag v0.7.0-rc.1 v0.7.0
git push origin v0.7.0-rc.1

# ── 第二輪 pre-release（如果 rc.1 有 bug）────────────
# 修 bug → PR merge → 重新走 Step 1-3
# tagpr 會開新的 Release PR (v0.7.1)

# ── Promote to stable（Step 5）────────────────────────
gh workflow run build.yml -f tag=v0.7.0

# ── 手動重跑（build 失敗時）──────────────────────────
gh workflow run build.yml -f tag=v0.7.0-rc.1
```

## GitHub Releases

每次 release 會產生兩個 GitHub Release：

| Release | Tag 格式 | 內容 |
|---|---|---|
| tagpr | `v0.7.0` | CHANGELOG（自動從 commits 產生） |
| chart-releaser | `openab-0.7.0` | Version Info + Installation instructions |

## Workflow 對應表

| Workflow | 觸發條件 | 用途 |
|---|---|---|
| `tagpr.yml` | push to main | 自動開 Release PR、打 tag、建立 GitHub Release |
| `build.yml` | tag push `v*` | pre-release: 完整 build / stable: promote image + push chart |
| `release.yml` | Chart.yaml 變更 push to main | chart-releaser 更新 GitHub Pages index + install instructions |

## Version 同步 (tagpr)

tagpr 在 Release PR 中自動更新以下檔案的版本：

| 檔案 | 欄位 | 更新方式 |
|---|---|---|
| `Cargo.toml` | `version` | tagpr 內建 (`versionFile`) |
| `charts/openab/Chart.yaml` | `version` | tagpr 內建 (`versionFile`) |
| `charts/openab/Chart.yaml` | `appVersion` | `postVersionCommand` |

三者統一為同一個 semver（e.g. `0.7.0`）。

## Image Variants

每次 build 產出 4 個 multi-arch image (linux/amd64 + linux/arm64)：

```
ghcr.io/openabdev/openab          # default (kiro-cli)
ghcr.io/openabdev/openab-codex    # codex
ghcr.io/openabdev/openab-claude   # claude
ghcr.io/openabdev/openab-gemini   # gemini
```

Image tags 依 release 類型不同：

| Tag | Stable (`v0.7.0`) | Pre-release (`v0.7.0-rc.1`) |
|---|---|---|
| `<sha>` | v (from pre-release) | v |
| `0.7.0` / `0.7.0-rc.1` | v | v |
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
| tagpr 開 Release PR 後 | Review 版本號 / CHANGELOG |
| 需要調整版本升級幅度 | 在 Release PR 加 `tagpr:minor` 或 `tagpr:major` label |
| 決定 release | Merge Release PR（tagpr 打 stable tag） |
| Pre-release | `git tag v0.7.0-rc.1 v0.7.0 && git push origin v0.7.0-rc.1` |
| 測試通過，promote stable | `gh workflow run build.yml -f tag=v0.7.0` |
| build 失敗或需重跑 | Actions → Build & Release → Run workflow（填入 tag） |

## 限制與注意事項

- **Stable release 必須先有 pre-release**：promote-stable 會驗證 image 是否存在，沒有 pre-release build 過就無法 promote
- **Pre-release 必須在同一 commit**：用 `git tag v0.7.0-rc.1 v0.7.0` 確保跟 stable tag 指向同一 commit
- **Promote 需手動觸發**：tagpr 打 stable tag 時 promote 會失敗（還沒 pre-release），測試通過後用 `gh workflow run` 重跑
- **外部用戶不會裝到 pre-release**：`helm install` 預設只拿 stable 版本，pre-release 需明確指定 `--version`
