use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{error, info};
use uuid::Uuid;

/// Inbound media directory under $HOME.
/// Pattern follows OpenClaw's `~/.openclaw/media/inbound/<uuid>`.
///
/// # Security Considerations
///
/// - **Path traversal prevention**: Filenames are always server-generated UUIDs,
///   never user-supplied. No extension, no special characters — eliminates path
///   traversal attacks (e.g. `../../etc/passwd`).
///
/// - **No auth token leakage**: Platform media URLs (Telegram getFile, LINE Content API)
///   contain bot tokens or require auth headers. By downloading in the gateway and
///   storing locally, tokens never reach Core or the agent.
///
/// - **TTL auto-eviction**: Files are evicted after 2 minutes. Prevents disk exhaustion
///   from accumulated media and limits the window for any leaked file to be exploited.
///
/// - **Colocate trust boundary**: This module assumes gateway and core share the same
///   filesystem (same pod / same $HOME). The file path is passed over the internal WS
///   connection — never exposed externally. If gateway and core are separated in the
///   future, switch to HTTP media proxy with internal-only binding.
///
/// - **Size limits enforced before write**: Callers must validate file size against
///   IMAGE_MAX_DOWNLOAD / AUDIO_MAX_DOWNLOAD / FILE_MAX_DOWNLOAD before calling
///   `store_media()`. This module does NOT re-validate — it trusts the caller.
///
/// - **No executable content**: Stored files are raw bytes (images, audio, text).
///   Core reads them as data only — never executed. The `mime_type` in the event
///   payload determines processing path, not the file content or name.
const MEDIA_INBOUND_DIR: &str = ".openab/media/inbound";

/// TTL for stored media files (2 minutes)
const TTL_SECS: u64 = 120;

/// Get the inbound media directory path, creating it if needed.
pub async fn media_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = Path::new(&home).join(MEDIA_INBOUND_DIR);
    if !dir.exists() {
        let _ = fs::create_dir_all(&dir).await;
    }
    dir
}

/// Maximum file size accepted by store (defense-in-depth, callers should pre-check).
const MAX_STORE_SIZE: usize = 20 * 1024 * 1024; // 20 MB (matches AUDIO_MAX_DOWNLOAD)

/// Store media bytes to disk, return the absolute file path.
/// Filename is UUID only (no extension) — MIME type is carried in the event payload.
/// Rejects files exceeding MAX_STORE_SIZE as a defense-in-depth measure.
pub async fn store_media(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_STORE_SIZE {
        error!(
            size = bytes.len(),
            max = MAX_STORE_SIZE,
            "store_media rejected: exceeds size limit"
        );
        return None;
    }
    let dir = media_dir().await;
    let filename = Uuid::new_v4().to_string();
    let path = dir.join(&filename);
    match fs::write(&path, bytes).await {
        Ok(_) => {
            info!(path = %path.display(), size = bytes.len(), "media stored");
            Some(path.to_string_lossy().into_owned())
        }
        Err(e) => {
            error!(error = %e, "failed to store media file");
            None
        }
    }
}

/// Background task: evict files older than TTL_SECS.
pub async fn eviction_loop() {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        interval.tick().await;
        if let Err(e) = evict_expired().await {
            error!(error = %e, "media eviction error");
        }
    }
}

async fn evict_expired() -> std::io::Result<()> {
    let dir = media_dir().await;
    if !dir.exists() {
        return Ok(());
    }
    let mut entries = fs::read_dir(&dir).await?;
    let now = std::time::SystemTime::now();
    while let Some(entry) = entries.next_entry().await? {
        if let Ok(meta) = entry.metadata().await {
            if let Ok(modified) = meta.modified() {
                if let Ok(age) = now.duration_since(modified) {
                    if age.as_secs() > TTL_SECS {
                        let path = entry.path();
                        let _ = fs::remove_file(&path).await;
                        tracing::debug!(path = %path.display(), "evicted expired media");
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn store_and_read_back() {
        let data = b"hello media";
        let path = store_media(data).await.unwrap();
        let read_back = fs::read(&path).await.unwrap();
        assert_eq!(read_back, data);
        // Cleanup
        let _ = fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn filename_is_uuid_no_extension() {
        let path = store_media(b"test").await.unwrap();
        let filename = Path::new(&path).file_name().unwrap().to_str().unwrap();
        // UUID v4 format: 8-4-4-4-12 hex chars
        assert_eq!(filename.len(), 36);
        assert!(!filename.contains('.'));
        let _ = fs::remove_file(&path).await;
    }
}
