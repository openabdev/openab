---
name: speckit-sdd
description: 使用 Spec-Kit 進行 Spec-Driven Development (SDD) 的完整流程。當使用者提到「SDD」、「spec-kit」、「寫規格」、「specify」、「建立 spec」時使用。
compatibility: >
  此 frontmatter 使用 YAML 格式，與 Kiro CLI (.kiro/skills/*/SKILL.md) 和
  Codex (codex-skills/*/SKILL.md) 原生相容。Claude Code (.claude/commands/) 會忽略
  未知的 frontmatter key。Gemini CLI 需轉為 TOML 格式，參見 docs/speckit-integration.md。
---

# Skill: Spec-Driven Development with Spec-Kit

當使用者要求進行 Spec-Driven Development 時，依照以下流程執行。

## 觸發條件

使用者提到以下意圖時啟動：
- 「用 spec-kit」、「SDD」、「spec-driven」
- 「寫規格」、「建立 spec」、「specify」
- 「/speckit.constitution」、「/speckit.specify」、「/speckit.plan」、「/speckit.tasks」、「/speckit.implement」
- 「幫我規劃這個功能」、「先寫 spec 再實作」

## 前置確認

1. 確認 `specify` CLI 可用：`specify --version`
   - 如果不存在，提示使用者安裝：`uv tool install specify-cli --from git+https://github.com/github/spec-kit.git@v0.6.0`
2. 確認專案是否已初始化 spec-kit：檢查 `.specify/` 目錄是否存在
   - 不存在 → 進入 Phase 0
   - 已存在 → 詢問使用者要從哪個 Phase 開始

## 執行流程

### Phase 0：初始化（Init）

只在專案尚未初始化時執行。

```bash
specify init . --ai <agent>
```

這會建立 `.specify/` 目錄結構。

### Phase 1：憲法（Constitution）

建立專案的核心原則與開發準則。

1. 詢問使用者專案的核心價值觀，例如：
   - 程式碼品質標準
   - 測試要求
   - 效能要求
   - 安全性考量
2. 執行：`specify constitution`（或手動建立 `.specify/constitution.md`）
3. 向使用者確認產出的原則是否正確

### Phase 2：規格（Specify）

描述要建構的功能。

1. 詢問使用者：「你想要建構什麼？專注在 what 和 why，不用管技術細節。」
2. 執行：`specify specify "<使用者描述>"`（或手動建立 `.specify/spec.md`）
3. 產出的 spec 應包含：
   - 功能描述
   - 使用者情境
   - 成功標準
   - 邊界條件
4. 向使用者確認 spec 內容

### Phase 3：計畫（Plan）

根據 spec 產生技術實作計畫。

1. 詢問使用者技術偏好：
   - 使用的框架 / 語言
   - 架構風格
   - 任何限制條件
2. 執行：`specify plan "<技術指引>"`（或手動建立 `.specify/plan.md`）
3. 產出的 plan 應包含：
   - 技術架構
   - 檔案結構
   - 依賴套件
   - 實作策略
4. 向使用者確認 plan 內容

### Phase 4：任務拆解（Tasks）

將 plan 拆成可執行的任務清單。

1. 執行：`specify tasks`（或手動建立 `.specify/tasks.md`）
2. 每個 task 應該是：
   - 獨立可完成的
   - 有明確的完成標準
   - 按依賴順序排列
3. 向使用者確認任務清單

### Phase 5：實作（Implement）

逐一執行任務。

1. 執行：`specify implement`（或按照 tasks.md 逐一手動實作）
2. 每完成一個 task：
   - 告知使用者進度
   - 如果遇到問題，暫停並詢問
3. 全部完成後，摘要所有改動

## 單一 Phase 執行

使用者可以只要求執行特定 phase，例如：
- 「幫我寫 spec」→ 只跑 Phase 2
- 「幫我拆 tasks」→ 只跑 Phase 4
- 「直接實作」→ 只跑 Phase 5（前提是前面的 phase 已完成）

如果前置 phase 的產出不存在（例如要跑 plan 但沒有 spec），先提醒使用者需要先完成前面的步驟。

## 注意事項

- 每個 phase 結束都要跟使用者確認，不要自動跳到下一個
- spec 和 plan 的內容要存到 `.specify/` 目錄下
- 如果使用者對產出不滿意，可以重跑該 phase
- `specify` CLI 不可用時，手動建立對應的 markdown 檔案也可以達到同樣效果
