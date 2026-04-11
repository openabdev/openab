use crate::acp::ContentBlock;
use base64::Engine;
use tracing::warn;

const MAX_IMAGE_SIDE: u32 = 1200;

pub enum MediaInput {
    Text(String),
    Image { bytes: Vec<u8>, mime: String },
    // Audio and Document variants added in future phases
}

pub fn resolve(input: MediaInput, model_id: Option<&str>) -> Vec<ContentBlock> {
    match input {
        MediaInput::Text(t) => vec![ContentBlock::Text { text: t }],
        MediaInput::Image { bytes, mime } => {
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
    }
}

/// Resize image so longest side <= MAX_IMAGE_SIDE, re-encode as JPEG.
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

    #[test]
    fn text_input_gives_text_block() {
        let blocks = resolve(MediaInput::Text("hello".into()), None);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "hello"));
    }

    #[test]
    fn non_vision_model_drops_image() {
        let blocks = resolve(
            MediaInput::Image { bytes: vec![1, 2, 3], mime: "image/jpeg".into() },
            Some("gemini-pro"),
        );
        assert!(blocks.is_empty());
    }

    #[test]
    fn gif_passes_through_unchanged() {
        let gif = vec![0x47, 0x49, 0x46, 0x38]; // GIF magic bytes
        let (out, mime) = resize_image(gif.clone(), "image/gif");
        assert_eq!(out, gif);
        assert_eq!(mime, "image/gif");
    }
}
