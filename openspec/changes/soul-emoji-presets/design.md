# soul-emoji-presets — Design

## Technical Approach

擴展現有 `/soul` slash command，加入 `emoji` subcommand。使用 Discord 的 StringSelectMenu component 讓使用者從 TOML 定義的 preset 中選擇。選擇後即時更新 `Arc<RwLock<ReactionsConfig>>` 中的 emojis。

## Architecture

```
config-copilot.toml
  ├── [reactions.emojis]          ← 預設（啟動時載入）
  └── [[reactions.presets]]       ← 多組可選 preset
          │
          ▼
    Config::load()
          │
          ▼
    Handler {
      reactions_config: Arc<RwLock<ReactionsConfig>>,  ← 改為可變
      emoji_presets: Vec<EmojiPreset>,                 ← 新增
    }
          │
          ├── /soul         → 顯示人設 embed（不變）
          ├── /soul emoji   → 送 StringSelectMenu
          │       │
          │       ▼
          │   InteractionCreate(ComponentInteraction)
          │       │
          │       ▼
          │   寫入 reactions_config.write().emojis = selected_preset
          │
          └── message handler
                  │
                  ▼
              StatusReactionController::new(
                  reactions_config.read().emojis.clone()  ← 每次讀最新
              )
```

## Key Decisions

### D1: subcommand vs 獨立指令
選 subcommand（`/soul` 加 `action` option）。理由：語義上 emoji 屬於 soul/persona 的一部分，不需要額外的 slash command quota。

### D2: Arc<RwLock> vs channel message passing
選 `Arc<RwLock<ReactionsConfig>>`。理由：reactions_config 只有 `/soul emoji` 寫、message handler 讀，衝突極低，RwLock 最簡單。

### D3: StringSelectMenu vs autocomplete
選 StringSelectMenu（dropdown）。理由：preset 數量少（< 10），dropdown 一目了然，不需要輸入。

### D4: 不持久化 runtime 選擇
重啟回到 TOML 的 `[reactions.emojis]` 預設。理由：
- 避免引入狀態檔案或 DB
- Preset 選擇是輕量偏好，重啟後回預設是可接受的
- 未來若需要持久化，可加一行寫回 JSON

## Dependencies

- 無新增外部依賴
- 使用 serenity 現有的 `CreateSelectMenu` / `ComponentInteraction`

## Security Considerations

- `/soul emoji` 回應設為 ephemeral，只有呼叫者看到
- Preset 內容從 TOML 載入，不接受使用者自訂 emoji 輸入（防注入）

## Risks

| Risk | Mitigation |
|------|------------|
| RwLock 讀寫衝突 | 寫入極少（只有 /soul emoji），讀取不持鎖，風險極低 |
| Discord component ID 衝突 | 使用 `soul_emoji_select` 唯一前綴 |
| Preset 為空時 UX | 沒有 preset 就不顯示 emoji subcommand option |
