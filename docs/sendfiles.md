# Sending Files Back to Discord

> **This doc is designed for your coding agent.** Share it with your agent so it learns how to send files back to Discord.
>
> Example prompt:
> ```
> Read docs/sendfiles.md from OpenAB GitHub and send the file back to my Discord thread.
> ```
>
> 💡 **Tip:** If it works the first time, ask your agent to save this as a **SKILL** so it remembers how to do it next time without re-reading the doc.

---

OpenAB streams text only via ACP — it does **not** relay file attachments inline.
To send a file back to the user, either configure `[agent].auto_upload_dirs` for
automatic post-turn upload (see [Auto-Upload Generated Files](#auto-upload-generated-files)
below), or have the agent call the Discord API directly.

> For image-specific guidance (formats, sidecar pattern), see [sendimages.md](sendimages.md).

## How It Works

### Direct Upload (small files)

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

### Enterprise / Large Files (presigned URL)

```
┌──────────┐  text only   ┌──────────┐  ACP stdio   ┌──────────────┐
│  Discord  │◄────────────│  OpenAB   │◄────────────│  Agent (CLI)  │
│  Thread   │             └──────────┘              └──────┬───────┘
│           │                                              │
│           │  send presigned URL as message                │  upload file
│           │◄─────────────────────────────────────────────┤─────────────►┌─────┐
│           │  POST /channels/{thread_id}/messages         │              │ S3  │
└─────┬────┘                                               │              │ R2  │
      │                                                    │              │ GCS │
      │  user clicks link                                  │              └──┬──┘
      └────────────────────────────────────────────────────────────────────►│
                              presigned GET                                 │
                              ◄─────────────────────────────────────────────┘
```

OpenAB only streams text via ACP. To send a file, use `[agent].auto_upload_dirs`
(recommended) or have the agent call the Discord API directly using the
`thread_id` from `sender_context`.

## Step-by-Step

### 1. Get the Target Channel from `sender_context`

Every message includes a `<sender_context>` JSON block:

```json
{
  "schema": "openab.sender.v1",
  "channel": "discord",
  "channel_id": "1490282656913559673",
  "thread_id": "1499442140172910654"
}
```

Use **`thread_id`** as the target. Fall back to `channel_id` if `thread_id` is absent.

### 2. Get the Bot Token

The agent needs a Discord Bot Token to call the API. Pass it via `[agent] env` in `config.toml`:

```toml
[agent]
env = { DISCORD_BOT_TOKEN = "${DISCORD_BOT_TOKEN}" }
```

> ⚠️ The token in `[discord] bot_token` is consumed by OpenAB itself and is **not** automatically forwarded to the agent subprocess. You must explicitly pass it via `[agent] env`.

#### Security: dedicated bot recommended

For production, consider creating a **dedicated "File Deliverer" bot** with minimal permissions instead of sharing the main OAB bot token. See [sendimages.md — Security: dedicated bot recommended](sendimages.md#security-dedicated-bot-recommended) for the full architecture diagram and comparison table.

```toml
# Production: dedicated file-upload bot (e.g. "File Deliverer")
[agent]
env = { DISCORD_FILE_BOT_TOKEN = "${DISCORD_FILE_BOT_TOKEN}" }
```

For personal use or small teams, sharing the same token is fine — you already trust your agent.

### 3. Upload the File

```
POST https://discord.com/api/v10/channels/{thread_id}/messages
Authorization: Bot {DISCORD_BOT_TOKEN}
Content-Type: multipart/form-data
```

#### curl example

```bash
curl -X POST "https://discord.com/api/v10/channels/${THREAD_ID}/messages" \
  -H "Authorization: Bot ${DISCORD_BOT_TOKEN}" \
  -F "content=Here is the report" \
  -F "files[0]=@/path/to/report.pdf"
```

#### Multiple files

Discord supports up to **10 attachments** per message:

```bash
curl -X POST "https://discord.com/api/v10/channels/${THREAD_ID}/messages" \
  -H "Authorization: Bot ${DISCORD_BOT_TOKEN}" \
  -F "content=Build artifacts" \
  -F "files[0]=@build.log" \
  -F "files[1]=@coverage.html"
```

#### Python example

```python
import os, requests

def send_file(thread_id: str, file_path: str, message: str = ""):
    url = f"https://discord.com/api/v10/channels/{thread_id}/messages"
    headers = {"Authorization": f"Bot {os.environ['DISCORD_BOT_TOKEN']}"}
    with open(file_path, "rb") as f:
        requests.post(url, headers=headers,
                      data={"content": message},
                      files={"files[0]": (os.path.basename(file_path), f)})
```

#### Node.js example

```javascript
const fs = require("fs");
const FormData = require("form-data");

async function sendFile(threadId, filePath, message = "") {
  const form = new FormData();
  form.append("content", message);
  form.append("files[0]", fs.createReadStream(filePath));

  await fetch(`https://discord.com/api/v10/channels/${threadId}/messages`, {
    method: "POST",
    headers: { Authorization: `Bot ${process.env.DISCORD_BOT_TOKEN}` },
    body: form,
  });
}
```

## File Size Limits

| Server Boost Level | Max Upload |
|--------------------|------------|
| None               | 25 MB      |
| Level 2            | 50 MB      |
| Level 3            | 100 MB     |

## Large Files & Enterprise Best Practice

For enterprise use or files exceeding Discord's upload limit, the recommended pattern is:

1. **Upload to external storage** — Amazon S3, Cloudflare R2, Google Drive, etc. using your own credentials.
2. **Generate a temporary link** — e.g. an [S3 presigned URL](https://docs.aws.amazon.com/AmazonS3/latest/userguide/ShareObjectPresignedURL.html) with a short TTL.
3. **Send the link back to Discord** — post the URL as a regular message in the thread.

> See the [Enterprise / Large Files diagram](#enterprise--large-files-presigned-url) above for the full flow.

#### Why this is better for enterprise

- **No file size limit** — S3/R2 handles files of any size.
- **Files stay off Discord** — you control where data lives, important for compliance and data governance.
- **You control the TTL** — the link expires on your terms; no permanent file sitting in a Discord CDN.

#### S3 presigned URL example (Python)

```python
import boto3

