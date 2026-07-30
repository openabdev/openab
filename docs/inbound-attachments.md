# Inbound Attachments

How OAB handles images, audio, and files sent by users across all platforms.

## Architecture

```
User sends media (photo/voice/file)
  → Platform webhook delivers to Gateway
  → Gateway downloads via platform API (auth stays in Gateway)
  → Image: resize ≤1200px, JPEG compress (GIF passthrough ≤5MB)
  → Store to ~/.openab/media/inbound/<uuid>
  → WS event includes file path in attachments[].path
  → Core reads from disk (zero encoding overhead)
  → Processes: image → LLM, audio → metadata block (+ STT when enabled), text_file → code block
  → File auto-evicted after 2 minutes
```

## Platform Support Matrix

| Platform | Images | Audio/Voice | Text Files | Video | Binary Files |
|----------|--------|-------------|------------|-------|--------------|
| **Discord** | ✅ | ✅ (file + STT) | ✅ | metadata + CDN URL | `[File: ...]` via filestore, else skipped |
| **Telegram** | ✅ | ✅ (file + STT) | ✅ (whitelist) | skipped | skipped |
| **Feishu** | ✅ | ✅ (file + STT) | ✅ (whitelist) | skipped | skipped |
| **Google Chat** | ✅ | ✅ (file + STT) | ✅ (whitelist) | skipped | Drive files skipped |
| **WeCom** | ✅ | — | ✅ (whitelist) | skipped | skipped |
| **LINE** | ✅ (LINE-hosted only) | ✅ (file + STT, 1:1 only, LINE-hosted only) | — | — | — |
| **LINE WORKS** | ✅ | ✅ (file + STT) | ✅ (whitelist) | skipped | skipped |
| **Slack** | ✅ | ✅ (file + STT) | ✅ | metadata + URL | `[File: ...]` via filestore, else skipped |

