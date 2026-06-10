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
  → Processes: image → LLM, audio → STT, text_file → code block
  → File auto-evicted after 2 minutes
```

## Platform Support Matrix

| Platform | Images | Audio/Voice | Text Files | Video | Binary Files |
|----------|--------|-------------|------------|-------|--------------|
| **Discord** | ✅ | ✅ (STT) | ✅ | metadata only | skipped |
| **Telegram** | ✅ | ✅ (STT) | ✅ (whitelist) | skipped | skipped |
| **Feishu** | ✅ | ✅ (STT) | ✅ (whitelist) | skipped | skipped |
| **Google Chat** | ✅ | ✅ (STT) | ✅ (whitelist) | skipped | Drive files skipped |
| **WeCom** | ✅ | — | ✅ (whitelist) | skipped | skipped |
| **LINE** | ✅ (LINE-hosted only) | — | — | — | — |
| **Slack** | ✅ | ✅ (STT) | ✅ | — | skipped |

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
3. Core reads bytes → STT transcription (Whisper/Groq) → `[Voice message transcript]: ...`
4. If STT disabled: silently skipped

### Text Files (Documents)

1. Gateway downloads file
2. Extension whitelist check: `.txt`, `.csv`, `.md`, `.json`, `.yaml`, `.rs`, `.py`, `.js`, `.ts`, `.go`, `.java`, `.c`, `.cpp`, `.sh`, `.sql`, `.html`, `.css`, `.toml`, `.xml`, `.ini`, `.cfg`, `.conf`, etc.
3. UTF-8 validation — non-UTF-8 files rejected
4. Stored to filesystem
5. Core reads → wraps in markdown code block: `` ```filename.ext\n<content>\n``` ``

### Unsupported Types

Binary files (zip, pdf, exe, docx), video, and stickers are **silently skipped**. The agent does not receive any notification that a file was sent.

## Size Limits

| Type | Max Size | Enforced By |
|------|----------|-------------|
| Images | 10 MB | Gateway (pre-download Content-Length + post-download bytes) |
| Audio | 20 MB | Gateway |
| Text files | 20 MB | Gateway (same as store cap) |
| GIF passthrough | 5 MB | `resize_and_compress()` |
| Store (defense-in-depth) | 20 MB | `store_media()` |

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
