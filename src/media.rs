use crate::acp::ContentBlock;
use crate::config::SttConfig;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::StreamExt;
use image::ImageReader;
use std::io::Cursor;
use std::sync::LazyLock;
use tracing::{debug, error};

/// Reusable HTTP client for downloading attachments (shared across adapters).
pub static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("static HTTP client must build")
});

/// Maximum dimension (width or height) for resized images.
const IMAGE_MAX_DIMENSION_PX: u32 = 1200;

/// JPEG quality for compressed output.
const IMAGE_JPEG_QUALITY: u8 = 75;

/// Download an image from a URL, resize/compress it, and return as a ContentBlock.
/// Pass `auth_token` for platforms that require authentication (e.g. Slack private files).
pub async fn download_and_encode_image(
    url: &str,
    mime_hint: Option<&str>,
    filename: &str,
    size: u64,
    auth_token: Option<&str>,
) -> Option<ContentBlock> {
    const MAX_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

    if url.is_empty() {
        return None;
    }

    let mime = mime_hint.or_else(|| {
        filename
            .rsplit('.')
            .next()
            .and_then(|ext| match ext.to_lowercase().as_str() {
                "png" => Some("image/png"),
                "jpg" | "jpeg" => Some("image/jpeg"),
                "gif" => Some("image/gif"),
                "webp" => Some("image/webp"),
                _ => None,
            })
    });

    let Some(mime) = mime else {
        debug!(filename, "skipping non-image attachment");
        return None;
    };
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    if !mime.starts_with("image/") {
        debug!(filename, mime, "skipping non-image attachment");
        return None;
    }

    if size > MAX_SIZE {
        error!(filename, size, "image exceeds 10MB limit");
        return None;
    }

    let mut req = HTTP_CLIENT.get(url);
    if let Some(token) = auth_token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }

    let response = match req.send().await {
        Ok(resp) => resp,
        Err(e) => { error!(url, error = %e, "download failed"); return None; }
    };
    if !response.status().is_success() {
        error!(url, status = %response.status(), "HTTP error downloading image");
        return None;
    }
    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => { error!(url, error = %e, "read failed"); return None; }
    };

    if bytes.len() as u64 > MAX_SIZE {
        error!(filename, size = bytes.len(), "downloaded image exceeds limit");
        return None;
    }

    let (output_bytes, output_mime) = match resize_and_compress(&bytes) {
        Ok(result) => result,
        Err(e) => {
            if bytes.len() > 1024 * 1024 {
                error!(filename, error = %e, size = bytes.len(), "resize failed and original too large, skipping");
                return None;
            }
            debug!(filename, error = %e, "resize failed, using original");
            (bytes.to_vec(), mime.to_string())
        }
    };

    debug!(
        filename,
        original_size = bytes.len(),
        compressed_size = output_bytes.len(),
        "image processed"
    );

    let encoded = BASE64.encode(&output_bytes);
    Some(ContentBlock::Image {
        media_type: output_mime,
        data: encoded,
    })
}

/// Download an audio file and transcribe it via the configured STT provider.
/// Pass `auth_token` for platforms that require authentication.
pub async fn download_and_transcribe(
    url: &str,
    filename: &str,
    mime_type: &str,
    size: u64,
    stt_config: &SttConfig,
    auth_token: Option<&str>,
) -> Option<String> {
    const MAX_SIZE: u64 = 25 * 1024 * 1024; // 25 MB (Whisper API limit)

    if size > MAX_SIZE {
        error!(filename, size, "audio exceeds 25MB limit");
        return None;
    }

    let mut req = HTTP_CLIENT.get(url);
    if let Some(token) = auth_token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }

    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        error!(url, status = %resp.status(), "audio download failed");
        return None;
    }
    let bytes = resp.bytes().await.ok()?.to_vec();

    crate::stt::transcribe(&HTTP_CLIENT, stt_config, bytes, filename.to_string(), mime_type).await
}

