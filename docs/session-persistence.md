# Session Persistence

## 背景與問題

openab 的每個 Discord thread 對應一個 ACP agent process（Claude Code、Gemini、Codex、Kiro 等）。原始設計將所有 session 狀態放在記憶體的 `HashMap` 裡，這代表：

- **Pod 重啟後**，所有進行中的對話會消失，user 必須從頭開始
- **Agent process crash 後**，session 不會自動還原
- **未來接其他平台**（Slack、Telegram）時，沒有統一的 session identity 格式

這份文件說明 `feat: add file-based session persistence` 這個 commit 的設計決策與實作細節。

---

## 設計目標

1. **重開機還原** — Pod 重啟後，user 發訊息時能自動還原上一次的對話脈絡
2. **最小依賴** — 不引入 Redis、PostgreSQL 等外部服務；利用已有的 PVC mount（`/data`）
3. **Crash-safe** — 中途斷電或 OOM kill 不會損壞已儲存的資料
4. **平台無關** — Session identity 不綁定 Discord，未來接 Slack/Telegram 不需要改核心邏輯

---

## 為什麼選擇檔案系統而不是 SQLite 或 Redis？

| 選項 | 優點 | 缺點 | 結論 |
|------|------|------|------|
| 檔案系統 (JSONL) | 零依賴、人可讀、append-only crash-safe、PVC 已有 | 無 index query、單 pod only | **採用** |
| SQLite | 有 index、WAL crash-safe | 單 pod only、需要額外 crate | 過度設計 |
| Redis | 多 pod、有 TTL | 需要額外服務、Redis 本身也要 persist | 目前不需要 |

目前 openab 是單 pod 部署，session 數量少（預設 max 10），不需要複雜查詢。等真的需要橫向擴展時，session store 已經是獨立的 `SessionStore` 層，換掉後端不影響其他邏輯。

---

## 架構

```
/data/sessions/
├── index.json                  ← 所有 session 的 metadata（atomic write）
├── discord_987654321.jsonl     ← session "discord:987654321" 的對話記錄
└── discord_111222333.jsonl
```

### `index.json` 格式

```json
{
  "sessions": {
    "discord:987654321": {
      "key": "discord:987654321",
      "platform": "discord",
      "agent": "claude-code",
      "created_at": 1712345678,
      "last_active": 1712399999
    }
  }
}
```

### JSONL transcript 格式

每行一個 JSON，append-only：

```jsonl
{"role":"user","content":"幫我寫一個 hello world","ts":1712345680}
{"role":"assistant","content":"好的，這是 hello world 範例...","ts":1712345682}
```

---

## Session Key 設計

原本的 key 是 `thread_id: u64`（Discord 特有）。

現在改成平台無關的字串格式：

```
"{platform}:{thread_id}"
```

範例：
- `"discord:987654321"` — Discord thread
- `"slack:C012AB3CD:1234567890.123456"` — Slack thread（未來）
- `"telegram:-100123:42"` — Telegram thread（未來）

這讓 `SessionPool` 和 `SessionStore` 完全不知道平台是誰，只管 key 字串。

---

## 還原流程

```
Pod 重啟 → 記憶體清空

User 在 Discord thread 發訊息
    ↓
discord.rs: session_key = "discord:{thread_id}"
    ↓
pool.get_or_create("discord:987654321")
    ↓
    ├─ 記憶體有，且 alive → 直接回傳（正常路徑）
    │
    ├─ 記憶體沒有，但 index.json 有記錄
    │      ↓
    │   spawn 新 agent process
    │      ↓
    │   initialize() + session/new()
    │      ↓
    │   讀 discord_987654321.jsonl（最多最近 20 條）
    │      ↓
    │   session_prime_context(history)
    │   → 把歷史對話送進 agent（silently drain 回應）
    │      ↓
    │   回傳，conn.session_reset = true
    │   → discord.rs 顯示 "⚠️ Session expired, starting fresh..."
    │
    └─ 都沒有 → 全新 session，寫入 index.json
```

---

## Crash-safe 保證

**`index.json`（atomic write）**

```rust
// 寫到 .tmp 再 rename，rename 是 POSIX atomic 操作
tokio::fs::write(&tmp, content).await?;
tokio::fs::rename(&tmp, index_path).await?;
```

即使在 write 過程中 crash，`.tmp` 會留著，但 `index.json` 還是舊的完整版本，下次啟動不會讀到半寫的資料。

**JSONL transcript（append-only）**

每行獨立，寫到一半只會損壞最後一行。`load_transcript` 使用 `serde_json::from_str(l).ok()` 忽略無法解析的行：

```rust
.filter_map(|l| serde_json::from_str(l).ok())
```

---

## 還原的 context 限制

`store.rs` 中的常數：

```rust
const MAX_RESTORE_ENTRIES: usize = 20;
```

只還原最近 20 條訊息。原因：
- Agent process 初始化時，`session_prime_context` 會把歷史送進去，等 agent 回應才算完成
- 太長的 context 會超過 timeout（`session_prime_context` 有 60s 上限）
- 大部分對話的連貫性在最近 20 條以內就夠了

---

## 新增的設定

`config.toml` 新增可選的 `[session]` 區段：

```toml
[session]
dir = "/data/sessions"  # 預設值，通常不需要改
```

若不設定，預設使用 `/data/sessions`（在 Helm chart 的 PVC mount 路徑 `/data` 之下）。

---

## 檔案結構

```
src/
├── session/
│   ├── mod.rs       ← pub use
│   ├── key.rs       ← SessionKey struct
│   └── store.rs     ← SessionStore（load/upsert/append/remove）
├── acp/
│   ├── connection.rs  ← 新增 session_prime_context()
│   └── pool.rs        ← 整合 SessionStore
├── discord.rs         ← 使用 SessionKey，記錄 transcript
├── config.rs          ← 新增 SessionConfig
└── main.rs            ← 初始化 SessionStore
```

---

## 未來擴充

接新平台（Slack、Telegram）時，只需要：

1. 新增 `SessionKey::slack(...)` / `SessionKey::telegram(...)` 等 constructor
2. 實作各自的 platform handler（類似 `discord.rs`），使用同樣的 `SessionStore` 和 `SessionPool`
3. Session 核心邏輯完全不動

接新 agent 時，只需要修改 `config.toml` 的 `[agent]` 區段，session store 格式與 agent 無關。
