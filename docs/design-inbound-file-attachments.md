# Design: Inbound File Attachment Support

## Problem

目前 openAB 對非圖片/非音訊/非文字的檔案（PDF、docx、xlsx、video、zip 等）**靜默丟棄**，agent 完全不知道用戶傳了檔案。

## Solution Overview

所有未處理的 inbound 檔案 → download to local disk → 在 prompt 中注入 metadata + 路徑 → agent 用 file-read tool 自行處理 → TTL 過期自動清除。

---

## Architecture Diagrams

### Current State（現狀）

```mermaid
flowchart TD
    User[用戶傳檔案] --> Adapter{Platform Adapter}
    Adapter -->|image/*| IMG[下載 → resize → base64 → prompt]
    Adapter -->|audio/*| STT[下載 → STT 轉錄 → prompt]
    Adapter -->|text file| TXT[下載 → inline 內容 → prompt]
    Adapter -->|PDF/docx/zip...| DROP[❌ 靜默丟棄]
    
    IMG --> LLM[LLM]
    STT --> LLM
    TXT --> LLM
    DROP -.->|agent 看不到| LLM
```

### Proposed State（新設計）

```mermaid
flowchart TD
    User[用戶傳檔案] --> Adapter{Platform Adapter}
    Adapter -->|image/*| IMG[下載 → resize → base64 → prompt]
    Adapter -->|audio/*| STT[下載 → STT 轉錄 → prompt]
    Adapter -->|text file| TXT[下載 → inline 內容 → prompt]
    Adapter -->|其他檔案| DISK[download_to_disk]
    
    DISK --> Store[存到 /tmp/openab-attachments/msg_id/filename]
    Store --> Inject[注入 metadata block 到 prompt]
    Inject --> LLM[LLM + Agent]
    LLM -->|agent 需要時| Read[Agent 用 file-read tool 讀取]
    Read --> Store
    
    Store --> TTL[TTL eviction loop 定期清理]
    
    IMG --> LLM
    STT --> LLM
    TXT --> LLM
```

---

## Detailed Flow: Discord / Slack（直連平台）

```mermaid
sequenceDiagram
    participant U as 用戶
    participant D as Discord/Slack
    participant OAB as openAB 主程式
    participant FS as Local Filesystem
    participant Agent as LLM Agent

    U->>D: 傳送 report.pdf
    D->>OAB: Webhook/Event (attachment URL + metadata)
    
    Note over OAB: 判斷 MIME type
    Note over OAB: 不是 image/audio/text → download_to_disk
    
    OAB->>D: GET attachment URL (+ Bearer token for Slack)
    D-->>OAB: File bytes
    OAB->>FS: 寫入 /tmp/openab-attachments/{msg_id}/report.pdf
    
    Note over OAB: 構建 metadata content block
    OAB->>Agent: prompt + metadata block:<br/>[File attachment received]<br/>filename: report.pdf<br/>mimetype: application/pdf<br/>size: 2.3 MB<br/>path: /tmp/openab-attachments/{msg_id}/report.pdf
    
    Agent->>FS: read_file(/tmp/.../report.pdf)
    FS-->>Agent: file contents
    Agent->>U: 回覆（基於檔案內容）
    
    Note over FS: 10 分鐘後 eviction loop 刪除
```

## Detailed Flow: Gateway 平台（Telegram/飛書/WeChat）

```mermaid
sequenceDiagram
    participant U as 用戶
    participant TG as Telegram API
    participant GW as Gateway Process
    participant FS as Local Filesystem
    participant OAB as openAB 主程式
    participant Agent as LLM Agent

    U->>TG: 傳送 report.pdf
    TG->>GW: Webhook (file_id)
    
    GW->>TG: getFile(file_id) + bot_token
    TG-->>GW: file_path
    GW->>TG: GET /file/bot{token}/{file_path}
    TG-->>GW: File bytes
    
    GW->>FS: store_media(bytes) → /home/.openab/media/inbound/{uuid}
    GW->>OAB: WebSocket event:<br/>attachment_type: "document"<br/>filename: "report.pdf"<br/>mime_type: "application/pdf"<br/>path: "/home/.openab/media/inbound/{uuid}"
    
    OAB->>FS: read file from path
    Note over OAB: 構建 metadata content block
    OAB->>Agent: prompt + metadata block:<br/>[File attachment received]<br/>path: /tmp/openab-attachments/{msg_id}/report.pdf
    
    Agent->>FS: read_file(path)
    FS-->>Agent: file contents
    Agent->>U: 回覆
    
    Note over FS: Gateway: 2 min TTL 刪原始檔<br/>openAB: 10 min TTL 刪 copy
```

---

## Component Changes

### 1. `src/media.rs` — 新增 `download_to_disk()`

