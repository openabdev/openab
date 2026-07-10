# OpenAB — Open Agent Broker

[English](README.md) | 繁體中文

[![Stars](https://img.shields.io/github/stars/openabdev/openab?style=flat-square)](https://github.com/openabdev/openab) [![GitHub Release](https://img.shields.io/github/v/release/openabdev/openab?style=flat-square&logo=github)](https://github.com/openabdev/openab/releases/latest) ![License](https://img.shields.io/badge/license-MIT-A374ED?style=flat-square)

![OpenAB banner](images/banner.jpg)

一個輕量、安全、雲端原生的 ACP harness，透過 stdio JSON-RPC 將 **Discord、Slack** 與任何相容於 [Agent Client Protocol](https://github.com/anthropics/agent-protocol) 的程式開發 CLI（Kiro CLI、Claude Code、Codex、Gemini、OpenCode、MiMo-Code、Copilot CLI、Hermes、Grok Build、Devin、Antigravity、Pi 等）連接起來，帶來新一代的開發體驗。**Telegram、LINE、Feishu/Lark、Google Chat** 以及其他以 webhook 為基礎的平台，則透過獨立的 [Custom Gateway](crates/openab-gateway/) 支援。

🪼 **加入我們的社群！** 歡迎到 Discord 和大家打招呼：**[🪼 OpenAB — Official](https://openab.dev/discord)** 🎉

```
┌──────────────┐  Gateway WS   ┌──────────────┐  ACP stdio    ┌──────────────────┐
│   Discord    │◄─────────────►│              │──────────────►│   coding CLI     │
│   User       │               │    openab    │◄── JSON-RPC ──│   (acp mode)     │
├──────────────┤  Socket Mode  │    (Rust)    │               ├──────────────────┤
│   Slack      │◄─────────────►│              │               │ kiro-cli acp     │
│   User       │               └──────┬───────┘               │ claude-agent-acp │
├──────────────┤                      │  WebSocket            │ codex-acp        │
│   Telegram   │◄──webhook──┐         │   (outbound)          │ gemini --acp     │
│   User       │            │         │                       │ copilot --acp    │
├──────────────┤            ▼         ▼                       │ hermes-acp       │
│   LINE       │◄──webhook──┌──────────────────┐              │ opencode acp     │
│   User       │            │  Custom Gateway  │              │ mimo acp         │
├──────────────┤            │  (standalone)    │              │ grok agent stdio │
│  Feishu/Lark │◄───WS──────│                  │              │ devin acp        │
│   User       │            │                  │              │ agy-acp          │
├──────────────┤            │                  │              │ pi-acp           │
│ Google Chat  │◄──webhook──│                  │              └──────────────────┘
│   User       │            └──────────────────┘
└──────────────┘
```

## 示範

![openab demo](images/demo.png)

## 功能特色

- **多平台支援** — 支援 Discord 與 Slack，可單獨或同時執行
- **Custom Gateway** — 透過獨立的 [gateway](crates/openab-gateway/) 擴充至 Telegram、LINE、Feishu/Lark、Google Chat、MS Teams
- **可替換的 agent backend** — 可透過設定在 Kiro CLI、Claude Code、Codex、Gemini、OpenCode、MiMo-Code、Copilot CLI、Hermes、Grok Build、Devin、Antigravity、Pi 之間切換
- **@mention 觸發** — 在允許的頻道中 mention bot，即可開始對話
- **以討論串進行多輪對話** — 自動建立討論串；後續訊息不需再次 @mention
- **多 agent 協作** — 支援 bot-to-bot 訊息，實現協調式工作流程（[docs/multi-agent.md](docs/multi-agent.md)）
- **由 agent 控制回覆對象** — agent 可透過 `[[reply_to:id]]` 指令選擇要回覆的訊息，讓多 bot 頻道中的對話脈絡更清楚（[docs/output-directives.md](docs/output-directives.md)）
- **編輯式串流輸出** — token 產生時每 1.5 秒即時更新 Discord 訊息
- **Emoji 狀態反應** — 👀→🤔→🔥/👨‍💻/⚡→👍+隨機情緒表情
- **圖片與檔案支援** — 透過聊天傳送圖片與檔案（[docs/sendimages.md](docs/sendimages.md)、[docs/sendfiles.md](docs/sendfiles.md)）
- **排程訊息** — 由設定驅動的 cron job，可自動傳送 agent prompt（[docs/cronjob.md](docs/cronjob.md)）
- **Slash commands** — 內建 slash command 支援（[docs/slash-commands.md](docs/slash-commands.md)）
- **Session pool** — 每個討論串一個 CLI process，自動管理生命週期
- **ACP protocol** — 透過 stdio 使用 JSON-RPC，支援 tool call、thinking 與 permission 自動回覆
- **支援 Kubernetes** — 提供 Dockerfile、k8s manifests 與用於驗證資料持久化的 PVC
- **語音訊息 STT** — 透過 Groq、OpenAI 或本機 Whisper server 自動轉錄 Discord 語音訊息（[docs/stt.md](docs/stt.md)）
- **生命週期 hooks** — 在啟動（`pre_boot`）與關閉（`pre_shutdown`）時執行自訂 script，可用於環境初始化、S3 同步與狀態備份（[docs/hooks.md](docs/hooks.md)）
- **Tailscale 整合** — 透過生命週期 hooks，讓非特權 container 加入私人 tailnet，無需自訂 image（[docs/tailscale.md](docs/tailscale.md)）

## 快速開始

### 事前準備

執行 openab 前，請在 [Discord Developer Portal](https://discord.com/developers/applications) 中啟用以下設定：

1. **Bot → Privileged Gateway Intents**：
   - ✅ Message Content Intent
   - ✅ Server Members Intent
2. **OAuth2 → URL Generator → Bot Permissions**：
   - Send Messages、Embed Links、Attach Files
   - Read Message History、Add Reactions

詳細步驟請參閱 [docs/discord.md](docs/discord.md)。

### 1. 建立 Bot

<details>
<summary><strong>Discord</strong></summary>

詳細步驟請參閱 [docs/discord.md](docs/discord.md)。

</details>

<details>
<summary><strong>Slack</strong></summary>

詳細步驟請參閱 [docs/slack.md](docs/slack.md)。

</details>

<details>
<summary><strong>Telegram</strong>（透過 Custom Gateway）</summary>

完整設定指南請參閱 [docs/telegram.md](docs/telegram.md)。需要獨立的 [Custom Gateway](crates/openab-gateway/) service。

</details>

<details>
<summary><strong>LINE</strong>（透過 Custom Gateway）</summary>

完整設定指南請參閱 [docs/line.md](docs/line.md)。需要獨立的 [Custom Gateway](crates/openab-gateway/) service。

</details>

<details>
<summary><strong>Feishu/Lark</strong>（透過 Custom Gateway）</summary>

完整設定指南請參閱 [docs/feishu.md](docs/feishu.md)。需要獨立的 [Custom Gateway](crates/openab-gateway/) service。支援 WebSocket 長連線（預設，不需要公開 URL）與 HTTP webhook fallback。

</details>

<details>
<summary><strong>Google Chat</strong>（透過 Custom Gateway）</summary>

完整設定指南請參閱 [docs/google-chat.md](docs/google-chat.md)。需要獨立的 [Custom Gateway](crates/openab-gateway/) service。

</details>

<details>
<summary><strong>WeCom（企業微信）</strong>（透過 Custom Gateway）</summary>

完整設定指南請參閱 [docs/wecom.md](docs/wecom.md)。需要獨立的 [Custom Gateway](crates/openab-gateway/) service。

</details>

### 2. 使用 Helm 安裝（Kiro CLI — 預設）

```bash
helm repo add openab https://openabdev.github.io/openab
helm repo update

helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID'

# Slack
helm install openab openab/openab \
  --set agents.kiro.slack.enabled=true \
  --set agents.kiro.slack.botToken="$SLACK_BOT_TOKEN" \
  --set agents.kiro.slack.appToken="$SLACK_APP_TOKEN" \
  --set-string 'agents.kiro.slack.allowedChannels[0]=C0123456789'
```

如需其他 Helm values，例如 `fullnameOverride`、`nameOverride`、`envFrom` 與 `agentsMd`，請參閱 [charts/openab/README.md](charts/openab/README.md)。

### 3. 驗證身分（僅首次需要）

```bash
kubectl exec -it deployment/openab-kiro -- kiro-cli login --use-device-flow
kubectl rollout restart deployment/openab-kiro
```

### 4. 使用方式

在 Discord 頻道中輸入：
```
@YourBot explain this code
```

bot 會建立一個討論串。之後只要直接在討論串中輸入即可，不需再次 @mention。

**Slack：** 在頻道中輸入 `@YourBot explain this code`，同樣會使用以討論串為基礎的工作流程。

## 其他 Agent

| Agent | CLI | ACP Adapter | 指南 |
|-------|-----|-------------|------|
| Kiro（預設） | `kiro-cli acp` | Native | [docs/kiro.md](docs/kiro.md) |
| Claude Code | `claude-agent-acp` | [@agentclientprotocol/claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp) | [docs/claude-code.md](docs/claude-code.md) |
| Codex | `codex-acp` | [@zed-industries/codex-acp](https://github.com/zed-industries/codex-acp) | [docs/codex.md](docs/codex.md) |
| Gemini | `gemini --acp` | Native | [docs/gemini.md](docs/gemini.md) |
| OpenCode | `opencode acp` | Native | [docs/opencode.md](docs/opencode.md) |
| MiMo-Code | `mimo acp` | Native | [docs/mimocode.md](docs/mimocode.md) |
| Copilot CLI ⚠️ | `copilot --acp --stdio` | Native | [docs/copilot.md](docs/copilot.md) |
| Cursor | `cursor-agent acp` | Native | [docs/cursor.md](docs/cursor.md) |
| Hermes Agent | `hermes-acp` | Native | [docs/hermes.md](docs/hermes.md) |
| Grok Build | `grok agent stdio` | Native | [docs/grok.md](docs/grok.md) |
| Devin | `devin acp` | Native | [docs/devin.md](docs/devin.md) |
| Antigravity | `agy-acp` | [agy-acp](agy-acp/) | [docs/antigravity.md](docs/antigravity.md) |
| Pi | `pi-acp` | [pi-acp](https://www.npmjs.com/package/pi-acp) | [docs/pi.md](docs/pi.md) |
| **Native Agent** | `openab-agent` | Built-in（Rust） | [docs/native-agent.md](docs/native-agent.md) |

> 🔧 同時執行多個 agent？請參閱 [docs/multi-agent.md](docs/multi-agent.md)

## AgentCore Runtime

在 [Amazon Bedrock AgentCore](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/runtime.html) 上遠端執行任何 coding agent，OAB image 中不需綑綁 CLI。

```
┌─────────┐       ┌─────────┐        ┌───────────────┐         ┌──────────────────────────┐
│ Discord │       │         │  ACP   │               │  AWS    │   AgentCore Runtime      │
│  Slack  │──────▶│   OAB   │───────▶│ agentcore-acp │──────▶  │   ┌──────────────────┐   │
│Telegram │       │         │ stdio  │   (adapter)   │  SDK    │   │ Firecracker μVM  │   │
└─────────┘       └─────────┘        └───────────────┘         │   │  Kiro / Claude…  │   │
                                                               │   │  /mnt/workspace  │   │
                                                               │   └──────────────────┘   │
                                                               └──────────────────────────┘
```

```toml
[agentcore]
runtime_arn = "arn:aws:bedrock-agentcore:us-east-1:123456789012:runtime/my-agent"
```

更小的 image（約 50MB）、持久化 filesystem、隔離的 microVM，以及按用量計費。完整設定請參閱 [docs/agentcore.md](docs/agentcore.md)。

## 設定參考

> 📖 所有選項、預設值與 Helm mapping 的完整參考：[docs/config-reference.md](docs/config-reference.md)

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"   # supports env var expansion
allowed_channels = ["123456789"]      # channel ID allowlist
# allowed_users = ["987654321"]       # user ID allowlist (empty = all users)

[slack]
bot_token = "${SLACK_BOT_TOKEN}"     # Bot User OAuth Token (xoxb-...)
app_token = "${SLACK_APP_TOKEN}"     # App-Level Token (xapp-...) for Socket Mode
allowed_channels = ["C0123456789"]   # channel ID allowlist (empty = allow all)
# allowed_users = ["U0123456789"]    # user ID allowlist (empty = allow all)

[agent]
# command, args, and working_dir default from OPENAB_AGENT_COMMAND and $HOME
# env = { OPENAI_API_KEY = "${OPENAI_API_KEY}" }

[pool]
max_sessions = 10                     # max concurrent sessions
session_ttl_hours = 24                # idle session TTL

[reactions]
enabled = true                        # enable emoji status reactions
remove_after_reply = false            # remove reactions after reply
```

## Kubernetes 部署

Docker image 將 `openab` 與 `kiro-cli` 綑綁在同一個 container 中。

```
┌─ Kubernetes Pod ──────────────────────────────────────┐
│  openab (PID 1)                                       │
│    └─ kiro-cli acp --trust-all-tools (child process)  │
│       ├─ stdin  ◄── JSON-RPC requests                 │
│       └─ stdout ──► JSON-RPC responses                │
│                                                       │
│  PVC (/data)                                          │
│    ├─ ~/.kiro/                  (settings, sessions)  │
│    └─ ~/.local/share/kiro-cli/  (OAuth tokens)        │
└───────────────────────────────────────────────────────┘
```

### 不使用 Helm 部署

```bash
kubectl create secret generic openab-secret \
  --from-literal=discord-bot-token="your-token"

kubectl apply -f k8s/configmap.yaml
kubectl apply -f k8s/pvc.yaml
kubectl apply -f k8s/deployment.yaml
```

| Manifest | 用途 |
|----------|------|
| `k8s/deployment.yaml` | 單一 container pod，掛載 config 與 data volume |
| `k8s/configmap.yaml` | `config.toml` 掛載至 `/etc/openab/` |
| `k8s/secret.yaml` | 透過 env 注入 `DISCORD_BOT_TOKEN` |
| `k8s/pvc.yaml` | 持久保存驗證資訊與設定 |

## AWS ECS 部署

偏好以 AWS 原生 infrastructure 取代 Kubernetes？[`oabctl`](operator/) 是一個 CLI，可在 Amazon ECS Fargate 上佈建與管理 OpenAB agent——一個 command 即可初始化 cluster、IAM、S3 與 networking，另一個 command 則可部署 agent，包括自動佈建 Telegram/LINE webhook ingress（API Gateway → VPC Link → Cloud Map）。

```bash
oabctl bootstrap                            # one-time infra setup
oabctl create my-bot && oabctl apply -f my-bot/manifest.yaml --wait
```

完整指南請參閱 **[docs/oabctl.md](docs/oabctl.md)**，內容涵蓋安裝、manifest schema、ingress/webhooks、secrets 與 bootstrap。

## 靈感來源

- [sample-acp-bridge](https://github.com/aws-samples/sample-acp-bridge) — ACP protocol 與 process pool 架構
- [OpenClaw](https://github.com/openclaw/openclaw) — StatusReactionController emoji pattern

## 授權條款

MIT
