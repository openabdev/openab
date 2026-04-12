# Speech-to-Text (STT) for Voice Messages

openab can automatically transcribe Discord voice message attachments and forward the transcript to your ACP agent as text.

## Quick Start

Add an `[stt]` section to your `config.toml`:

```toml
[stt]
enabled = true
```

If `GROQ_API_KEY` is set in your environment, that's all you need — openab will auto-detect it and use Groq's free tier. You can also set the key explicitly:

```toml
[stt]
enabled = true
api_key = "${GROQ_API_KEY}"
```

## How It Works

```
Discord voice message (.ogg)
       │
       ▼
  openab downloads the audio file
       │
       ▼
  POST /audio/transcriptions  →  STT provider
       │
       ▼
  transcript injected as:
  "[Voice message transcript]: <transcribed text>"
       │
       ▼
  ACP agent receives plain text
```

The transcript is prepended to the prompt as a `ContentBlock::Text`, so the downstream agent (Kiro CLI, Claude Code, etc.) sees it as regular text input.

## Configuration Reference

```toml
[stt]
enabled = true                              # default: false
api_key = "${GROQ_API_KEY}"                 # required for cloud providers
model = "whisper-large-v3-turbo"            # default
base_url = "https://api.groq.com/openai/v1" # default
```

| Field | Required | Default | Description |
|---|---|---|---|
| `enabled` | no | `false` | Enable/disable STT. When disabled, audio attachments are silently skipped. |
| `api_key` | no* | — | API key for the STT provider. *Auto-detected from `GROQ_API_KEY` env var if not set. For local servers, use any non-empty string (e.g. `"not-needed"`). |
| `model` | no | `whisper-large-v3-turbo` | Whisper model name. Varies by provider. |
| `base_url` | no | `https://api.groq.com/openai/v1` | OpenAI-compatible API base URL. |

## Deployment Options

openab uses the standard OpenAI-compatible `/audio/transcriptions` endpoint. Any provider that implements this API works — just change `base_url`.

### Option 1: Groq Cloud (recommended, free tier)

```toml
[stt]
enabled = true
api_key = "${GROQ_API_KEY}"
```

- Free tier with rate limits
- Model: `whisper-large-v3-turbo` (default)
- Sign up at https://console.groq.com

### Option 2: OpenAI

```toml
[stt]
enabled = true
api_key = "${OPENAI_API_KEY}"
model = "whisper-1"
base_url = "https://api.openai.com/v1"
```

- ~$0.006 per minute of audio
- Model: `whisper-1`

### Option 3: Local Whisper Server

For users running openab on a Mac Mini, home lab, or any machine with a local whisper server:

```toml
[stt]
enabled = true
api_key = "not-needed"
model = "large-v3-turbo"
base_url = "http://localhost:8080/v1"
```

- Audio stays local — never leaves your machine
- No API key or cloud account needed
- Apple Silicon users get hardware acceleration

Compatible local whisper servers:

| Server | Install | Apple Silicon |
|---|---|---|
| [faster-whisper-server](https://github.com/fedirz/faster-whisper-server) | `pip install faster-whisper-server` | ✅ CoreML |
| [whisper.cpp server](https://github.com/ggerganov/whisper.cpp) | `brew install whisper-cpp` | ✅ Metal |
| [LocalAI](https://github.com/mudler/LocalAI) | Docker or binary | ✅ |

### Option 4: LAN / Sidecar Server

Point to a whisper server running on another machine in your network:

```toml
[stt]
enabled = true
api_key = "not-needed"
base_url = "http://192.168.1.100:8080/v1"
```

### Not Supported

- **Ollama** — does not expose an `/audio/transcriptions` endpoint.

## Helm Chart (Kubernetes)

When deploying via the openab Helm chart, STT is a first-class config block — no manual configmap patching needed:

```bash
helm upgrade openab openab/openab \
  --set agents.kiro.stt.enabled=true \
  --set agents.kiro.stt.apiKey=gsk_xxx
```

The API key is stored in a K8s Secret and injected as an env var (never in plaintext in the configmap). You can also customize model and endpoint:

```bash
helm upgrade openab openab/openab \
  --set agents.kiro.stt.enabled=true \
  --set agents.kiro.stt.apiKey=gsk_xxx \
  --set agents.kiro.stt.model=whisper-large-v3-turbo \
  --set agents.kiro.stt.baseUrl=https://api.groq.com/openai/v1
```

## Disabling STT

Omit the `[stt]` section entirely, or set:

```toml
[stt]
enabled = false
```

When disabled, audio attachments are silently skipped with no impact on existing functionality.

## Technical Notes

- openab sends `response_format=json` in the transcription request to ensure the response is always parseable JSON. Some local whisper servers default to plain text output without this parameter.
- The actual MIME type from the Discord attachment is passed through to the STT API (e.g. `audio/ogg`, `audio/mp4`, `audio/wav`).
- Environment variables in config values are expanded via `${VAR}` syntax (e.g. `api_key = "${GROQ_API_KEY}"`).
- The `api_key` field is auto-detected from the `GROQ_API_KEY` environment variable when using the default Groq endpoint. If you set a custom `base_url` (e.g. local server), auto-detect is disabled to avoid leaking the Groq key to unrelated endpoints — you must set `api_key` explicitly.