/// Resize image so longest side <= IMAGE_MAX_DIMENSION_PX, then encode as JPEG.
/// GIFs are passed through unchanged to preserve animation.
pub fn resize_and_compress(raw: &[u8]) -> Result<(Vec<u8>, String), image::ImageError> {
    let reader = ImageReader::new(Cursor::new(raw))
        .with_guessed_format()?;

    let format = reader.format();

    if format == Some(image::ImageFormat::Gif) {
        return Ok((raw.to_vec(), "image/gif".to_string()));
    }

    let img = reader.decode()?;
    let (w, h) = (img.width(), img.height());

    let img = if w > IMAGE_MAX_DIMENSION_PX || h > IMAGE_MAX_DIMENSION_PX {
        let max_side = std::cmp::max(w, h);
        let ratio = f64::from(IMAGE_MAX_DIMENSION_PX) / f64::from(max_side);
        let new_w = (f64::from(w) * ratio) as u32;
        let new_h = (f64::from(h) * ratio) as u32;
        img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, IMAGE_JPEG_QUALITY);
    img.write_with_encoder(encoder)?;

    Ok((buf.into_inner(), "image/jpeg".to_string()))
}

/// Check if a MIME type is audio.
pub fn is_audio_mime(mime: &str) -> bool {
    mime.starts_with("audio/")
}

/// Check if a MIME type is `text/plain` (Discord's auto-convert for long messages).
pub fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/plain")
}

