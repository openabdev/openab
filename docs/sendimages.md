# Sending Images Back to Discord

> **This doc is designed for your coding agent.** Share it with your agent so it learns how to send images back to Discord.
>
> Example prompt:
> ```
> Read docs/sendimages.md from OpenAB GitHub and send the image back to my Discord thread.
> ```
>
> 💡 **Tip:** If it works the first time, ask your agent to save this as a **SKILL** so it remembers how to do it next time without re-reading the doc.

---

OpenAB does **not** relay images from the agent to Discord — it only streams text.
To send an image back to the user, the agent must call the Discord API directly.

> For sending non-image files (PDF, CSV, logs, etc.), see [sendfiles.md](sendfiles.md).

## How It Works

```
┌──────────┐  text only   ┌──────────┐  ACP stdio   ┌──────────────┐
│  Discord  │◄────────────│  OpenAB   │◄────────────│  Agent (CLI)  │
│  Thread   │             └──────────┘              └──────┬───────┘
│           │                                              │
│           │         Discord REST API                     │
│           │◄─────────────────────────────────────────────┘
│           │  POST /channels/{thread_id}/messages
│           │  + multipart file attachment
└──────────┘
```

OpenAB only streams text via ACP. To send an image, the agent calls the
Discord API directly using the `thread_id` from `sender_context`.

## Step-by-Step

### 1. Get the Target Channel from `sender_context`

Every message from OpenAB includes a `<sender_context>` JSON block:

```json
{
  "schema": "openab.sender.v1",
  "sender_id": "845835116920307722",
  "sender_name": "pahud.hsieh",
  "display_name": "pahud.hsieh",
  "channel": "discord",
  "channel_id": "1490282656913559673",
  "thread_id": "1499442140172910654",
  "is_bot": false
}
```

Use **`thread_id`** as the target channel. If `thread_id` is absent, fall back to `channel_id`.

### 2. Get the Bot Token

The agent needs a Discord Bot Token to call the API. Pass it via `[agent] env` in `config.toml`:

```toml
[agent]
env = { DISCORD_BOT_TOKEN = "${DISCORD_BOT_TOKEN}" }
```

> ⚠️ The token in `[discord] bot_token` is consumed by OpenAB itself and is **not** automatically forwarded to the agent subprocess. You must explicitly pass it via `[agent] env`.

#### Security: dedicated bot recommended

For production, consider creating a **dedicated "File Deliverer" bot** with minimal permissions instead of sharing the main OAB bot token:

```
┌─────────────────────────────────────────────────────────────────┐
│ Discord Server                                                  │
│                                                                 │
│  ┌─────────────────┐         ┌──────────────────────────────┐   │
│  │  OAB Bot         │         │  File Deliverer Bot          │   │
│  │  (main bot)      │         │  (dedicated, minimal perms)  │   │
│  │                  │         │                              │   │
│  │  ✅ Read Messages │         │  ✅ Send Messages             │   │
│  │  ✅ Manage Threads│         │  ✅ Send Messages in Threads  │   │
│  │  ✅ Add Reactions │         │  ✅ Attach Files              │   │
│  │  ✅ Send Messages │         │  ❌ Read Messages             │   │
│  │  ✅ Attach Files  │         │  ❌ Manage Threads            │   │
│  └────────┬────────┘         └──────────────┬───────────────┘   │
│           │                                  │                   │
│           │  ACP stdio                       │  REST API         │
│           ▼                                  ▼                   │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  Agent (kiro-cli / claude / codex)                        │   │
│  │                                                           │   │
│  │  DISCORD_FILE_BOT_TOKEN ──► POST /channels/{id}/messages  │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

**What users see in the thread:**

```
超渡法師:        好的，我幫你生成報告...
File Deliverer:  📎 report.pdf
```

| | Same token (simple) | Dedicated bot (recommended) |
|---|---|---|
| Setup | One bot, one token | Create a second bot in Discord Developer Portal |
| Agent permissions | Everything the OAB bot can do | Only `Send Messages` + `Attach Files` |
| Token leak impact | Full bot access exposed | Limited to file uploads |
| Audit trail | Cannot distinguish OAB vs agent messages | Separate bot identity in logs |

```toml
# Production: dedicated file-upload bot (e.g. "File Deliverer")
[agent]
env = { DISCORD_FILE_BOT_TOKEN = "${DISCORD_FILE_BOT_TOKEN}" }
```

For personal use or small teams, sharing the same token is fine — you already trust your agent.

### 3. Upload the Image

Use the Discord [Create Message](https://discord.com/developers/docs/resources/message#create-message) endpoint with a `multipart/form-data` body:

```
POST https://discord.com/api/v10/channels/{thread_id}/messages
Authorization: Bot {DISCORD_BOT_TOKEN}
Content-Type: multipart/form-data
```

#### curl example

```bash
curl -X POST "https://discord.com/api/v10/channels/${THREAD_ID}/messages" \
  -H "Authorization: Bot ${DISCORD_BOT_TOKEN}" \
  -F "content=Here is the generated image" \
  -F "files[0]=@/path/to/image.png"
```

#### Python example

```python
import os, requests

def send_image(thread_id: str, image_path: str, message: str = ""):
    url = f"https://discord.com/api/v10/channels/{thread_id}/messages"
    headers = {"Authorization": f"Bot {os.environ['DISCORD_BOT_TOKEN']}"}
    with open(image_path, "rb") as f:
        requests.post(url, headers=headers,
                      data={"content": message},
                      files={"files[0]": (os.path.basename(image_path), f)})
```

#### Node.js example

```javascript
const fs = require("fs");
const FormData = require("form-data");

async function sendImage(threadId, imagePath, message = "") {
  const form = new FormData();
  form.append("content", message);
  form.append("files[0]", fs.createReadStream(imagePath));

  await fetch(`https://discord.com/api/v10/channels/${threadId}/messages`, {
    method: "POST",
    headers: { Authorization: `Bot ${process.env.DISCORD_BOT_TOKEN}` },
    body: form,
  });
}
```

## Automated Sidecar Pattern

If your agent generates images to a known directory (e.g. Codex writes to
`~/.codex/generated_images/`), you can run a **file-watcher sidecar** that
automatically uploads new images:

1. Watch the output directory for new files.
2. Read the session metadata to find the originating `thread_id`.
3. Upload via the Discord API.
4. Track uploaded files in a state file to avoid duplicates.

This is the pattern used by the community `discord-image-uploader` sidecar.

## Security Considerations

- **Never hardcode the bot token.** Read it from `$DISCORD_BOT_TOKEN` or a mounted secret.
- **Scope permissions.** The bot only needs `Send Messages` and `Attach Files` in the target channels.
- **Validate file paths.** If the agent constructs paths dynamically, sanitize them to prevent path traversal.
- **Rate limits.** Discord enforces rate limits on message creation. Space uploads if sending multiple images.

## Bot Permission Checklist

In the [Discord Developer Portal](https://discord.com/developers/applications), ensure your bot has:

- [x] `Send Messages`
- [x] `Send Messages in Threads`
- [x] `Attach Files`

These are typically already granted if your bot works with OpenAB.

## FAQ

**Q: Can OpenAB relay images natively?**
A: Not currently. OpenAB streams text via ACP JSON-RPC. Image/file sending is done out-of-band by the agent.

**Q: Does this work with Slack / Telegram / LINE?**
A: The same concept applies — call the platform's file upload API using the channel ID from `sender_context`. The API details differ per platform.

**Q: What image formats are supported?**
A: Discord supports PNG, JPEG, GIF, and WebP. Max file size is 25 MB (or higher with Nitro boost).
