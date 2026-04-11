# Spec-Driven Development (SDD) with OpenAB

透過 [GitHub Spec Kit](https://github.com/github/spec-kit) 在 Discord 上執行 Spec-Driven Development 流程。不需要修改任何 OpenAB 程式碼，只需要設定 Skill / Command 檔案。

## 前置需求

- 一個正在運行的 OpenAB 實例（任何 agent 皆可）
- Agent 的 working directory 有寫入權限

## 設定步驟

### 1. 安裝 specify CLI（在 Pod 內）

進入 OpenAB 的 Pod：

```bash
kubectl exec -it <pod-name> -- bash
```

安裝 `uv` 和 `specify-cli`：

```bash
curl -LsSf https://astral.sh/uv/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
uv tool install specify-cli --from git+https://github.com/github/spec-kit.git@v0.6.0
```

驗證安裝：

```bash
specify --version
```

> 如果希望每次部署都自帶，可以把上述步驟加到 Dockerfile 裡。

### 2. 安裝 SDD Skill / Command

SDD 流程定義在 `docs/skills/speckit-sdd.md`。根據你使用的 agent，將它放到對應位置：

#### Kiro CLI

```bash
mkdir -p .kiro/skills/speckit-sdd
cp docs/skills/speckit-sdd.md .kiro/skills/speckit-sdd/SKILL.md
```

#### Claude Code

```bash
mkdir -p .claude/commands
cp docs/skills/speckit-sdd.md .claude/commands/speckit-sdd.md
```

#### Codex

```bash
mkdir -p codex-skills/speckit-sdd
cp docs/skills/speckit-sdd.md codex-skills/speckit-sdd/SKILL.md
```

#### Gemini CLI

需要轉成 TOML 格式，或直接將內容貼入 `.gemini/commands/speckit.sdd.toml` 的 `prompt` 欄位。

### 3. 初始化 Spec-Kit 專案（可選）

在 working directory 下執行：

```bash
specify init . --ai <agent-name>
```

這會建立 `.specify/` 目錄結構。也可以跳過這步，讓 agent 在 Discord 對話中自動初始化。

## 使用方式

在 Discord 中 mention bot，用自然語言觸發 SDD 流程：

| 你說的話 | Agent 會做什麼 |
|---|---|
| `用 spec-kit 幫我規劃一個 photo app` | 從 Phase 1 開始跑完整 SDD |
| `幫我寫 spec：一個照片管理工具` | 只跑 specify phase |
| `幫我拆 tasks` | 只跑 tasks phase |
| `SDD` | 詢問你要做什麼，然後開始 |

每個 phase 結束後 agent 會暫停確認，不會自動跳到下一步。

## SDD 流程概覽

```
Phase 0: Init        → specify init（建立 .specify/ 目錄）
Phase 1: Constitution → 建立專案原則
Phase 2: Specify      → 撰寫功能規格
Phase 3: Plan         → 產生技術實作計畫
Phase 4: Tasks        → 拆解為可執行任務
Phase 5: Implement    → 逐一實作任務
```

所有產出存放在 `.specify/` 目錄下，agent 在後續對話中會自動讀取這些檔案作為上下文。
