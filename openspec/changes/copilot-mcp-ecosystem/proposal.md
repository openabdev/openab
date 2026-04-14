# Proposal: Copilot MCP Ecosystem

## Goal

讓 GITX/COPILX 從「純聊天」升級為「能做事」——接入 MCP servers，使 Copilot backend 能存取 GitHub、Notion、文檔查詢等外部工具，並逐步演化為多用戶自助 MCP 生態。

## Background

- GITX/COPILX 目前 0 個 MCP server，Copilot CLI 的 MCP 能力完全未啟用
- Copilot CLI v1.0.24+ 已支援：`--enable-all-github-mcp-tools`、`~/.copilot/mcp-config.json`、`--additional-mcp-config`、`copilot mcp add/remove`
- 競品 Kiro CLI（OpenAB 預設 agent）從 Amazon Q 時代就有成熟 MCP 生態，支援 auto-detect + per-session 注入
- OpenAB 的 CICX（Claude Code）已有豐富工具鏈，GITX 相比之下功能空白

## Scope

### In Scope（4 Phases）

| Phase | 名稱 | 核心 |
|-------|------|------|
| 1 | 全域 MCP | 管理員寫 `mcp-config.json` + GitHub 內建 flag，所有用戶共享 |
| 2 | Per-User Profile | `data/mcp-profiles/{user_id}.json` + `/mcp-add` `/mcp-remove` `/mcp-list` 自助指令 |
| 3 | 自動偵測 | `gh auth status` / workspace `.mcp.json` / npm global scan → 自動建議啟用 |
| 4 | Marketplace | `/mcp-browse` `/mcp-install` `/mcp-status` `/mcp-share` + MCP Registry catalog |

### Out of Scope

- Kiro CLI 的 MCP 接入（Kiro 已有原生支援）
- CICX（Claude Code）的 MCP（走 Claude 自己的 MCP 機制）
- Copilot SDK 源碼修改（只改 bridge + OpenAB Rust）

## Success Criteria

1. Phase 1：Discord 裡 `@GITX 查 openab repo 最新 PR` → 回傳 GitHub PR 資訊
2. Phase 2：User A 有 Notion MCP、User B 沒有 → 各自 session 行為不同
3. Phase 3：新用戶首次互動 → GITX 自動偵測到 GitHub token 並提示啟用
4. Phase 4：`/mcp-browse` 列出 10+ 可安裝的 MCP servers
