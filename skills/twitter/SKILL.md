---
name: twitter
description: Open and interact with X (Twitter) using agent-browser with persistent login profile.
openclaw:
  always: true
---

# twitter

> **SKILL_DIR** = the directory containing this SKILL.md file. Resolve all `scripts/` paths relative to it.

## SUMMARIZE_X

**YOU MUST execute these exact commands in order. Do NOT improvise or write your own collection logic.**

### Step 1: Open browser
```bash
# Kill stale daemon first (required for fresh credentials to take effect)
pkill -f agent-browser 2>/dev/null
sleep 2
# Export SSO credentials explicitly (agent-browser does not read AWS_PROFILE)
eval $(aws configure export-credentials --format env)
# Verify token before proceeding
aws sts get-caller-identity > /dev/null || { echo "Token expired"; exit 1; }
AGENTCORE_REGION=us-east-1 AGENTCORE_PROFILE_ID=pahudnet_gmail-attEctm8Qe \
  agent-browser -p agentcore open "https://x.com/home" 2>&1
sleep 5
```

### Step 2: Collect tweets (MUST use this script)
```bash
uv run ${SKILL_DIR}/scripts/collect_timeline.py
```

### Step 3: Filter candidates (MUST use this script)
```bash
uv run ${SKILL_DIR}/scripts/filter_candidates.py
```

### Step 4: Close browser
```bash
agent-browser close
```

### Step 5: Read candidates and generate summary

Read candidates with IDs — **use this exact command**:
```bash
python3 -c "
import json, re
d = json.load(open('/tmp/candidates_filtered.json'))
for i,t in enumerate(d[:200]):
    tag = '🤖' if t.get('_is_tech') else '📰'
    sid = t.get('id','')
    if not sid:
        m = re.search(r'/status/(\d+)', t.get('url',''))
        sid = m.group(1) if m else '?'
    print(f'{i+1}. {tag} [{t[\"_likes\"]}❤ {t[\"_replies\"]}💬] id={sid}')
    print(f'   {t[\"text\"][:200]}')
"
```

Then write `/tmp/index.txt` with this format:

```
## 📋 X 動態摘要（過去 24 小時）
最後更新：台北時間 YYYY-MM-DD HH:MM

### emoji 主題名
- 事件描述＋中立客觀的 insight/implication。（statusId）

### 📊 統計
> 共收集 N 條推文，精選 M 條摘要。

### 📝 總結
> 一段完整分析段落。
```

**Rules:**
- 繁體中文，中立報導語氣，不帶作者名
- 每則 MUST 有 event + insight（回答「所以呢？」），insight 必須中立客觀
- 每則結尾的 statusId MUST 來自上面印出的 id，禁止捏造或重複使用
- 目標 50-80 則，跳過純 meme、一句話、灌水互動的推文
- 分類用 emoji 標題（🤖 AI, 🦞 OpenClaw, 💰 財經, 🌐 地緣, 🏭 產業, 🛠️ 工具 等）

### Step 6: Upload (MUST use this script)
```bash
uv run ${SKILL_DIR}/scripts/upload.py
```

Published at: https://x.deepsrt.cc/index.txt

## SUMMARIZE_1

Summarize a single tweet by URL.

### Step 1: Open browser
```bash
agent-browser close 2>/dev/null
AGENTCORE_PROFILE_ID=pahudnet_gmail-attEctm8Qe agent-browser -p agentcore open "<TWEET_URL>"
sleep 5
```

### Step 2: Extract tweet content and metrics
```bash
agent-browser eval "JSON.stringify([...document.querySelectorAll('article')].slice(0,5).map(el => ({
  text: el.querySelector('[data-testid=\"tweetText\"]')?.textContent,
  author: el.querySelector('[data-testid=\"User-Name\"]')?.textContent
})))"
```

### Step 3: Close browser
```bash
agent-browser close
```

### Step 4: Generate summary

Output in 繁體中文:
- 推文串摘要（含原文、回覆脈絡）
- 互動數據（❤️ 🔁 💬 👁️）
- 一句話摘要：事件 + insight
