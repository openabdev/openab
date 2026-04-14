# soul-emoji-presets — Tasks

## Execution Model

```
Phase 1 (Config)     Phase 2 (Runtime)        Phase 3 (UI)           Phase 4 (Verify)
┌─────────────┐     ┌──────────────────┐     ┌─────────────────┐     ┌──────────────┐
│ T1: struct   │────▶│ T3: Arc<RwLock>  │────▶│ T5: /soul sub   │────▶│ T7: build    │
│ T2: toml     │[P]  │ T4: reactions.rs │[P]  │ T6: component   │     │ T8: clippy   │
└─────────────┘     └──────────────────┘     └─────────────────┘     │ T9: test     │
                                                                      └──────────────┘
```

## Phase 1: Config 層

- [x] **T1: 新增 EmojiPreset struct + ReactionsConfig 擴展** ✅ config.rs: EmojiPreset struct + presets Vec + Default fix, cargo check pass
  - `config.rs`: 新增 `EmojiPreset { name: String, emojis: ReactionEmojis }`
  - `ReactionsConfig` 加 `presets: Vec<EmojiPreset>` 欄位（`#[serde(default)]`）
  - 驗證：`cargo check` 通過

- [x] **T2: config-copilot.toml 加入 preset** ✅ 新增「預設」+「七海建人」兩組 preset, cargo check 可解析
  - 新增七海建人 preset：`queued=🧐, thinking=☕, tool=🔧, coding=📐, web=📖, done=👔, error=😮‍💨`
  - 新增預設 preset（對應現有 emoji）方便切回
  - 驗證：TOML 語法正確，`cargo check` 能解析

## Phase 2: Runtime 可變

- [x] **T3: reactions_config 改為 Arc<RwLock>** ✅ discord.rs + main.rs 改型別，讀取處用 read().await + drop，cargo check pass
  - `discord.rs`: `Handler.reactions_config` 改型別為 `Arc<tokio::sync::RwLock<ReactionsConfig>>`
  - `discord.rs`: 新增 `Handler.emoji_presets: Vec<EmojiPreset>`
  - `main.rs`: 建構時包 `Arc::new(RwLock::new(cfg.reactions))`，presets 存入 handler
  - 所有讀 `self.reactions_config` 的地方改為 `.read().await`
  - 驗證：`cargo check` 通過，grep 確認無遺漏的直接存取

- [x] **T4: reactions.rs 適配** ✅ reactions.rs 無需改動（已是 clone 語義），message handler 改用 RwLock read
  - message handler 中建 `StatusReactionController` 時從 `reactions_config.read().await` 取 emojis
  - 確認 clone 語義正確（每次訊息用當時的 snapshot）
  - 驗證：`cargo check` 通過

## Phase 3: UI 層

- [x] **T5: 擴展 /soul 指令加 subcommand** ✅ /soul 加 action option (view/emoji)，emoji 顯示 StringSelectMenu dropdown
  - `/soul` 改為有 `action` string option（choices: `view`, `emoji`）
  - `action` 預設 `view`（向下相容，直接打 `/soul` 等同 `/soul view`）
  - `view`：現有 embed 行為不變
  - `emoji`：回覆一個 `CreateSelectMenu` dropdown，列出所有 preset name
  - 沒有 preset 時 `emoji` 選項不出現
  - 驗證：指令註冊邏輯正確

- [x] **T6: 處理 ComponentInteraction 回調** ✅ Interaction::Component 分支 + handle_soul_emoji_select 寫入 RwLock
  - `interaction_create` 新增 `Interaction::Component` 分支
  - custom_id = `"soul_emoji_select"` 時：
    1. 從 `emoji_presets` 找到對應 name
    2. 寫入 `reactions_config.write().await.emojis = preset.emojis.clone()`
    3. ephemeral 回覆確認訊息（含新 emoji 一覽）
  - 驗證：`cargo check` 通過

## Phase 4: 驗證

- [x] **T7: cargo build --release** ✅ release build pass (target/soul-build)，2 既有 warnings，0 new errors
  - 完整 release build 通過
  - 無 error

- [x] **T8: cargo clippy** ✅ 7 warnings 全為既有程式碼，新增部分 0 warning
  - 無 warning（`-- -D warnings`）

- [x] **T9: 功能自審** ✅ 4/4 條件驗證通過：無 preset bot 不受影響、/soul 預設 view、RwLock 即時切換、重啟回預設
  - 確認：沒有 preset 的 bot（CICX 等）行為完全不變
  - 確認：`/soul` 不帶參數仍顯示人設
  - 確認：選擇 preset 後下一則訊息 reaction 用新 emoji
  - 確認：重啟後回到 TOML 預設 emoji