"file + STT" means the agent always receives the audio file's metadata (and a
fetchable URL where one exists), with the STT transcript added on top when
enabled. See [Audio / Voice Messages](#audio--voice-messages).

## Processing Pipeline

### Images

1. Gateway downloads from platform API
2. `resize_and_compress()` — longest side ≤1200px, JPEG quality 75
3. GIFs ≤5MB passed through unchanged (preserves animation)
4. Stored to `~/.openab/media/inbound/<uuid>`
5. Core reads bytes → `ContentBlock::Image` → sent to LLM

### Downstream Image Requirements

OpenAB can create the ACP image block, but downstream coding agents and selected models must also support image input. For local `llama.cpp` examples, see [Local OpenAI-Compatible Vision Models](local-vision-models.md).

### Audio / Voice Messages

1. Gateway downloads raw audio (ogg/m4a/mp3)
2. Stored to filesystem (no transcoding)
3. Core always emits an `[Audio attachment]` metadata block so skills can process the original file
4. If STT is enabled, transcription (Whisper/Groq) adds `[Voice message transcript]: ...` on top

The metadata block is emitted regardless of the STT setting. A transcript augments
the file, it never replaces it:

```
[Audio attachment]
filename: meeting.m4a
content_type: audio/mp4
size_bytes: 8342016
url: https://<presigned-or-platform-url>
note: presigned URL, expires in 60 minutes
```

Which `url` the agent gets depends on the platform and whether a
[filestore](filestore.md) is configured:

| Platform | With filestore | Without filestore |
|----------|----------------|-------------------|
| Discord | presigned S3 URL | `cdn.discordapp.com` URL, expires ~24h |
| Slack | presigned S3 URL | `url_private_download`, needs an `Authorization: Bearer <bot token>` header |
| Gateway (Telegram / Feishu / LINE / LINE WORKS / Google Chat) | presigned S3 URL | no `url` line (the gateway already consumed the platform URL during download), so the block carries metadata only |

**Slack caveat.** Slack forwards an attachment only when its file JSON carries
`url_private_download` or `url_private`. When both are absent the whole
attachment is skipped before its type is examined, audio included, so the
guarantee on Slack reads "always emitted, provided Slack returned a private
URL."

Gateway attachments reach Core as bytes (base64 or a colocate path), never as a
platform URL, so a filestore is the only way to give the agent a fetchable link
on those platforms. The colocate path is deliberately not exposed: it is evicted
after 2 minutes, so it would be dead by the time most skills fetch it.

**Count bound.** Audio has no per-message count cap of its own, unlike text
files (`TEXT_FILE_COUNT_CAP = 5`). The text cap exists to protect the prompt
from inlined content and is bypassed as soon as a filestore takes over the
upload; audio bytes are never inlined, so that reason does not apply. The bound
is the platform's own per-message file limit, 10 on both Slack and Discord,
combined with the per-file `max_file_size`.

LINE-specific note:
- LINE voice-message STT currently works in **1:1 chats only**.
- LINE group/room voice messages are still blocked by mention gating because LINE does not attach mention metadata to audio messages.

### Video

Discord and Slack forward video as a `[Video attachment]` metadata block rather
than inlining it. The gateway platforms have no video branch and skip it.

```
[Video attachment]
filename: standup.mp4
content_type: video/mp4
size_bytes: 24117248
url: https://<bucket>.s3.<region>.amazonaws.com/incoming/standup.mp4?X-Amz-Signature=...
note: presigned URL, expires in 60 minutes
```

Which URL the agent gets:

| Platform | Filestore configured | No filestore |
|---|---|---|
| Discord | `attachment.url`, a public CDN link needing no credentials | same |
| Slack | presigned S3 URL | `url_private_download`, which needs an `Authorization: Bearer <bot token>` header |

Discord video deliberately never reaches the filestore: the CDN link already
resolves without credentials, so uploading a copy would buy nothing. The
filestore branch in `discord.rs` excludes video for that reason.

The `note:` line is present only when the URL needs an explanation, so a Discord
CDN link carries no note. On Slack the note always appears, because neither form
is self-explanatory: the presigned URL expires, and the fallback needs a
credential the agent does not hold. In that last case the block is effectively
metadata plus an explanation of why the link will not resolve, which is still
more actionable than an unannotated dead URL.

### Text Files (Documents)

1. Gateway downloads file
2. Extension whitelist check: `.txt`, `.csv`, `.md`, `.json`, `.yaml`, `.rs`, `.py`, `.js`, `.ts`, `.go`, `.java`, `.c`, `.cpp`, `.sh`, `.sql`, `.html`, `.css`, `.toml`, `.xml`, `.ini`, `.cfg`, `.conf`, etc.
3. UTF-8 validation — non-UTF-8 files rejected
4. Stored to filesystem
5. Core reads → wraps in markdown code block: `` ```filename.ext\n<content>\n``` ``

### Unsupported Types

On the gateway platforms, binary files (zip, pdf, exe, docx), video, and stickers are **rejected with a status reason**. The agent receives a `[System: attachment "..." was not delivered — unsupported format: ...]` notification so it can inform the user.

Discord and Slack do not reject these: video goes through [Video](#video), and binary files go through the filestore as a `[File: ...]` block when one is configured.

## Size Limits

| Type | Max Size | Enforced By |
|------|----------|-------------|
| Images | 10 MB | Gateway (pre-download Content-Length + post-download bytes) |
| Audio (gateway platforms) | 20 MB | Gateway |
| Audio and video (Discord, Slack) | `max_file_size_mb` (default 250 MB) | Filestore; over the cap the block is still delivered with the platform URL |
| Text files | 20 MB | Gateway (same as store cap) |
| GIF passthrough | 5 MB | `resize_and_compress()` |
| Store (defense-in-depth) | 20 MB | `store_media()` |

## Pre-Dispatch Limits (Gateway WebSocket Path)

Attachment bytes are fetched inside the per-event task rather than on the
WebSocket receive path, so a slow object-storage transfer cannot stop the socket
from reading the next event (a `/cancel` included). Three limits bound what that
concurrency can cost, all compile-time constants in `gateway.rs` with no config
key: they are safety valves, not tuning knobs, and an operator who reaches them
has a load problem to report rather than a value to raise.

| Limit | Value | Effect when reached |
|-------|-------|---------------------|
| Concurrent attachment fetches | 4 | Further events queue for a slot; nothing is dropped |
| Pending pre-dispatch events | 32 | The next event's attachment bytes are **not** fetched. The agent still receives the message, carrying the same `[System: attachment ... was not delivered ...]` line a platform-side rejection produces, with the pending-attachment limit named as the reason |
| Tracked thread keys | 256 | Idle threads are forgotten; keys with work in flight are kept |

Two ordering properties survive the move:

- **Arrival order per thread.** Each event takes a ticket at receipt and waits for
  the previous same-thread event before reaching the dispatcher, so a voice note
  that takes 30 seconds to upload cannot be overtaken by the text sent after it.
  Different threads never wait on each other.
- **`/reset` beats work in flight.** A reset invalidates every event admitted
  before it, so a message still being prepared is dropped instead of landing in
  the new session. The `Dropped n buffered message(s)` count in the reset reply
  covers buffered messages only; anything still being prepared is dropped with an
  `info!` log and is not counted.

## Storage (Colocate Mode)

Media is stored at `~/.openab/media/inbound/<uuid>`:

- **Filenames**: Server-generated UUID v4, no extension (MIME type in event payload)
- **TTL**: 2 minutes — background task evicts expired files every 30 seconds
- **Trust boundary**: Gateway and Core share the same `$HOME` (same pod / sidecar)
- **No auth required**: Core reads directly from filesystem, no HTTP/token needed

### Security

- **Path traversal**: Impossible — filenames are UUID only, never user-supplied
- **Token leakage**: Platform auth tokens (Telegram bot token, LINE access token, Feishu tenant token) stay in Gateway, never reach Core or agent
- **Disk exhaustion**: TTL eviction + size limits prevent unbounded growth
- **No executable content**: Files are raw data, never executed

### Future: HTTP Proxy Mode

For separated deployments (Gateway ≠ Core pod), a future PR will add `GET /media/<uuid>` on the Gateway, allowing Core to fetch via internal HTTP. The `attachments[].path` field will be replaced by `attachments[].url` in that mode.

## Configuration

No additional configuration required. The filesystem store is always active when Gateway is running. Ensure Gateway and Core share the same `$HOME` (default in Helm colocate/sidecar mode).

## Related

- [Local OpenAI-Compatible Vision Models](local-vision-models.md) — Local vision model setup for Pi and OpenCode
- [Telegram](telegram.md) — Telegram-specific behavior and limitations
- [Feishu](feishu.md) — Feishu image/file/audio handling
- [Google Chat](google-chat.md) — Google Chat attachment support
- [STT (Speech-to-Text)](stt.md) — Audio transcription configuration
- [Sending Files (Outbound)](sendfiles.md) — Agent → user file delivery (separate mechanism)
