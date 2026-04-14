# Design: Copilot MCP Ecosystem

## Architecture

```
Discord User
     │
     ▼
┌──────────────┐  ACP stdio    ┌─────────────────────┐
│ OpenAB (Rust)│──────────────►│ copilot-agent-acp.js│
│  discord.rs  │◄── JSON-RPC ──│    (bridge)          │
│              │               │                      │
│ /mcp-add     │               │ MCP Router           │
│ /mcp-list    │               │ ├─ Phase 1: global   │
│ /mcp-browse  │               │ ├─ Phase 2: per-user │
│              │               │ ├─ Phase 3: detect   │
│ mcp-profiles/│               │ └─ Phase 4: registry │
└──────────────┘               └──────────┬───────────┘
                                          │
                     ┌────────────────────┼────────────────────┐
                     ▼                    ▼                    ▼
              ┌────────────┐      ┌────────────┐      ┌────────────┐
              │ GitHub MCP │      │ Notion MCP │      │ Context7   │
              │ (built-in) │      │ (HTTP)     │      │ (stdio)    │
              └────────────┘      └────────────┘      └────────────┘
```

## Phase 1: Global MCP Config

### Approach
1. 寫 `~/.copilot/mcp-config.json`（Notion + Context7）
2. COPILX 的 `config-copilot-native.toml` args 加 `--enable-all-github-mcp-tools`
3. GITX bridge：Copilot SDK 的 `CopilotClient` 啟動時自動讀 `~/.copilot/mcp-config.json`（需驗證）
4. 若 SDK 不自動讀，bridge 改用 `--additional-mcp-config @~/.copilot/mcp-config.json` 傳入

### Files Changed
- `~/.copilot/mcp-config.json`（新增）
- `config-copilot-native.toml`（args 加 flag）
- `copilot-agent-acp.js`（可能需改 SDK init）

## Phase 2: Per-User MCP Profile

### Approach
1. `data/mcp-profiles/{discord_user_id}.json` 儲存個人 MCP 設定
2. OpenAB Rust 新增 `/mcp-add`、`/mcp-remove`、`/mcp-list` slash commands
3. `/mcp-add` 寫入 profile JSON → bridge 下次建 session 時讀取
4. Bridge `handleSessionNew` 改造：從 ACP `session/new` params 拿到 user context → 讀 profile → 注入 `--additional-mcp-config`

### Profile Schema
```json
{
  "discord_user_id": "844236700611379200",
  "mcpServers": {
    "notion": {
      "type": "http",
      "url": "https://mcp.notion.com/mcp"
    }
  },
  "enabled": true,
  "updated_at": "2026-04-13T10:00:00Z"
}
```

### Files Changed
- `src/discord.rs`（+3 slash commands + handlers）
- `src/config.rs`（+mcp_profiles_dir option）
- `copilot-agent-acp.js`（讀 profile + 注入）
- `data/mcp-profiles/`（新目錄）

## Phase 3: Auto-Detection

### Approach
1. Bridge 啟動時 + 新用戶首次互動時觸發 `discoverMcpServers()`
2. 偵測來源：
   - `gh auth status` → GitHub token 存在 → 建議啟用 GitHub MCP
   - `~/.copilot/mcp-config.json` → 已有手動設定 → 合併
   - workspace `.mcp.json` → project-level MCP
   - `npm list -g --json` → 已安裝的 MCP packages（`@modelcontextprotocol/*`）
3. 偵測結果回報給 Discord 用戶：「我偵測到你有 GitHub 和 Notion 可用，要啟用嗎？」
4. 用戶確認後寫入 profile

### Files Changed
- `copilot-agent-acp.js`（+`discoverMcpServers()` 函數）
- `src/discord.rs`（+onboarding message on first interaction）
- Health monitor：background ping loop（每 5 分鐘）

## Phase 4: Marketplace

### Approach
1. MCP Registry：`data/mcp-registry.json`（靜態 catalog，可擴充為 remote API）
2. `/mcp-browse`：列出 registry 所有可安裝的 MCP（Discord embed + pagination）
3. `/mcp-install <name>`：從 registry 讀 config → 寫入用戶 profile
4. `/mcp-status`：ping 所有已安裝 MCP → 顯示連線狀態
5. `/mcp-share <server> @user`：複製設定到目標用戶 profile

### Registry Schema
```json
{
  "servers": [
    {
      "name": "github",
      "description": "GitHub Issues, PRs, Code Search",
      "type": "builtin",
      "category": "development",
      "setup": "--enable-all-github-mcp-tools"
    },
    {
      "name": "notion",
      "description": "Notion pages, databases, comments",
      "type": "http",
      "url": "https://mcp.notion.com/mcp",
      "category": "productivity",
      "auth": "oauth"
    }
  ]
}
```

## Dependencies

- Copilot CLI v1.0.24+（MCP public preview）
- `gh` CLI（GitHub MCP auth）
- Node.js（bridge + stdio MCP spawning）

## Security Considerations

- Per-user profile 存明文 URL/headers → 不可含 secrets，auth 走 OAuth redirect
- `/mcp-add` 需要 allowed_users 白名單限制（防止任意用戶加危險 MCP）
- Health monitor 的 ping 不可觸發 side effects
- GitHub MCP 全開可能洩漏 private repo → 用 `--add-github-mcp-toolset` 精細控制
