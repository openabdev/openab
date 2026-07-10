use crate::acp::ContentBlock;
use crate::config::SttConfig;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, ImageReader};
use std::io::Cursor;
use std::sync::LazyLock;
use tracing::{debug, error, warn};

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

/// Error variants for `download_and_encode_image`.
#[derive(Debug)]
pub enum MediaFetchError {
    /// URL empty or MIME/filename doesn't indicate an image; skip silently.
    NotAnImage,
    /// HTTP response Content-Type is not a supported image format.
    UnsupportedResponseType { actual: Option<String> },
    /// Response body magic bytes don't match a supported image format.
    InvalidImageBody { magic_prefix_hex: String },
    /// File exceeds the configured size limit.
    SizeExceeded { actual: u64, limit: u64 },
    /// Network-level error (send or body-read).
    Network(reqwest::Error),
    /// Server returned a non-success HTTP status.
    HttpStatus(reqwest::StatusCode),
    /// Body was a valid image but post-processing (resize/compress) failed.
    /// Unlike `InvalidImageBody`, the bytes decoded successfully — this is an
    /// unexpected processing error, not a content validation failure. Both the
    /// Slack and Discord adapters surface this as a user-facing warning alongside
    /// other image-validation failures.
    ProcessingFailed(image::ImageError),
}

impl std::fmt::Display for MediaFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnImage => write!(f, "not an image attachment"),
            Self::UnsupportedResponseType { actual } => write!(
                f,
                "server returned unexpected content type (actual: {})",
                actual.as_deref().unwrap_or("none"),
            ),
            Self::InvalidImageBody { magic_prefix_hex } => write!(
                f,
                "response body is not a valid image (first 8 bytes: {magic_prefix_hex})"
            ),
            Self::SizeExceeded { actual, limit } => {
                write!(f, "file size {actual} exceeds limit {limit}")
            }
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::HttpStatus(s) => write!(f, "HTTP {s}"),
            Self::ProcessingFailed(e) => write!(f, "image processing failed: {e}"),
        }
    }
}

/// Strip MIME parameters and trim whitespace.  `"image/png; charset=binary"` → `"image/png"`.
pub(crate) fn strip_mime_params(mime: &str) -> &str {
    mime.split(';').next().unwrap_or(mime).trim()
}

/// Format the first 8 bytes of a buffer as lowercase hex (no separator).
fn hex_prefix(body: &[u8]) -> String {
    body.iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .concat()
}

/// Validate the HTTP response Content-Type and body magic bytes.
///
/// If Content-Type is present and explicitly text-typed (e.g. `text/html` from
/// Slack's auth redirect when `files:read` scope is missing), rejects immediately.
/// Generic types such as `application/octet-stream` and absent headers pass through
/// to the magic-byte check, which is the authoritative gate for image validity.
///
/// Content-Type is filtered with a block-list (`text/*`) rather than an allow-list
/// (`image/*`) because CDNs commonly serve any file type as `application/octet-stream`;
/// rejecting that header would silently break real downloads. The magic-byte check
/// examines the actual bytes regardless of what the server claims.
fn validate_image_response(
    content_type: Option<&str>,
    body: &[u8],
) -> Result<(), MediaFetchError> {
    // Reject explicitly-text responses early (e.g. Slack HTML login page at HTTP 200).
    // application/octet-stream and other generic types pass through to magic-byte check.
    if let Some(ct) = content_type {
        let base = strip_mime_params(ct).to_lowercase();
        if base.starts_with("text/") {
            return Err(MediaFetchError::UnsupportedResponseType { actual: Some(base) });
        }
    }

    let reader = match ImageReader::new(Cursor::new(body)).with_guessed_format() {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "image format detection I/O error");
            return Err(MediaFetchError::InvalidImageBody {
                magic_prefix_hex: hex_prefix(body),
            });
        }
    };

    match reader.format() {
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP) => {
            Ok(())
        }
        Some(image::ImageFormat::Gif) => {
            validate_gif_body(body).map_err(|e| {
                warn!(error = %e, "GIF validation failed");
                MediaFetchError::InvalidImageBody {
                    magic_prefix_hex: hex_prefix(body),
                }
            })?;
            Ok(())
        }
        _ => Err(MediaFetchError::InvalidImageBody {
            magic_prefix_hex: hex_prefix(body),
        }),
    }
}

