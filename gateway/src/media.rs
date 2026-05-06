use image::ImageReader;
use std::io::Cursor;

pub const IMAGE_MAX_DIMENSION_PX: u32 = 1200;
pub const IMAGE_JPEG_QUALITY: u8 = 75;
pub const IMAGE_MAX_DOWNLOAD: u64 = 10 * 1024 * 1024; // 10 MB
pub const FILE_MAX_DOWNLOAD: u64 = 512 * 1024; // 512 KB
pub const AUDIO_MAX_DOWNLOAD: u64 = 20 * 1024 * 1024; // 20 MB

/// Resize image so longest side <= 1200px, then encode as JPEG.
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
