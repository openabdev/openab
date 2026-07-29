# Filestore — S3/R2-Compatible Object Store for File Attachments

## Problem

When a user attaches a file that OAB cannot inline into the prompt — either
because it's too large (text > 512 KB) or because it's an unsupported format
(PDF, ZIP, binary, etc.) — the file was previously **silently dropped**.
The agent never knew the file existed.

This affected all 7 platforms: Discord, Slack, Telegram, Feishu, Google Chat,
WeCom, and LINE.

PR #1346 proposed returning the platform's raw URL as a hint, but this has
fundamental limitations:

| Platform | Issue |
|----------|-------|
| Discord | CDN URLs expire in ~24 hours |
| Slack | `url_private_download` requires a Bearer token the agent does not have |
| Any | Agent may lack web-fetch capability or network access to platform CDNs |

## Solution

The `[filestore]` feature solves all three issues:

```
User attaches large text file (> 512 KB)
  → OAB downloads using its platform token (Slack Bearer / Discord CDN)
  → Uploads to user-configured S3/R2 bucket
  → Generates a presigned GET URL (configurable TTL)
  → Returns a ContentBlock::Text hint with the presigned URL
  → Agent fetches via bare HTTP GET (no auth needed)
```

Key insight: **OAB already has the platform credentials to download the file** —
it just wasn't using them for files above the inline limit.

## Configuration

Add a `[filestore]` section to your `config.toml`:

```toml
[filestore]
bucket = "my-oab-files"
region = "us-west-2"
prefix = "incoming/"       # object key prefix (default)
presigned_ttl = 3600       # URL expiry in seconds (default: 1 hour)
```

### With Cloudflare R2

**Recommended: use secret refs** (credentials resolved from AWS Secrets Manager
or exec provider at boot time — never stored in plaintext config):

```toml
[filestore]
bucket = "my-oab-files"
region = "auto"
endpoint = "https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
presigned_ttl = 3600
access_key_id = "${secrets.r2_access_key}"
secret_access_key = "${secrets.r2_secret_key}"

[secrets.refs]
r2_access_key = "aws-sm://openab/prod#R2_ACCESS_KEY_ID"
r2_secret_key = "aws-sm://openab/prod#R2_SECRET_ACCESS_KEY"
```

Alternative (env vars — acceptable for development, not recommended for production):

```toml
[filestore]
bucket = "my-oab-files"
region = "auto"
endpoint = "https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
presigned_ttl = 3600
access_key_id = "${R2_ACCESS_KEY_ID}"
secret_access_key = "${R2_SECRET_ACCESS_KEY}"
```

> **Security note:** Always prefer `[secrets.refs]` over env vars for R2/S3
> credentials. Secret refs support rotation, audit trails, and are never
> exposed in process listings or environment dumps.

### With AWS S3

```toml
[filestore]
bucket = "my-oab-files"
region = "us-west-2"
presigned_ttl = 3600
# Credentials resolved via AWS provider chain (IRSA, env vars, instance role)
```

### With MinIO (self-hosted)

```toml
[filestore]
bucket = "oab-uploads"
region = "us-east-1"
endpoint = "http://minio.internal:9000"
access_key_id = "${MINIO_ACCESS_KEY}"
secret_access_key = "${MINIO_SECRET_KEY}"
presigned_ttl = 7200
```

## Fields

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `bucket` | ✅ | — | S3 bucket name |
| `region` | ✅ | — | AWS region (`"auto"` for R2) |
| `endpoint` | ❌ | AWS default | Custom S3-compatible endpoint URL |
| `prefix` | ❌ | `"incoming/"` | Object key prefix |
| `presigned_ttl` | ❌ | `3600` | Presigned URL lifetime in seconds (clamped to 60 … 604800, i.e. 1 minute … 7 days) |
| `max_file_size_mb` | ❌ | `250` | Maximum file size for upload in MB (max 500) |
| `access_key_id` | ❌ | provider chain | Explicit access key |
| `secret_access_key` | ❌ | provider chain | Explicit secret key |

## Technical Details

### Streaming Multipart Upload

Files are uploaded using S3 multipart upload with streaming:

