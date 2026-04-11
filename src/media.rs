use crate::acp::ContentBlock;
use base64::Engine;
use tracing::warn;

const MAX_IMAGE_SIDE: u32 = 1200;
const MAX_DOCUMENT_BYTES: usize = 20 * 1024 * 1024; // 20MB (Telegram bot limit)
const MARKITDOWN_TIMEOUT_SECS: u64 = 60;

pub enum MediaInput {
    Text(String),
    Image { bytes: Vec<u8>, mime: String },
    Document { bytes: Vec<u8>, filename: String },
}

pub async fn resolve(input: MediaInput, model_id: Option<&str>) -> Vec<ContentBlock> {
    match input {
        MediaInput::Text(t) => vec![ContentBlock::Text { text: t }],
        MediaInput::Image { bytes, mime } => resolve_image(bytes, mime, model_id),
        MediaInput::Document { bytes, filename } => resolve_document(bytes, filename).await,
    }
}

fn resolve_image(bytes: Vec<u8>, mime: String, model_id: Option<&str>) -> Vec<ContentBlock> {
    let is_vision_capable = model_id
        .map(|id| id.to_lowercase().contains("claude") || id == "auto")
        .unwrap_or(true);

    if !is_vision_capable {
        warn!(model_id, "model does not support images, dropping image block");
        return vec![];
    }

    let (final_bytes, final_mime) = resize_image(bytes, &mime);
    let data = base64::engine::general_purpose::STANDARD.encode(&final_bytes);
    vec![ContentBlock::Image { media_type: final_mime, data }]
}

async fn resolve_document(bytes: Vec<u8>, filename: String) -> Vec<ContentBlock> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return vec![ContentBlock::Text {
            text: format!("[File: {filename} — too large to process (max 20MB)]"),
        }];
    }

    match convert_with_markitdown(&bytes, &filename).await {
        Ok(markdown) if !markdown.trim().is_empty() => {
            vec![ContentBlock::Text {
                text: format!("[File: {filename}]\n\n{markdown}"),
            }]
        }
        Ok(_) => vec![ContentBlock::Text {
            text: format!("[File: {filename} — no content extracted]"),
        }],
        Err(e) => {
            warn!(filename, error = %e, "markitdown conversion failed");
            vec![ContentBlock::Text {
                text: format!("[File: {filename} — could not extract content: {e}]"),
            }]
        }
    }
}

async fn convert_with_markitdown(bytes: &[u8], filename: &str) -> anyhow::Result<String> {
    use tokio::io::AsyncWriteExt;

    // Write to temp file with original filename so markitdown detects format correctly
    let tmp_dir = tempfile::tempdir()?;
    let tmp_path = tmp_dir.path().join(filename);
    tokio::fs::write(&tmp_path, bytes).await?;

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(MARKITDOWN_TIMEOUT_SECS),
        tokio::process::Command::new("markitdown")
            .arg(&tmp_path)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("markitdown timed out after {MARKITDOWN_TIMEOUT_SECS}s"))??;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("markitdown exited with error: {stderr}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
/// Falls back to original bytes on any error (matches upstream #210 pattern).
fn resize_image(bytes: Vec<u8>, mime: &str) -> (Vec<u8>, String) {
    // GIFs pass through unchanged to preserve animation
    if mime.contains("gif") {
        return (bytes, mime.to_string());
    }

    let img = match image::load_from_memory(&bytes) {
        Ok(i) => i,
        Err(e) => {
            warn!("image decode failed ({e}), using original");
            return (bytes, mime.to_string());
        }
    };

    let (w, h) = (img.width(), img.height());
    let resized = if w > MAX_IMAGE_SIDE || h > MAX_IMAGE_SIDE {
        let (nw, nh) = if w >= h {
            (MAX_IMAGE_SIDE, (h * MAX_IMAGE_SIDE / w).max(1))
        } else {
            ((w * MAX_IMAGE_SIDE / h).max(1), MAX_IMAGE_SIDE)
        };
        img.resize(nw, nh, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    let mut out = Vec::new();
    if resized
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .is_err()
    {
        warn!("image re-encode failed, using original");
        return (bytes, mime.to_string());
    }

    (out, "image/jpeg".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn text_input_gives_text_block() {
        let blocks = resolve(MediaInput::Text("hello".into()), None).await;
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "hello"));
    }

    #[tokio::test]
    async fn non_vision_model_drops_image() {
        let blocks = resolve(
            MediaInput::Image { bytes: vec![1, 2, 3], mime: "image/jpeg".into() },
            Some("gemini-pro"),
        ).await;
        assert!(blocks.is_empty());
    }

    #[test]
    fn gif_passes_through_unchanged() {
        let gif = vec![0x47, 0x49, 0x46, 0x38]; // GIF magic bytes
        let (out, mime) = resize_image(gif.clone(), "image/gif");
        assert_eq!(out, gif);
        assert_eq!(mime, "image/gif");
    }

    #[tokio::test]
    async fn document_too_large_returns_placeholder() {
        let big = vec![0u8; MAX_DOCUMENT_BYTES + 1];
        let blocks = resolve(MediaInput::Document { bytes: big, filename: "big.pdf".into() }, None).await;
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text.contains("too large")));
    }
}