/// Validate a GIF body by attempting to decode exactly one frame.
///
/// Decoding only the first frame is intentional: the GIF header and colour tables
/// must be valid before the first frame can be decoded, so this catches truncated
/// or corrupt payloads without the CPU/memory cost of decoding a large animated GIF
/// in full.
///
/// Creates its own `Cursor` over `raw`; the caller can independently re-read the
/// same slice for resizing.
fn validate_gif_body(raw: &[u8]) -> image::ImageResult<()> {
    let decoder = GifDecoder::new(Cursor::new(raw))?;
    let mut frames = decoder.into_frames();
    frames.next().ok_or_else(|| {
        image::ImageError::Decoding(image::error::DecodingError::new(
            image::error::ImageFormatHint::Exact(image::ImageFormat::Gif),
            "GIF has no frames",
        ))
    })??;
    Ok(())
}

/// Download an image from a URL, resize/compress it, and return as a ContentBlock.
///
/// Returns `Err(MediaFetchError::NotAnImage)` when the URL or MIME hint don't
/// indicate an image — callers should skip silently.  Returns
/// `Err(MediaFetchError::SizeExceeded)` when the declared `size` exceeds the limit
/// before any request is made, or when the downloaded body exceeds the limit.  Returns
/// other `Err` variants (`Network`, `HttpStatus`, `UnsupportedResponseType`,
/// `InvalidImageBody`) after a request attempt — callers should surface these to the user.  Returns
/// `Err(MediaFetchError::ProcessingFailed)` when the body is a valid image but
/// resize/compression fails — callers should warn the user and skip.
///
/// Pass `auth_token` for platforms that require authentication (e.g. Slack private files).
pub async fn download_and_encode_image(
    url: &str,
    mime_hint: Option<&str>,
    filename: &str,
    size: u64,
    auth_token: Option<&str>,
) -> Result<ContentBlock, MediaFetchError> {
    const MAX_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

    if url.is_empty() {
        return Err(MediaFetchError::NotAnImage);
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
        return Err(MediaFetchError::NotAnImage);
    };
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    if !mime.starts_with("image/") {
        debug!(filename, mime, "skipping non-image attachment");
        return Err(MediaFetchError::NotAnImage);
    }

    if size > MAX_SIZE {
        error!(filename, size, "image exceeds 10MB limit");
        return Err(MediaFetchError::SizeExceeded {
            actual: size,
            limit: MAX_SIZE,
        });
    }

    let mut req = HTTP_CLIENT.get(url);
    if let Some(token) = auth_token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }

    let response = match req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            error!(url, error = %e, "download failed");
            return Err(MediaFetchError::Network(e));
        }
    };
    if !response.status().is_success() {
        error!(url, status = %response.status(), "HTTP error downloading image");
        return Err(MediaFetchError::HttpStatus(response.status()));
    }

    // Capture Content-Type BEFORE .bytes() consumes the response.
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            error!(url, error = %e, "read failed");
            return Err(MediaFetchError::Network(e));
        }
    };

    if bytes.len() as u64 > MAX_SIZE {
        error!(
            filename,
            size = bytes.len(),
            "downloaded image exceeds limit"
        );
        return Err(MediaFetchError::SizeExceeded {
            actual: bytes.len() as u64,
            limit: MAX_SIZE,
        });
    }

    // Guard against HTTP 200 responses that are error pages (e.g. Slack auth redirect
    // when files:read scope is missing), and against corrupted or mislabeled bodies.
    if let Err(e) = validate_image_response(content_type.as_deref(), &bytes) {
        error!(
            filename,
            mime_hint = mime,
            content_type = content_type.as_deref().unwrap_or("none"),
            magic = hex_prefix(&bytes),
            error = %e,
            "image validation failed — body is not a supported image"
        );
        return Err(e);
    }

    let (output_bytes, output_mime) = match resize_and_compress(&bytes) {
        Ok(result) => result,
        Err(e) => {
            error!(
                filename,
                error = %e,
                size = bytes.len(),
                "resize failed after successful validation"
            );
            return Err(MediaFetchError::ProcessingFailed(e));
        }
    };

    debug!(
        filename,
        original_size = bytes.len(),
        compressed_size = output_bytes.len(),
        "image processed"
    );

    let encoded = BASE64.encode(&output_bytes);
    Ok(ContentBlock::Image {
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

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            error!(url, error = %e, "audio download request failed");
            return None;
        }
    };
    if !resp.status().is_success() {
        error!(url, status = %resp.status(), "audio download failed");
        return None;
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            error!(url, error = %e, "audio body read failed");
            return None;
        }
    };

    if bytes.len() as u64 > MAX_SIZE {
        error!(filename, size = bytes.len(), "downloaded audio exceeds 25MB limit");
        return None;
    }

    crate::stt::transcribe(
        &HTTP_CLIENT,
        stt_config,
        bytes,
        filename.to_string(),
        mime_type,
    )
    .await
}

