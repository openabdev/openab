use crate::acp::ContentBlock;
use crate::config::{OutboundConfig, SttConfig};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use image::ImageReader;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::LazyLock;
use tracing::{debug, error, info, warn};

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

/// Extensions recognised as text-based files that can be inlined into the prompt.
const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "csv", "log", "md", "json", "jsonl", "yaml", "yml", "toml", "xml",
    "rs", "py", "js", "ts", "jsx", "tsx", "go", "java", "c", "cpp", "h", "hpp",
    "rb", "sh", "bash", "zsh", "fish", "ps1", "bat", "sql", "html", "css",
    "scss", "less", "ini", "cfg", "conf", "env",
];

/// Exact filenames (no extension) recognised as text files.
const TEXT_FILENAMES: &[&str] = &[
    "dockerfile", "makefile", "justfile", "rakefile", "gemfile",
    "procfile", "vagrantfile", ".gitignore", ".dockerignore", ".editorconfig",
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
/// Files larger than 512 KB are skipped to avoid bloating the prompt.
///
/// Pass `auth_token` for platforms that require authentication (e.g. Slack private files).
///
/// Note: the caller already guards total size via a total cap; the per-file
/// MAX_SIZE check here is intentional defense-in-depth so this function remains
/// self-contained and safe when called from other contexts.
pub async fn download_and_read_text_file(
    url: &str,
    filename: &str,
    size: u64,
    auth_token: Option<&str>,
) -> Option<(ContentBlock, u64)> {
    const MAX_SIZE: u64 = 512 * 1024; // 512 KB

    if size > MAX_SIZE {
        tracing::warn!(filename, size, "text file exceeds 512KB limit, skipping");
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
    let bytes = resp.bytes().await.ok()?;
    let actual_size = bytes.len() as u64;

    // Defense-in-depth: verify actual download size
    if actual_size > MAX_SIZE {
        tracing::warn!(filename, size = actual_size, "downloaded text file exceeds 512KB limit, skipping");
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

// --- Outbound attachments ---
//
// Agent → chat file upload. Agents write `![alt](/path)` markdown in their
// response; this module extracts and validates paths. Only files under
// `~/.oab/outgoing/` are permitted — the agent must explicitly copy files
// there before referencing them.

/// Regex for outbound attachment markers: `![alt](/path/to/file)`.
static OUTBOUND_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"!\[[^\]]*\]\((/[^\)]+)\)").unwrap()
});

/// Check file magic bytes to verify it is an image. Only images are
/// allowed for outbound attachments to prevent data exfiltration via
/// text files.
fn is_image_file(path: &std::path::Path) -> bool {
    let Ok(buf) = std::fs::read(path) else {
        return false;
    };
    // Check magic bytes for common image formats
    let header = buf.get(..12).unwrap_or(&buf);
    matches!(
        header,
        [0x89, 0x50, 0x4E, 0x47, ..]           // PNG
        | [0xFF, 0xD8, 0xFF, ..]                 // JPEG
        | [0x47, 0x49, 0x46, 0x38, ..]           // GIF
        | [0x52, 0x49, 0x46, 0x46, _, _, _, _, 0x57, 0x45, 0x42, 0x50] // WebP
        | [0x42, 0x4D, ..]                       // BMP
    )
}

/// Scan agent response `text` for `![alt](/path)` markers, validate each
/// path against `config`, and return `(cleaned_text, list_of_paths)`.
///
/// Only files under `OutboundConfig::outgoing_dir()` are accepted.
/// Markers for accepted files are stripped; rejected markers stay visible.
pub fn extract_outbound_attachments(
    text: &str,
    config: &OutboundConfig,
) -> (String, Vec<PathBuf>) {
    if !config.enabled {
        return (text.to_string(), Vec::new());
    }

    let outgoing_dir = OutboundConfig::outgoing_dir();
    if let Err(e) = std::fs::create_dir_all(&outgoing_dir) {
        warn!(dir = %outgoing_dir.display(), error = %e, "outbound: cannot create outgoing dir");
        return (text.to_string(), Vec::new());
    }

    let canonical_outgoing = match std::fs::canonicalize(&outgoing_dir) {
        Ok(p) => p,
        Err(e) => {
            warn!(dir = %outgoing_dir.display(), error = %e, "outbound: cannot canonicalize outgoing dir");
            return (text.to_string(), Vec::new());
        }
    };

    let mut attachments = Vec::new();
    let mut markers_to_strip = Vec::new();

    for cap in OUTBOUND_RE.captures_iter(text) {
        if attachments.len() >= config.max_per_message {
            warn!(cap = config.max_per_message, "outbound: per-message cap hit");
            break;
        }

        let full_match = cap.get(0).unwrap().as_str();
        let path_str = &cap[1];
        let path = PathBuf::from(path_str);

        let canonical = match std::fs::canonicalize(&path) {
            Ok(p) => p,
            Err(e) => {
                debug!(path = %path_str, error = %e, "outbound: cannot canonicalize");
                continue;
            }
        };

        if !canonical.starts_with(&canonical_outgoing) {
            warn!(path = %path_str, canonical = %canonical.display(), "outbound: path not in outgoing dir");
            continue;
        }

        match std::fs::metadata(&canonical) {
            Ok(meta) if meta.is_file() && meta.len() <= config.max_size_bytes() => {
                if !is_image_file(&canonical) {
                    warn!(path = %canonical.display(), "outbound: not an image file, only images are allowed");
                    continue;
                }
                info!(path = %canonical.display(), size = meta.len(), "outbound: attachment accepted");
                attachments.push(canonical);
                markers_to_strip.push(full_match.to_string());
            }
            Ok(meta) if meta.len() > config.max_size_bytes() => {
                warn!(path = %canonical.display(), size = meta.len(), limit_mb = config.max_file_size_mb, "outbound: over size limit");
            }
            Ok(_) => {
                warn!(path = %canonical.display(), "outbound: not a regular file");
            }
            Err(e) => {
                debug!(path = %canonical.display(), error = %e, "outbound: metadata error");
            }
        }
    }

    let mut cleaned = text.to_string();
    for marker in &markers_to_strip {
        cleaned = cleaned.replace(marker, "");
    }
    while cleaned.contains("\n\n\n") {
        cleaned = cleaned.replace("\n\n\n", "\n\n");
    }
    (cleaned.trim().to_string(), attachments)
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
}

#[cfg(test)]
mod outbound_tests {
    use super::*;
    use crate::config::OutboundConfig;

    fn cfg_enabled() -> OutboundConfig {
        OutboundConfig {
            enabled: true,
            ..OutboundConfig::default()
        }
    }

    fn outgoing_dir() -> PathBuf {
        OutboundConfig::outgoing_dir()
    }

    #[test]
    fn disabled_by_default_is_noop() {
        let cfg = OutboundConfig::default();
        assert!(!cfg.enabled);
        let text = "![foo](/tmp/does-not-matter.png)";
        let (cleaned, atts) = extract_outbound_attachments(text, &cfg);
        assert_eq!(cleaned, text);
        assert!(atts.is_empty());
    }

    #[test]
    fn enabled_extracts_outgoing_file() {
        let dir = outgoing_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_happy.png");
        // Valid PNG magic bytes
        std::fs::write(&path, &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        let text = format!("Here: ![screenshot]({}) done.", path.display());
        let (cleaned, atts) = extract_outbound_attachments(&text, &cfg_enabled());
        assert_eq!(atts.len(), 1);
        assert!(!cleaned.contains("test_happy"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn blocks_path_outside_outgoing() {
        let text = "![secret](/etc/passwd)";
        let (cleaned, atts) = extract_outbound_attachments(text, &cfg_enabled());
        assert!(atts.is_empty());
        assert!(cleaned.contains("/etc/passwd"));
    }

    #[test]
    fn blocks_tmp_path() {
        let path = "/tmp/openab_outbound_test.png";
        std::fs::write(path, b"x").unwrap();
        let text = format!("![img]({path})");
        let (_, atts) = extract_outbound_attachments(&text, &cfg_enabled());
        assert!(atts.is_empty(), "/tmp must be blocked, only ~/.oab/outgoing/ allowed");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn blocks_symlink_escape() {
        let dir = outgoing_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("escape.png");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("/etc/hosts", &link).unwrap();
        let text = format!("![esc]({})", link.display());
        let (_, atts) = extract_outbound_attachments(&text, &cfg_enabled());
        assert!(atts.is_empty(), "symlink escaping outgoing dir must be blocked");
        std::fs::remove_file(&link).ok();
    }

    #[test]
    fn blocks_path_traversal() {
        let dir = outgoing_dir();
        let text = format!("![x]({}/../../../etc/hosts)", dir.display());
        let (_, atts) = extract_outbound_attachments(&text, &cfg_enabled());
        assert!(atts.is_empty(), "path traversal must be blocked");
    }

    #[test]
    fn enforces_max_per_message() {
        let dir = outgoing_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let png = [0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let mut text = String::new();
        let mut paths = Vec::new();
        for i in 0..5 {
            let p = dir.join(format!("cap_{i}.png"));
            std::fs::write(&p, &png).unwrap();
            text.push_str(&format!("![a{i}]({})\n", p.display()));
            paths.push(p);
        }
        let cfg = OutboundConfig {
            enabled: true,
            max_per_message: 2,
            ..OutboundConfig::default()
        };
        let (_, atts) = extract_outbound_attachments(&text, &cfg);
        assert_eq!(atts.len(), 2, "must cap at max_per_message");
        for p in &paths { std::fs::remove_file(p).ok(); }
    }

    #[test]
    fn blocks_text_file_exfiltration() {
        let dir = outgoing_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secrets.txt");
        std::fs::write(&path, b"SECRET_KEY=hunter2").unwrap();
        let text = format!("![leak]({})", path.display());
        let (cleaned, atts) = extract_outbound_attachments(&text, &cfg_enabled());
        assert!(atts.is_empty(), "text files must be blocked");
        assert!(cleaned.contains("secrets.txt"), "blocked marker stays visible");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn accepts_real_png() {
        let dir = outgoing_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("real.png");
        // Minimal valid PNG header
        let png_header: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        std::fs::write(&path, &png_header).unwrap();
        let text = format!("![img]({})", path.display());
        let (_, atts) = extract_outbound_attachments(&text, &cfg_enabled());
        assert_eq!(atts.len(), 1, "real PNG must be accepted");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn enforces_max_file_size() {
        let dir = outgoing_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("large.bin");
        // PNG header + padding to exceed size limit
        let mut data = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        data.resize(2 * 1024 * 1024, 0);
        std::fs::write(&path, &data).unwrap();
        let cfg = OutboundConfig {
            enabled: true,
            max_file_size_mb: 1,
            ..OutboundConfig::default()
        };
        let text = format!("![big]({})", path.display());
        let (_, atts) = extract_outbound_attachments(&text, &cfg);
        assert!(atts.is_empty(), "file exceeding max_file_size_mb must be blocked");
        std::fs::remove_file(&path).ok();
    }
}