/// Download a `text/plain` attachment and return its UTF-8 contents.
///
/// Discord auto-converts messages longer than 2000 characters into a `.txt` file
/// attachment; inlining the bytes lets the agent see the full content instead of
/// receiving an empty prompt. Capped at 128 KB to avoid blowing the context window.
pub async fn download_text_attachment(
    url: &str,
    filename: &str,
    size: u64,
    auth_token: Option<&str>,
) -> Option<String> {
    // Discord auto-converts any message body over 2000 characters into a
    // .txt attachment. 128 KB covers ~20× that threshold — plenty for pasted
    // logs and stack traces, while keeping a bounded share of the agent's
    // context window.
    const MAX_SIZE: u64 = 128 * 1024;

    // Discord metadata hint. Only a hint — cheap fail-fast to skip the HTTP
    // round-trip when the attachment is obviously oversized. The
    // authoritative cap is enforced on the response body below; metadata
    // and Content-Length are not trusted on their own.
    if size > MAX_SIZE {
        tracing::warn!(filename, size, "text attachment exceeds 128KB (metadata), skipping");
        return None;
    }

    let mut req = HTTP_CLIENT.get(url);
    if let Some(token) = auth_token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }

    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        error!(url, status = %resp.status(), "text download failed");
        return None;
    }

    // Single source of truth for the cap: stream chunks and abort the moment
    // the running total would exceed MAX_SIZE. This is the authoritative
    // guard — Content-Length and metadata are untrusted. Pre-size the
    // buffer against the metadata hint so the common path (valid small
    // attachment) avoids repeated reallocations.
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(size.min(MAX_SIZE) as usize);
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(filename, error = %e, "text attachment stream error");
                return None;
            }
        };
        if buf.len().saturating_add(chunk.len()) > MAX_SIZE as usize {
            tracing::warn!(
                filename,
                streamed = buf.len(),
                chunk_size = chunk.len(),
                "text attachment body exceeds 128KB, aborting"
            );
            return None;
        }
        buf.extend_from_slice(&chunk);
    }

    // `String::from_utf8` consumes the Vec — no extra copy.
    match String::from_utf8(buf) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!(filename, error = %e, "text attachment is not valid UTF-8, skipping");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::new(width, height);
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn large_image_resized_to_max_dimension() {
        let png = make_png(3000, 2000);
        let (compressed, mime) = resize_and_compress(&png).unwrap();

        assert_eq!(mime, "image/jpeg");
        let result = image::load_from_memory(&compressed).unwrap();
        assert!(result.width() <= IMAGE_MAX_DIMENSION_PX);
        assert!(result.height() <= IMAGE_MAX_DIMENSION_PX);
    }

    #[test]
    fn small_image_keeps_original_dimensions() {
        let png = make_png(800, 600);
        let (compressed, mime) = resize_and_compress(&png).unwrap();

        assert_eq!(mime, "image/jpeg");
        let result = image::load_from_memory(&compressed).unwrap();
        assert_eq!(result.width(), 800);
        assert_eq!(result.height(), 600);
    }

    #[test]
    fn landscape_image_respects_aspect_ratio() {
        let png = make_png(4000, 2000);
        let (compressed, _) = resize_and_compress(&png).unwrap();

        let result = image::load_from_memory(&compressed).unwrap();
        assert_eq!(result.width(), 1200);
        assert_eq!(result.height(), 600);
    }

    #[test]
    fn portrait_image_respects_aspect_ratio() {
        let png = make_png(2000, 4000);
        let (compressed, _) = resize_and_compress(&png).unwrap();

        let result = image::load_from_memory(&compressed).unwrap();
        assert_eq!(result.width(), 600);
        assert_eq!(result.height(), 1200);
    }

    #[test]
    fn compressed_output_is_smaller_than_original() {
        let png = make_png(3000, 2000);
        let (compressed, _) = resize_and_compress(&png).unwrap();

        assert!(compressed.len() < png.len(), "compressed {} should be < original {}", compressed.len(), png.len());
    }

    #[test]
    fn gif_passes_through_unchanged() {
        let gif: Vec<u8> = vec![
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61,
            0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
            0x02, 0x02, 0x44, 0x01, 0x00,
            0x3B,
        ];
        let (output, mime) = resize_and_compress(&gif).unwrap();

        assert_eq!(mime, "image/gif");
        assert_eq!(output, gif);
    }

    #[test]
    fn invalid_data_returns_error() {
        let garbage = vec![0x00, 0x01, 0x02, 0x03];
        assert!(resize_and_compress(&garbage).is_err());
    }

    // --- download_text_attachment tests ---
    //
    // The implementation has exactly two gates: a metadata hint short-
    // circuit (no network traffic), and the streaming cap on the response
    // body (authoritative). Each gate gets one test, plus a happy-path
    // sanity check. No regression fence is needed because there is no
    // second defense layer that could silently absorb a failure of the
    // streaming cap — if the streaming check is removed, oversized bodies
    // pass through and the oversized-body test fails directly.

    use wiremock::matchers::{method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn download_text_returns_content_when_body_small() {
        let server = MockServer::start().await;
        let body = "hello\nworld";
        Mock::given(method("GET"))
            .and(wm_path("/a.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let url = format!("{}/a.txt", server.uri());
        let result =
            download_text_attachment(&url, "a.txt", body.len() as u64, None).await;
        assert_eq!(result, Some(body.to_string()));
    }

    #[tokio::test]
    async fn download_text_rejects_when_metadata_over_cap() {
        // Metadata guard must short-circuit before any network call. We
        // stand up a MockServer with `.expect(0)` so the test fails if any
        // HTTP request reaches the server — a regression that dropped the
        // metadata guard and delegated rejection to the streaming cap
        // would be caught here as unnecessary outbound traffic, even
        // though it would still return `None` at the end.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .expect(0)
            .mount(&server)
            .await;

        let url = format!("{}/does-not-matter", server.uri());
        let result = download_text_attachment(
            &url,
            "big.txt",
            200 * 1024, // > 128 KB cap
            None,
        )
        .await;
        assert_eq!(result, None);
        // MockServer verifies `.expect(0)` on drop; leaving this explicit
        // so the intent survives a copy-paste by future maintainers.
        drop(server);
    }

    #[tokio::test]
    async fn download_text_rejects_body_over_cap() {
        // Metadata declares 1 KB (bypasses the metadata guard), body is
        // 200 KB. Only the streaming cap can catch this — there is no
        // other layer in the implementation that could reject it. A
        // regression that removes the streaming cap would let the full
        // body buffer and return `Some(...)`, failing this test directly.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/big.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'A'; 200 * 1024]))
            .mount(&server)
            .await;

        let url = format!("{}/big.txt", server.uri());
        let result =
            download_text_attachment(&url, "big.txt", 1024, None).await;
        assert_eq!(result, None);
    }
}