/// Resize image so longest side <= IMAGE_MAX_DIMENSION_PX, then encode as JPEG.
/// GIFs are passed through unchanged to preserve animation.
pub fn resize_and_compress(raw: &[u8]) -> Result<(Vec<u8>, String), image::ImageError> {
    let reader = ImageReader::new(Cursor::new(raw)).with_guessed_format()?;

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

/// Check if an attachment is a video file.
pub fn is_video_file(filename: &str, content_type: Option<&str>) -> bool {
    let mime = content_type.unwrap_or("");
    let mime_base = mime.split(';').next().unwrap_or(mime).trim();
    if mime_base.starts_with("video/") {
        return true;
    }

    filename
        .rsplit('.')
        .next()
        .map(|ext| {
            matches!(
                ext.to_lowercase().as_str(),
                "mp4" | "mov" | "m4v" | "webm" | "mkv" | "avi"
            )
        })
        .unwrap_or(false)
}

/// Extensions recognised as text-based files that can be inlined into the prompt.
const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "csv", "log", "md", "json", "jsonl", "yaml", "yml", "toml", "xml", "rs", "py", "js",
    "ts", "jsx", "tsx", "go", "java", "c", "cpp", "h", "hpp", "rb", "sh", "bash", "zsh", "fish",
    "ps1", "bat", "sql", "html", "css", "scss", "less", "ini", "cfg", "conf", "env",
];

/// Exact filenames (no extension) recognised as text files.
const TEXT_FILENAMES: &[&str] = &[
    "dockerfile",
    "makefile",
    "justfile",
    "rakefile",
    "gemfile",
    "procfile",
    "vagrantfile",
    ".gitignore",
    ".dockerignore",
    ".editorconfig",
];

/// MIME types recognised as text-based (beyond `text/*`).
const TEXT_MIME_TYPES: &[&str] = &[
    "application/json",
    "application/xml",
    "application/javascript",
    "application/x-yaml",
    "application/x-sh",
    "application/toml",
    "application/x-toml",
];

/// Check if a file is text-based and can be inlined into the prompt.
pub fn is_text_file(filename: &str, content_type: Option<&str>) -> bool {
    let mime = content_type.unwrap_or("");
    let mime_base = mime.split(';').next().unwrap_or(mime).trim();
    if mime_base.starts_with("text/") || TEXT_MIME_TYPES.contains(&mime_base) {
        return true;
    }
    // Check extension
    if filename.contains('.') {
        if let Some(ext) = filename.rsplit('.').next() {
            if TEXT_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
                return true;
            }
        }
    }
    // Check exact filename (Dockerfile, Makefile, etc.)
    TEXT_FILENAMES.contains(&filename.to_lowercase().as_str())
}

