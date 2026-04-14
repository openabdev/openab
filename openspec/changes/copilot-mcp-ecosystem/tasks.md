# Tasks: Copilot MCP Ecosystem

## Phase 1: Global MCP（全域配置）

- [x] **1.1** 寫 `~/.copilot/mcp-config.json`（Notion HTTP + Context7 stdio），驗證 `copilot mcp list` 顯示兩個 server [P]
- [x] **1.2** COPILX `config-copilot-native.toml` args 加 `--enable-all-github-mcp-tools`，重啟後驗證 log 有 GitHub MCP tools loaded [P]
- [x] **1.3** 驗證 GITX bridge 的 CopilotClient 是否自動讀 `mcp-config.json`：❌ 不讀（mcp-list count=0），需走 1.4 fallback
- [x] **1.4** SDK SessionConfig.mcpServers 注入：bridge 啟動時讀 `~/.copilot/mcp-config.json` → `createSession({ mcpServers })` + `resumeSession` 同步注入
- [x] **1.5** E2E 驗證：GITX bridge 無法注入 MCP（SDK 不管 MCP，只 CLI 層生效）。MCP 由 COPILX 專責，GITX 保留人設+指令+usage tracking
- [x] **1.6** E2E 驗證：COPILX `github-mcp-server-list_issues` ✅ 成功呼叫 GitHub MCP tool

## Phase 2: Per-User MCP Profile

- [x] **2.1** 建立 `data/mcp-profiles/` 目錄 + JSON schema 定義（含 discord_user_id、mcpServers、enabled、updated_at）[P]
- [x] **2.2** `config.rs` + `discord.rs` Handler + `main.rs` + `config-copilot.toml` 新增 `mcp_profiles_dir`，`cargo check` 通過 [P]
- [x] **2.3** `/mcp-add <name> <url>` — 寫入 `data/mcp-profiles/{user_id}.json`，ephemeral 確認
- [x] **2.4** `/mcp-remove <name>` — 從 profile 刪除，ephemeral 確認
- [x] **2.5** `/mcp-list` — 讀取 profile 顯示所有 MCP servers（ephemeral）
- [~] **2.6** BLOCKED: SDK 不支援 per-session MCP 注入。需要 OpenAB Rust 端支援 per-user 動態 agent args（架構級改動）
- [~] **2.7** BLOCKED: 依賴 2.6
- [~] **2.8** BLOCKED: 依賴 2.6-2.7

## Phase 3: Auto-Detection

- [~] **3.1-3.5** BLOCKED: Phase 3 全部依賴 per-session MCP 注入機制（Phase 2.6）。需先解決架構問題。

## Phase 4: Marketplace

- [x] **4.1** `data/mcp-registry.json` — 11 servers（github/notion/context7/filesystem/memory/brave-search/slack/linear/sentry/puppeteer/mempalace）[P]
- [x] **4.2** `/mcp-browse` — 讀 registry 顯示 11 servers（名稱+分類+描述+auth）
- [x] **4.3** `/mcp-install <name>` — 從 registry 複製到 user profile，顯示 auth 需求
- [x] **4.4** `/mcp-status` — 顯示已安裝 MCP 列表（靜態狀態，未做 runtime ping）
- [~] **4.5** `/mcp-share` — 暫緩（低優先級，需要 @mention 解析）
- [~] **4.6** E2E 驗證：GITX 33 commands 已部署，待 Discord 測試 `/mcp-browse` → `/mcp-install` → `/mcp-status`