- **Chunk size:** 16 MB per part (above S3 minimum of 5 MB)
- **Memory usage:** ~16 MB per concurrent upload (fixed, regardless of file size)
- **Part count:** max ~32 parts for a 500 MB file (well below S3's 10,000 limit)
- **Timeout:** 10 minutes total for the streaming upload operation

The streaming approach means a 500 MB file uses the same ~16 MB of memory as a 1 MB file on the Discord/Slack streaming path. Gateway adapters use buffered single PUT (see Platform-Specific Behavior below).

### Platform-Specific Behavior

| Platform | Download Method | Upload Method | File Types |
|----------|----------------|---------------|------------|
| Discord | Streaming download | Streaming multipart (~16 MB chunks) | Text > 512KB + PDF/ZIP/binary + audio; video excluded, its CDN link already needs no credentials |
| Slack | Streaming download | Streaming multipart (~16 MB chunks) | Text > 512KB + PDF/ZIP/binary + audio + video, since neither Slack URL form is fetchable by the agent |
| Gateway (Telegram, Feishu, Google Chat, WeCom, LINE) | File on local disk | Single PUT | Text files delivered by adapter pipeline, plus audio bytes on the adapters that emit them (Telegram, Feishu, Google Chat, LINE; WeCom drops `voice` outright); binary limited by adapter validation |

Gateway adapters use their existing text-file pipeline (extension whitelist).
When filestore is configured, large text files (>512 KB) that pass through
the adapter are uploaded to S3/R2 instead of being inlined. Without filestore,
all text files are inlined regardless of size (original behavior preserved).
Full binary support requires a gateway schema change (tracked in #1349).

### Timeouts

| Operation | Timeout | Notes |
|-----------|---------|-------|
| Download from platform (streaming path) | 10 minutes | HTTP request including body streaming |
| Streaming upload to S3 | 10 minutes | Total for download + all parts + complete |
| Download from platform (inline path) | 30 seconds | For files expected ≤512 KB |
| Individual part upload | SDK default | Per upload_part call |

### Incomplete Multipart Upload Cleanup

If a streaming upload is interrupted (timeout, OAB crash, network failure),
S3 may retain incomplete multipart upload parts. These consume storage until
cleaned up.

**Required:** Configure an `AbortIncompleteMultipartUpload` lifecycle rule:

```json
{
  "Rules": [{
    "ID": "abort-incomplete-uploads",
    "Filter": { "Prefix": "incoming/" },
    "Status": "Enabled",
    "AbortIncompleteMultipartUpload": { "DaysAfterInitiation": 1 }
  }]
}
```

```bash
# Apply alongside your expiry rule
aws s3api put-bucket-lifecycle-configuration \
  --bucket my-oab-files \
  --lifecycle-configuration file://lifecycle.json
```

For Cloudflare R2, incomplete multipart uploads are automatically cleaned up
after 24 hours (no configuration needed).

## Behavior

> **Platform scope:**
> - **Discord / Slack:** All file types supported — text > 512KB, PDF, ZIP, binary,
>   and any unsupported format are uploaded to filestore via streaming multipart.
> - **Gateway (Telegram, Feishu, Google Chat, WeCom, LINE):** Filestore is wired for
>   files delivered by existing adapter pipelines. Large text files (>512KB) that pass
>   through the adapter are uploaded, as is audio, whose bytes the adapter already holds
>   and which therefore uses the buffered single PUT rather than the streaming path.
>   Binary/generic-file support remains limited by
>   current gateway adapter validation (UTF-8 checks, platform-specific size limits).
>   Full binary support requires a gateway schema change tracked in follow-up #1349.

| File size | Filestore configured | Result |
|-----------|---------------------|--------|
| Text ≤ 512 KB | any | Inlined into prompt (unchanged) |
| Text > 512 KB | ✅ yes | Uploaded → presigned URL returned |
| PDF, ZIP, DOCX, binary (Discord/Slack only) | ✅ yes | Uploaded → presigned URL returned |
| Audio (`audio/*` MIME, or `.ogg/.oga/.opus/.m4a/.mp3/.wav/.flac/.aac/.amr`) | ✅ yes | Uploaded, and the `[Audio attachment]` block's `url:` is the presigned URL |
| Audio | ❌ no | Not uploaded; the block still appears, carrying the platform URL where one exists |
| Video on Discord (`video/*` MIME or `.mp4/.mov/.m4v/.webm/.mkv/.avi`) | any | **Never uploaded.** The agent receives a `[Video attachment]` block whose `url:` is the CDN link, which already needs no credentials |
| Video on Slack | ✅ yes | Uploaded, because neither `url_private_download` nor `url_private` is fetchable by the agent |
| Video on Slack | ❌ no | Not uploaded; the block carries `url_private_download` plus a `note:` naming the bearer-token requirement |
| Text > 512 KB | ❌ no | Silently dropped (legacy behavior) |
| PDF, ZIP, DOCX, binary | ❌ no | Silently dropped (legacy behavior) |
| > max_file_size_mb (default 250 MB, max 500 MB) | ✅ yes | Text and binary dropped on Discord/Slack; audio and Slack video still delivered with the platform URL and a `note:` naming the size refusal; degraded hint on gateway (see Error Handling) |

## What the Agent Sees

When a file is uploaded to the filestore, the agent receives a text block like:

```
[File: test-results.txt]
This file (1024 KB) exceeds the 512 KB inline limit. It has been uploaded to
temporary storage. Fetch the contents using the URL below:
https://my-bucket.s3.us-west-2.amazonaws.com/incoming/abc123_test-results.txt?X-Amz-Algorithm=...
Note: this URL expires in 60 minutes.
```

The agent can then use any HTTP tool (`web-fetch`, `curl`, etc.) to download
the file — no authentication headers required.

## Security

### Credentials

- **Platform tokens (Slack/Discord)** stay server-side — never exposed to agent
- **S3 credentials** stay server-side — only used for upload + presigning
- **Presigned URLs** are time-limited and scoped to a single object

### Object Keys

Object keys are server-generated: `{prefix}{uuid}_{filename}`. The UUID
prevents collision and enumeration. The filename portion is sanitized before
being embedded in the key: path separators (`/`, `\`), traversal sequences
(`..`), double quotes, and non-ASCII characters are replaced or stripped,
and the name is capped at 200 characters.

### Filename Sanitization & Content-Disposition

- **S3 objects** are uploaded with `Content-Disposition: attachment` so
  browsers download the file instead of rendering it inline — this prevents
  HTML/JS content from executing in the bucket's origin when a presigned URL
  is opened in a browser. Double quotes are stripped from filenames so the
  header cannot be broken out of.
- **Agent-visible text** (inline file blocks and error/degraded hints) strips
  control characters from filenames and caps them at 200 characters,
  preventing prompt injection via crafted attachment filenames.

### Size Limits

- Per-file cap: configurable via `max_file_size_mb` (default 250 MB, max 500 MB)
- File count cap: 5 text files per message (unchanged)
- Aggregate inline cap: 1 MB for inlined files (filestore uploads bypass this)

### Recommended: S3 Lifecycle Rules

OAB does not delete uploaded objects. **You must configure lifecycle rules
on your bucket** to auto-expire objects, otherwise storage will grow unbounded.

> **Important:** Set your lifecycle expiry **longer** than `presigned_ttl`.
> If objects are deleted before the presigned URL expires, agents will get 404
> errors when fetching. Example: if `presigned_ttl = 3600` (1 hour), set
> lifecycle expiry to at least 24 hours.

#### AWS S3 — via CLI

Create a `lifecycle.json` file:

```json
{
  "Rules": [{
    "ID": "expire-filestore-uploads",
    "Filter": { "Prefix": "incoming/" },
    "Status": "Enabled",
    "Expiration": { "Days": 1 }
  }]
}
```

Apply it:

```bash
aws s3api put-bucket-lifecycle-configuration \
  --bucket my-oab-files \
  --lifecycle-configuration file://lifecycle.json
```

To verify:

```bash
aws s3api get-bucket-lifecycle-configuration --bucket my-oab-files
```

For longer retention (e.g. 30 days), change `"Days": 30`.

📖 Reference: [AWS S3 Lifecycle Configuration](https://docs.aws.amazon.com/AmazonS3/latest/userguide/object-lifecycle-mgmt.html)

#### AWS S3 — via Console

1. Go to **S3 → your bucket → Management → Lifecycle rules**
2. Click **Create lifecycle rule**
3. Rule name: `expire-filestore-uploads`
4. Filter: Prefix `incoming/`
5. Actions: **Expire current versions of objects** → Days: `1` (or `30`)
6. Save

#### Cloudflare R2

R2 supports object lifecycle rules via the dashboard:

1. Go to **R2 → your bucket → Settings → Object lifecycle rules**
2. Click **Add rule**
3. Condition: Prefix matches `incoming/`
4. Action: **Delete objects** after `1 day` (or `30 days`)
5. Save

📖 Reference: [Cloudflare R2 Object Lifecycle Rules](https://developers.cloudflare.com/r2/buckets/object-lifecycles/)

#### MinIO

```bash
# Set lifecycle to expire objects in incoming/ after 1 day
mc ilm rule add myminio/oab-uploads \
  --prefix "incoming/" \
  --expiry-days 1
```

📖 Reference: [MinIO Object Lifecycle Management](https://min.io/docs/minio/linux/administration/object-management/object-lifecycle-management.html)

#### Recommended Expiry Settings

| Use case | Expiry | Rationale |
|----------|--------|-----------|
| Most deployments | 1 day | Agent fetches within minutes; 24h covers retries |
| Long-running sessions | 7 days | For agents that may revisit conversations later |
| Compliance/audit | 30 days | Keep files available for review |

### Minimum IAM Policy

```json
{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Action": [
      "s3:PutObject",
      "s3:GetObject",
      "s3:AbortMultipartUpload",
      "s3:ListMultipartUploadParts"
    ],
    "Resource": "arn:aws:s3:::my-oab-files/incoming/*"
  }]
}
```

## Error Handling

| Failure | Behavior |
|---------|----------|
| S3 upload fails | Agent receives degraded hint: "file could not be uploaded to temporary storage" |
| S3 upload times out (>10 min) | Same as upload failure — degraded hint returned |
| Download from platform fails | File is dropped (warn log), agent not notified |
| Download times out (>10 min) | Same as download failure — file dropped |
| File exceeds max_file_size_mb (Discord/Slack, text or binary) | File is dropped (warn log), agent not notified |
| File exceeds max_file_size_mb (Discord/Slack, audio or Slack video) | Block still emitted with the platform URL; on Slack a `note:` names the size refusal, on Discord the CDN link needs no note |
| File exceeds max_file_size_mb (gateway) | Agent receives degraded hint: "exceeds the configured upload limit and could not be stored" |
| Presigned URL generation fails | Agent receives degraded hint |
| Filestore not configured | Legacy behavior (>512KB files silently dropped) |
| Gateway audio, configured store rejects on size | `[Audio attachment]` block with `note: exceeds the configured upload limit and could not be stored, so there is no fetchable URL` |
| Gateway audio, configured store fails to upload or presign | `[Audio attachment]` block with `note: the configured filestore could not store this attachment, so there is no fetchable URL` |
| Gateway audio, no filestore configured | `[Audio attachment]` block with `note: no fetchable URL for this attachment; configure a filestore ...` |

When filestore is configured but upload fails, the agent always receives a
hint indicating the file exists but content is unavailable. This ensures
the agent can inform the user, even if it cannot retrieve the content. The
audio rows above are stated separately because a configured store that fails
must not be reported as an absent one: that would send the operator to
configure something they already have.

## Build Requirement

The filestore feature is **enabled by default** in standard builds. No extra
flags needed. If you need to disable it:

```bash
cargo build --no-default-features --features "discord,slack,..."
```

When built without it, the `[filestore]` config section is ignored and all
filestore behaviour is unchanged from before. Inbound audio still produces an
`[Audio attachment]` block either way, carrying the platform URL where one exists
and a note explaining the absence where one does not.

## Cost Considerations

| Backend | Storage | PUT | GET (via presigned) | Egress |
|---------|---------|-----|---------------------|--------|
| AWS S3 | $0.023/GB/mo | $0.005/1K | $0.0004/1K | $0.09/GB |
| Cloudflare R2 | $0.015/GB/mo | $0.0045/1K | Free (Class B) | **Free** |
| MinIO (self-hosted) | Disk cost | — | — | — |

For typical usage (a few large files per day, auto-expired after 24h):
- **R2**: essentially free (zero egress + negligible storage)
- **S3**: < $0.01/month for most teams

## Comparison with Alternatives

| Approach | Pros | Cons |
|----------|------|------|
| **Filestore (this)** | Works for all agents, no platform auth leakage, configurable TTL | Requires S3/R2 bucket setup |
| Raw URL hint (PR #1346) | Zero infra needed | Slack broken, Discord expires, agent needs web-fetch |
| Local filesystem | No external deps | Only works in colocate mode, no remote agents |
| OAB HTTP proxy | No bucket needed | Complex, single-instance only, needs port management |

## Future Directions

- **Structured `ContentBlock::File`** in ACP for richer metadata (mime, size, TTL)
- **Metrics** — upload success rate, latency, file size distribution
- **URL hint fallback** — when filestore is not configured, fall back to platform URL hint (PR #1346 pattern)
- **Multi-modal** — extend filestore to images/audio when inline is too large
