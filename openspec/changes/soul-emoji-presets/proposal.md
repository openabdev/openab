# soul-emoji-presets

## Goal

讓使用者透過 `/soul emoji` 指令即時切換 GITX bot 的 reaction emoji 風格，無需重啟。

## Background

- 目前 5 個 bot 共用同一組 reaction emoji（👀🤔🔥👨‍💻⚡🆗😱）
- GITX 有獨立人設（七海建人），但 emoji 沒有反映個性
- 使用者希望 `/soul` 不只顯示人設，還能選擇專屬 emoji 風格
- 選擇方案 A：TOML 內嵌 preset，結構清晰、型別安全、可復用

## Scope

### In
- config.rs：新增 `EmojiPreset` struct + `reactions.presets` 欄位
- discord.rs：`reactions_config` 改為 `Arc<RwLock<ReactionsConfig>>`，支援執行時切換
- discord.rs：擴展 `/soul` 指令加 `emoji` subcommand（Discord StringSelect dropdown）
- main.rs：建構 handler 時包 `Arc<RwLock<>>`
- config-copilot.toml：加入七海建人風格的 emoji preset
- reactions.rs：每次建 controller 時從 RwLock 讀最新 emojis

### Out
- 其他 4 個 bot 的 config 不改（保持預設 emoji）
- 不改 soul file 格式（維持純文字）
- 不做 preset 持久化（重啟回到 TOML 預設，runtime 選擇是暫時的）

## Success Criteria

1. `/soul` 顯示人設（現有功能不變）
2. `/soul emoji` 彈出 dropdown 列出所有 preset
3. 選擇 preset 後，下一則訊息的 reaction 立即使用新 emoji
4. 沒有 preset 設定的 bot 不會出現 `/soul emoji` 選項
5. `cargo build --release` 編譯通過
6. `cargo clippy` 無 warning