/// Download a text-based file and return it as a ContentBlock::Text.
/// Files larger than `max_bytes` are skipped to avoid bloating the prompt.
/// The limit is configurable via `[media] max_text_file_kb` in config.toml
/// (default: 512 KB).
///
/// Pass `auth_token` for platforms that require authentication (e.g. Slack private files).
///
/// Note: the caller already guards total size via a total cap; the per-file
/// max_bytes check here is intentional defense-in-depth so this function remains
/// self-contained and safe when called from other contexts.
pub async fn download_and_read_text_file(
    url: &str,
    filename: &str,
    size: u64,
    auth_token: Option<&str>,
    max_bytes: u64,
) -> Option<(ContentBlock, u64)> {
    if size > max_bytes {
        tracing::warn!(filename, size, max_bytes, "text file exceeds size limit, skipping");
        return None;
    }

    let mut req = HTTP_CLIENT.get(url);
    if let Some(token) = auth_token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url, error = %e, "text file download failed");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(url, status = %resp.status(), "text file download failed");
        return None;
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(url, error = %e, "text file body read failed");
            return None;
        }
    };
    let actual_size = bytes.len() as u64;

    // Defense-in-depth: verify actual download size
    if actual_size > max_bytes {
        tracing::warn!(
            filename,
            size = actual_size,
            max_bytes,
            "downloaded text file exceeds size limit, skipping"
        );
        return None;
    }

    // from_utf8_lossy returns Cow::Borrowed for valid UTF-8 (zero-copy)
    let text = String::from_utf8_lossy(&bytes).into_owned();

    // Dynamic fence: keep adding backticks until the fence doesn't appear in content
    let mut fence = "```".to_string();
    while text.contains(fence.as_str()) {
        fence.push('`');
    }

    debug!(filename, bytes = text.len(), "text file inlined");
    Some((
        ContentBlock::Text {
            text: format!("[File: {filename}]\n{fence}\n{text}\n{fence}"),
        },
        actual_size,
    ))
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

    fn make_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::new(width, height);
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
        buf.into_inner()
    }

    fn make_gif() -> Vec<u8> {
        vec![
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00,
            0x00, 0x00, 0xff, 0xff, 0xff, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00,
            0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3B,
        ]
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

        assert!(
            compressed.len() < png.len(),
            "compressed {} should be < original {}",
            compressed.len(),
            png.len()
        );
    }

    #[test]
    fn gif_passes_through_unchanged() {
        let gif = make_gif();
        let (output, mime) = resize_and_compress(&gif).unwrap();

        assert_eq!(mime, "image/gif");
        assert_eq!(output, gif);
    }

    #[test]
    fn invalid_data_returns_error() {
        let garbage = vec![0x00, 0x01, 0x02, 0x03];
        assert!(resize_and_compress(&garbage).is_err());
    }

    #[test]
    fn video_file_detects_mime_and_common_extensions() {
        assert!(is_video_file("clip.bin", Some("video/mp4")));
        assert!(is_video_file("clip.mp4", None));
        assert!(is_video_file("clip.MOV", None));
        assert!(!is_video_file("notes.txt", Some("text/plain")));
    }

    // --- validate_image_response tests ---

    #[test]
    fn validate_accepts_png_with_matching_content_type() {
        let png = make_png(1, 1);
        assert!(validate_image_response(Some("image/png"), &png).is_ok());
    }

    #[test]
    fn validate_accepts_jpeg_with_matching_content_type() {
        let jpeg = make_jpeg(1, 1);
        assert!(validate_image_response(Some("image/jpeg"), &jpeg).is_ok());
    }

    #[test]
    fn validate_accepts_gif_with_matching_content_type() {
        let gif = make_gif();
        assert!(validate_image_response(Some("image/gif"), &gif).is_ok());
    }

    #[test]
    fn validate_rejects_corrupt_gif_body() {
        let corrupt_gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00";
        let result = validate_image_response(Some("image/gif"), corrupt_gif);
        assert!(matches!(
            result,
            Err(MediaFetchError::InvalidImageBody { .. })
        ));
    }

    #[test]
    fn validate_accepts_missing_content_type_with_valid_png() {
        // When Content-Type header is absent, fall back to magic-byte detection.
        let png = make_png(1, 1);
        assert!(validate_image_response(None, &png).is_ok());
    }

    #[test]
    fn validate_content_type_strips_params() {
        // "image/png; charset=binary" is a real header value — must be accepted.
        let png = make_png(1, 1);
        assert!(validate_image_response(Some("image/png; charset=binary"), &png).is_ok());
    }

    /// Exact reproduction of issue #776: Slack serves the workspace login HTML
    /// page at HTTP 200 when the bot token lacks the `files:read` scope.
    /// The Slack file metadata says `mimetype: image/png`; the response body
    /// magic bytes are `<!DOCTYP` (0x3c 0x21 0x44 0x4f 0x43 0x54 0x59 0x50).
    #[test]
    fn validate_rejects_html_body_labeled_as_image_png() {
        let html_body = b"<!DOCTYPE html><html><head></head><body>Slack login</body></html>";
        let result = validate_image_response(Some("image/png"), html_body);
        match result {
            Err(MediaFetchError::InvalidImageBody { magic_prefix_hex }) => {
                assert_eq!(magic_prefix_hex, "3c21444f43545950");
            }
            other => panic!("expected InvalidImageBody, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_text_html_content_type() {
        // Even if the body were a valid image, a text/html Content-Type must be rejected.
        let png = make_png(1, 1);
        let result = validate_image_response(Some("text/html; charset=utf-8"), &png);
        assert!(matches!(
            result,
            Err(MediaFetchError::UnsupportedResponseType { .. })
        ));
    }

    #[test]
    fn validate_rejects_mixed_case_text_content_type() {
        // Mixed-case Content-Type must be normalised before rejection.
        let png = make_png(1, 1);
        let result = validate_image_response(Some("Text/HTML; Charset=utf-8"), &png);
        assert!(matches!(
            result,
            Err(MediaFetchError::UnsupportedResponseType { .. })
        ));
    }

    /// Regression test for the application/octet-stream fix: CDNs and generic
    /// file download endpoints commonly serve any file with this Content-Type.
    /// The old allow-list incorrectly rejected it before magic-byte check.
    #[test]
    fn validate_accepts_octet_stream_with_valid_png() {
        let png = make_png(1, 1);
        assert!(
            validate_image_response(Some("application/octet-stream"), &png).is_ok(),
            "application/octet-stream must pass through to magic-byte check"
        );
    }

    /// application/json body is rejected by magic bytes, not by Content-Type.
    #[test]
    fn validate_rejects_json_body_by_magic_bytes() {
        let json_body = b"{\"error\":\"invalid_auth\",\"ok\":false}";
        let result = validate_image_response(Some("application/json"), json_body);
        assert!(matches!(
            result,
            Err(MediaFetchError::InvalidImageBody { .. })
        ));
    }

    /// Missing Content-Type with invalid body: CDN stripping the header should
    /// still be caught by magic-byte detection.
    #[test]
    fn validate_rejects_html_body_with_missing_content_type() {
        let html_body = b"<!DOCTYPE html><html><body>error page</body></html>";
        let result = validate_image_response(None, html_body);
        assert!(matches!(
            result,
            Err(MediaFetchError::InvalidImageBody { .. })
        ));
    }

    #[test]
    fn validate_rejects_empty_body() {
        let result = validate_image_response(Some("image/png"), &[]);
        assert!(matches!(
            result,
            Err(MediaFetchError::InvalidImageBody { .. })
        ));
    }

    #[test]
    fn validate_rejects_truncated_png_header() {
        // PNG magic is 8 bytes; 4 bytes is not enough to identify the format.
        let truncated = [0x89u8, 0x50, 0x4e, 0x47];
        let result = validate_image_response(Some("image/png"), &truncated);
        assert!(matches!(
            result,
            Err(MediaFetchError::InvalidImageBody { .. })
        ));
    }

    #[test]
    fn truncated_png_body_must_not_produce_content_block() {
        // Valid PNG magic bytes (8 bytes) + partial IHDR -- body is too short to decode.
        // Previously: the <=1MB fallback in download_and_encode_image forwarded raw bytes
        // after resize_and_compress failed, reproducing the #776 poisoning class.
        // After removing the fallback, resize_and_compress failure must propagate as Err.
        let truncated: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG magic
            0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, // partial IHDR
        ];
        assert!(
            validate_image_response(Some("image/png"), truncated).is_ok(),
            "magic-byte check still passes for truncated body"
        );
        assert!(
            resize_and_compress(truncated).is_err(),
            "truncated PNG must fail at decode -- no raw-byte fallback allowed"
        );
    }

    #[test]
    fn media_fetch_error_display_renders() {
        let _ = MediaFetchError::NotAnImage.to_string();
        let _ = MediaFetchError::UnsupportedResponseType {
            actual: Some("text/html".into()),
        }
        .to_string();
        let s = MediaFetchError::UnsupportedResponseType { actual: None }.to_string();
        assert!(s.contains("none"), "None branch should render as 'none'");
        let _ = MediaFetchError::InvalidImageBody {
            magic_prefix_hex: "3c21444f43545950".into(),
        }
        .to_string();
        let _ = MediaFetchError::SizeExceeded {
            actual: 11_000_000,
            limit: 10_000_000,
        }
        .to_string();
        let _ = MediaFetchError::HttpStatus(reqwest::StatusCode::UNAUTHORIZED).to_string();
        let _ = MediaFetchError::ProcessingFailed(image::ImageError::Unsupported(
            image::error::UnsupportedError::from_format_and_kind(
                image::error::ImageFormatHint::Unknown,
                image::error::UnsupportedErrorKind::Color(image::ExtendedColorType::Rgba16),
            ),
        ))
        .to_string();
    }

    #[test]
    fn validate_accepts_webp_by_magic_bytes() {
        let img = image::RgbImage::new(1, 1);
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::WebP).unwrap();
        let webp_body = buf.into_inner();
        assert!(validate_image_response(Some("image/webp"), &webp_body).is_ok());
    }

    #[test]
    fn hex_prefix_formats_first_8_bytes() {
        let bytes = b"<!DOCTYPE html>";
        assert_eq!(hex_prefix(bytes), "3c21444f43545950");
    }

    #[test]
    fn hex_prefix_handles_short_buffer() {
        let bytes = [0xffu8, 0xd8];
        assert_eq!(hex_prefix(&bytes), "ffd8");
    }
}