s3 = boto3.client("s3")

# Upload
s3.upload_file("/path/to/report.pdf", "my-bucket", "reports/report.pdf")

# Generate presigned URL (expires in 1 hour)
url = s3.generate_presigned_url(
    "get_object",
    Params={"Bucket": "my-bucket", "Key": "reports/report.pdf"},
    ExpiresIn=3600,
)

# Then send `url` as a Discord message via the API
```

## Common File Types

| Use Case | Typical Format | Notes |
|----------|---------------|-------|
| Code patches | `.diff`, `.patch` | Attach as file to avoid Discord's 2000-char limit |
| Logs | `.log`, `.txt` | Truncate or compress large logs |
| Reports | `.pdf`, `.csv`, `.html` | PDF renders a preview in Discord |
| Archives | `.zip`, `.tar.gz` | Bundle multiple files |

## Security Considerations

- **Never hardcode the bot token.** Read from `$DISCORD_BOT_TOKEN` or a mounted secret.
- **Validate file paths.** Sanitize any dynamically constructed paths to prevent path traversal.
- **Check file size** before uploading to avoid silent failures.
- **Sensitive content.** Do not send files containing secrets, credentials, or PII unless the user explicitly requests it.
- **Rate limits.** Discord enforces per-channel rate limits. Space uploads when sending multiple files.

## Bot Permission Checklist

Ensure your bot has these permissions in the [Discord Developer Portal](https://discord.com/developers/applications):

- [x] `Send Messages`
- [x] `Send Messages in Threads`
- [x] `Attach Files`

## FAQ

**Q: Can OpenAB relay files natively?**
A: Not currently. OpenAB streams text via ACP JSON-RPC. File sending is done out-of-band by the agent.

**Q: What if the file is too large?**
A: Use the [Large Files & Enterprise Best Practice](#large-files--enterprise-best-practice) pattern — upload to S3/R2/Google Drive, generate a temporary link, and send the URL in the message.

**Q: Does this work with Slack / Telegram / LINE?**
A: Same concept — call the platform's file upload API using the channel ID from `sender_context`. API details differ per platform. For Slack, use the v2 external upload flow (`files.getUploadURLExternal` + POST binary + `files.completeUploadExternal`); the legacy `files.upload` is deprecated. For Telegram, use [`sendDocument`](https://core.telegram.org/bots/api#senddocument).

**Q: My agent generates images (e.g. `image_gen`) but the user never sees them. How do I fix this?**
A: OpenAB does not relay inline images from ACP. Use `[agent].auto_upload_dirs` in `config.toml` to auto-upload files created during a turn:

```toml
[agent]
auto_upload_dirs = ["/home/node/.codex/generated_images"]
```

After each turn completes, OpenAB scans these directories for newly created files and uploads them to the originating Slack/Discord thread using the bot token. The Slack bot needs `files:write` scope. See below for the Slack-specific details.

## Auto-Upload Generated Files

When `[agent].auto_upload_dirs` is set, OpenAB automatically uploads files
created during an ACP turn to the originating thread. This is designed for
agent-generated assets (images, PDFs, reports) that the text-only ACP stream
cannot deliver.

### How it works

1. Before dispatching the ACP prompt, OpenAB records the current timestamp.
2. After the turn completes and text is delivered, it scans the configured
   directories (recursively, one level deep) for files whose mtime is newer
   than the turn start.
3. Each new file is uploaded to the thread via the platform's file upload API.
4. Upload failures are logged but non-fatal — the text reply is always delivered.

### Configuration

```toml
[agent]
auto_upload_dirs = ["/home/node/.codex/generated_images"]
```

Multiple directories are supported. Paths are absolute or relative to the
agent's working directory.

### Slack requirements

- Bot token must have `files:write` scope (add via api.slack.com/apps → OAuth & Permissions)
- The token is already available to OpenAB via `[slack] bot_token`; no additional
  `[agent] env` configuration is needed for the auto-upload (OpenAB uploads directly,
  not the agent subprocess)