```mermaid
flowchart LR
    A[download_to_disk] --> B{URL empty?}
    B -->|Yes| C[return None]
    B -->|No| D[HTTP GET + auth_token]
    D --> E{size > MAX?}
    E -->|Yes| F[log warn, return None]
    E -->|No| G[sanitize filename]
    G --> H[write to disk]
    H --> I[return ContentBlock::Text with metadata]
```

**函數簽名：**
```rust
pub async fn download_to_disk(
    url: &str,
    filename: &str,
    mime_type: &str,
    size: u64,
    bucket_id: &str,
    auth_token: Option<&str>,
) -> Option<ContentBlock>
```

**輸出的 ContentBlock 內容：**
```
<<<EXTERNAL_UNTRUSTED_CONTENT>>>
[File attachment received — not auto-parsed by OpenAB]
- filename: report.pdf
- mimetype: application/pdf
- size: 2.3 MB
- local_path: /tmp/openab-attachments/1234567890/report.pdf

Use your file-reading tools to access this file if needed.
<<<END_EXTERNAL_UNTRUSTED_CONTENT>>>
```

### 2. Adapter 改動

```mermaid
flowchart TD
    subgraph "Discord adapter (src/discord.rs)"
        D1[for attachment in msg.attachments] --> D2{is_audio?}
        D2 -->|Yes| D3[STT]
        D2 -->|No| D4{is_text_file?}
        D4 -->|Yes| D5[inline text]
        D4 -->|No| D6[download_and_encode_image]
        D6 -->|Ok| D7[image block]
        D6 -->|NotAnImage| D8{is_video?}
        D8 -->|Yes| D9[video URL block]
        D8 -->|No| D10[✨ download_to_disk ✨]
    end
    
    subgraph "Slack adapter (src/slack.rs)"
        S1[for file in files] --> S2{is_audio?}
        S2 -->|Yes| S3[STT]
        S2 -->|No| S4{is_text_file?}
        S4 -->|Yes| S5[inline text]
        S4 -->|No| S6[download_and_encode_image]
        S6 -->|Ok| S7[image block]
        S6 -->|NotAnImage| S10[✨ download_to_disk ✨]
    end
```

### 3. Gateway 改動

```mermaid
flowchart TD
    subgraph "Gateway main (src/gateway.rs)"
        G1[for att in event.content.attachments] --> G2{attachment_type?}
        G2 -->|image| G3[base64 → ContentBlock::Image]
        G2 -->|text_file| G4[UTF-8 → code block]
        G2 -->|audio| G5[STT]
        G2 -->|document ✨| G6[read file → metadata block]
        G2 -->|unknown| G7[✨ same as document ✨]
    end
    
    subgraph "Telegram adapter (gateway)"
        T1[TelegramMessage] --> T2{photo?}
        T2 -->|Yes| T3[download_telegram_media Image]
        T2 -->|No| T4{document?}
        T4 -->|Yes| T5{is_text?}
        T5 -->|Yes| T6[download_telegram_document text]
        T5 -->|No| T7[✨ download_telegram_document binary ✨]
        T4 -->|No| T8{voice/audio?}
    end
```

### 4. Cleanup 機制

```mermaid
flowchart TD
    subgraph "Eviction (existing gateway pattern)"
        Loop[每 60 秒掃描一次] --> Scan[讀取 /tmp/openab-attachments/]
        Scan --> Check{file age > TTL?}
        Check -->|Yes| Del[刪除檔案]
        Check -->|No| Skip[保留]
        Del --> Loop
        Skip --> Loop
    end
    
    subgraph "Config"
        TTL[OPENAB_ATTACHMENTS_TTL_SECS = 600]
        MAX[OPENAB_ATTACHMENTS_MAX_BYTES = 500MB]
        DIR[OPENAB_ATTACHMENTS_DIR = /tmp/openab-attachments]
    end
```

---

## Security Checklist

| Item | Status |
|------|--------|
| Filename sanitization（path separators, null bytes, `..`） | 必做 |
| Path containment check（resolve 後確認在 target dir 內） | 必做 |
| `<<<EXTERNAL_UNTRUSTED_CONTENT>>>` markers | 必做 |
| Per-file size cap（configurable, default 200MB） | 必做 |
| Total dir size cap（500MB default） | 必做 |
| TTL eviction（10 min default） | 必做 |
| SSRF guard on download URL（block private IP ranges） | 可選（Discord/Slack URL 已知安全）|

---

## Config (config.toml)

```toml
[attachments]
enabled = true
dir = "/tmp/openab-attachments"
max_file_size_mb = 200
max_total_size_mb = 500
ttl_secs = 600
```

---

## Scope

### Phase 1（本次 PR）
- Discord + Slack: download_to_disk for unhandled files
- Gateway: 新增 `document` attachment_type 處理
- Telegram adapter: binary document 支援
- Eviction loop
- Security: filename sanitize + path containment + markers

### Phase 2（後續）
- 飛書/WeChat/Google Chat adapter 同步支援
- PDF text extraction（optional，像 OpenClaw 那樣）
- Config-driven allowlist/blocklist
- Metrics/logging for attachment types
