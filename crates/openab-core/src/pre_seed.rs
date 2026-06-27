use crate::config::{parse_s3_uri, OnFailure, PreSeedConfig};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Instant;
use tracing::{error, info, warn};

/// Maximum number of sources allowed.
const MAX_SOURCES: usize = 5;

/// Default max extracted (uncompressed) size: 500 MiB.
const DEFAULT_MAX_EXTRACTED_BYTES: u64 = 500 * 1024 * 1024;

/// Default max file count per zip.
const DEFAULT_MAX_FILE_COUNT: usize = 10_000;

/// Run the pre_seed phase: download zip archives from S3 and extract them in order.
pub async fn run(cfg: &PreSeedConfig) -> anyhow::Result<()> {
    if cfg.sources.is_empty() {
        return Ok(());
    }
    if cfg.sources.len() > MAX_SOURCES {
        anyhow::bail!(
            "hooks.pre_seed: too many sources ({}, max {})",
            cfg.sources.len(),
            MAX_SOURCES
        );
    }

    let target = match &cfg.target {
        Some(t) => std::path::PathBuf::from(t),
        None => dirs_home(),
    };

    info!(
        sources = cfg.sources.len(),
        target = %target.display(),
        "hooks.pre_seed: starting"
    );

    let mut s3_config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest());
    if let Some(ref region) = cfg.region {
        s3_config_loader = s3_config_loader.region(aws_config::Region::new(region.clone()));
    }
    if let Some(ref endpoint) = cfg.endpoint_url {
        s3_config_loader = s3_config_loader.endpoint_url(endpoint);
    }
    let aws_cfg = s3_config_loader.load().await;
    let s3 = aws_sdk_s3::Client::new(&aws_cfg);

    for (i, source) in cfg.sources.iter().enumerate() {
        let layer = i + 1;
        info!(
            layer,
            source = source.as_str(),
            "hooks.pre_seed: downloading"
        );

        let deadline = Instant::now() + std::time::Duration::from_secs(cfg.timeout_seconds);

        let result = download_and_extract(&s3, source, &target, cfg.max_bytes, deadline).await;

        let outcome = match result {
            Ok(()) => {
                info!(layer, "hooks.pre_seed: layer extracted successfully");
                continue;
            }
            Err(e) => e,
        };

        match cfg.on_failure {
            OnFailure::Abort => {
                error!(layer, error = %outcome, "hooks.pre_seed failed (on_failure=abort)");
                return Err(outcome);
            }
            OnFailure::Warn => {
                warn!(layer, error = %outcome, "hooks.pre_seed failed (on_failure=warn), continuing");
            }
        }
    }

    info!("hooks.pre_seed: complete");
    Ok(())
}

/// Download zip from S3, verify integrity, extract to a temp dir, then move into target.
/// The deadline is enforced cooperatively inside the blocking task.
async fn download_and_extract(
    s3: &aws_sdk_s3::Client,
    uri: &str,
    target: &Path,
    max_bytes: u64,
    deadline: Instant,
) -> anyhow::Result<()> {
    let (bucket, key) = parse_s3_uri(uri)?;

    // Check deadline before S3 call
    if Instant::now() >= deadline {
        anyhow::bail!("hooks.pre_seed: timed out before download for {uri}");
    }

    let resp = s3
        .get_object()
        .bucket(&bucket)
        .key(&key)
        .checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("S3 GetObject failed for {uri}: {e}"))?;

    if let Some(len) = resp.content_length() {
        if len as u64 > max_bytes {
            anyhow::bail!("hooks.pre_seed: {uri} too large ({len} bytes, max {max_bytes})");
        }
    }

    // Capture S3-native SHA-256 checksum if present (set during upload with --checksum-algorithm SHA256)
    let s3_checksum_sha256 = resp.checksum_sha256().map(|s| s.to_string());

    let body = resp
        .body
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("failed to read S3 body for {uri}: {e}"))?;
    let bytes = body.into_bytes();

    if bytes.len() as u64 > max_bytes {
        anyhow::bail!(
            "hooks.pre_seed: {uri} too large ({} bytes, max {max_bytes})",
            bytes.len()
        );
    }

    // SHA-256 verification: auto-verify S3-native checksum if present
    if let Some(ref s3_b64) = s3_checksum_sha256 {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual_hex = format!("{:x}", hasher.finalize());
        let s3_hex = base64_sha256_to_hex(s3_b64)?;
        if actual_hex != s3_hex {
            anyhow::bail!(
                "hooks.pre_seed: S3 checksum mismatch for {uri}: expected {s3_hex}, got {actual_hex}"
            );
        }
        info!(uri, "hooks.pre_seed: S3-native SHA-256 verified");
    }

    if Instant::now() >= deadline {
        anyhow::bail!("hooks.pre_seed: timed out after download for {uri}");
    }

    info!(
        uri,
        bytes = bytes.len(),
        "hooks.pre_seed: downloaded, extracting"
    );

    // Extract and move in a blocking task with cooperative deadline checking.
    let target = target.to_path_buf();
    // Bytes is Arc-backed, Clone is zero-copy (ref-count bump only)
    tokio::task::spawn_blocking(move || extract_and_apply(&bytes, &target, deadline))
        .await
        .map_err(|e| anyhow::anyhow!("hooks.pre_seed: extract task panicked: {e}"))??;

    Ok(())
}

/// Extract archive to a temp directory with budget enforcement, then move into target.
/// Supports zip and gzipped tarball formats (detected via magic bytes).
/// Checks deadline cooperatively before each file operation.
fn extract_and_apply(
    data: &[u8],
    target: &Path,
    deadline: Instant,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(target)?;
    let temp_dir = tempfile::tempdir_in(target)?;

    if data.starts_with(&[0x1f, 0x8b]) {
        extract_tarball_with_limits(data, temp_dir.path(), deadline)?;
    } else {
        extract_zip_with_limits(data, temp_dir.path(), deadline)?;
    }

    // Check deadline before applying to target
    if Instant::now() >= deadline {
        anyhow::bail!("hooks.pre_seed: timed out before applying to target");
    }

    move_recursive(temp_dir.path(), target, deadline)?;
    Ok(())
}

/// Extract a zip archive with cooperative deadline checks and extraction budget.
fn extract_zip_with_limits(data: &[u8], dest: &Path, deadline: Instant) -> anyhow::Result<()> {
    extract_zip_budgeted(
        data,
        dest,
        deadline,
        DEFAULT_MAX_FILE_COUNT,
        DEFAULT_MAX_EXTRACTED_BYTES,
    )
}

/// Inner extraction with configurable limits (enables testing with small budgets).
fn extract_zip_budgeted(
    data: &[u8],
    dest: &Path,
    deadline: Instant,
    max_file_count: usize,
    max_extracted_bytes: u64,
) -> anyhow::Result<()> {
    let cursor = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)?;

    let file_count = archive.len();
    if file_count > max_file_count {
        anyhow::bail!(
            "hooks.pre_seed: zip contains too many entries ({file_count}, max {max_file_count})"
        );
    }

    let mut total_extracted: u64 = 0;

    for i in 0..file_count {
        // Cooperative deadline check per file
        if i.is_multiple_of(100) && Instant::now() >= deadline {
            anyhow::bail!("hooks.pre_seed: timed out during extraction at entry {i}");
        }

        let mut file = archive.by_index(i)?;
        let name = file.enclosed_name().ok_or_else(|| {
            anyhow::anyhow!("hooks.pre_seed: invalid zip entry name at index {i}")
        })?;
        let out_path = dest.join(name);

        if file.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Check extracted size budget before writing
            let uncompressed = file.size();
            total_extracted += uncompressed;
            if total_extracted > max_extracted_bytes {
                anyhow::bail!(
                    "hooks.pre_seed: extracted size exceeds limit ({total_extracted} > {max_extracted_bytes})"
                );
            }

            let mut out = std::fs::File::create(&out_path)?;
            std::io::copy(&mut file, &mut out)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    let mode = mode & 0o0777; // strip suid/sgid/sticky
                    std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode))?;
                }
            }
        }
    }

    Ok(())
}

/// Extract a .tar.gz/.tgz archive with cooperative deadline checks and size budget.
fn extract_tarball_with_limits(data: &[u8], dest: &Path, deadline: Instant) -> anyhow::Result<()> {
    use flate2::read::GzDecoder;

    let decoder = GzDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(false);

    let mut file_count: usize = 0;
    let mut total_extracted: u64 = 0;

    for entry in archive.entries()? {
        let mut entry = entry?;

        file_count += 1;
        if file_count > DEFAULT_MAX_FILE_COUNT {
            anyhow::bail!(
                "hooks.pre_seed: tarball contains too many entries ({file_count}, max {DEFAULT_MAX_FILE_COUNT})"
            );
        }

        // Cooperative deadline check every 10 files
        if file_count.is_multiple_of(10) && Instant::now() >= deadline {
            anyhow::bail!("hooks.pre_seed: timed out during tarball extraction at entry {file_count}");
        }

        // Size budget
        total_extracted += entry.size();
        if total_extracted > DEFAULT_MAX_EXTRACTED_BYTES {
            anyhow::bail!(
                "hooks.pre_seed: extracted size exceeds limit ({total_extracted} > {DEFAULT_MAX_EXTRACTED_BYTES})"
            );
        }

        entry.unpack_in(dest)?;

        // Skip symlinks with escaping targets created by unpack_in
        #[cfg(unix)]
        if let Ok(path) = entry.path() {
            let out_path = dest.join(&*path);
            if out_path.symlink_metadata().map(|m| m.is_symlink()).unwrap_or(false) {
                if let Ok(target) = std::fs::read_link(&out_path) {
                    if target.is_absolute()
                        || target
                            .components()
                            .any(|c| c == std::path::Component::ParentDir)
                    {
                        let _ = std::fs::remove_file(&out_path);
                        warn!(
                            "hooks.pre_seed: removed symlink with escaping target: {}",
                            target.display()
                        );
                    }
                }
            }
        }

        // Manually set permissions (strip suid/sgid/sticky, like zip path)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(path) = entry.path() {
                let out_path = dest.join(path);
                if out_path.symlink_metadata().map(|m| m.file_type().is_file()).unwrap_or(false) {
                    let mode = entry.header().mode().unwrap_or(0o644) & 0o0777;
                    let _ = std::fs::set_permissions(
                        &out_path,
                        std::fs::Permissions::from_mode(mode),
                    );
                }
            }
        }
    }

    Ok(())
}

/// Create a symlink on Unix, or copy the resolved target on other platforms.
/// Rejects symlink targets that escape via absolute path or `..` components.
#[allow(unused_variables)]
fn create_symlink_or_copy(link_target: &Path, dst: &Path, src: &Path) -> anyhow::Result<()> {
    // Reject symlinks that could escape the target directory
    if link_target.is_absolute()
        || link_target
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        anyhow::bail!(
            "hooks.pre_seed: rejecting symlink with escaping target: {}",
            link_target.display()
        );
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(link_target, dst)?;
    }
    #[cfg(not(unix))]
    {
        let resolved = src.parent().unwrap_or(Path::new(".")).join(link_target);
        if resolved.exists() {
            std::fs::copy(&resolved, dst)?;
        } else {
            anyhow::bail!(
                "hooks.pre_seed: symlink target does not exist: {}",
                resolved.display()
            );
        }
    }
    Ok(())
}

/// Recursively move files from src directory into dst directory.
/// Checks deadline cooperatively.
fn move_recursive(src: &Path, dst: &Path, deadline: Instant) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(src)? {
        if Instant::now() >= deadline {
            anyhow::bail!("hooks.pre_seed: timed out during move to target");
        }

        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        let meta = src_path.symlink_metadata()?;
        if meta.is_symlink() {
            // Preserve symlinks as-is without following
            let link_target = std::fs::read_link(&src_path)?;
            // Remove existing dst (file or directory) before creating symlink
            if let Ok(dst_meta) = dst_path.symlink_metadata() {
                if dst_meta.is_dir() {
                    std::fs::remove_dir_all(&dst_path)?;
                } else {
                    std::fs::remove_file(&dst_path)?;
                }
            }
            create_symlink_or_copy(&link_target, &dst_path, &src_path)?;
        } else if meta.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            move_recursive(&src_path, &dst_path, deadline)?;
        } else {
            if std::fs::rename(&src_path, &dst_path).is_err() {
                std::fs::copy(&src_path, &dst_path)?;
                std::fs::remove_file(&src_path)?;
            }
        }
    }
    Ok(())
}

/// Decode a base64-encoded SHA-256 (as returned by S3) to lowercase hex.
fn base64_sha256_to_hex(b64: &str) -> anyhow::Result<String> {
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| anyhow::anyhow!("hooks.pre_seed: invalid base64 in S3 checksum: {e}"))?;
    Ok(hex::encode(decoded))
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/home/agent"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_zip_basic() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);

        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("hello.txt", options).unwrap();
        writer.write_all(b"world").unwrap();
        writer.start_file("sub/nested.txt", options).unwrap();
        writer.write_all(b"nested content").unwrap();
        let cursor = writer.finish().unwrap();

        extract_zip_with_limits(cursor.get_ref(), dir.path(), deadline).unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
            "world"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sub/nested.txt")).unwrap(),
            "nested content"
        );
    }

    #[test]
    fn extract_and_apply_atomic() {
        use std::io::Write;
        let target = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);

        std::fs::write(target.path().join("existing.txt"), "keep").unwrap();

        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("new.txt", options).unwrap();
        writer.write_all(b"added").unwrap();
        let cursor = writer.finish().unwrap();

        extract_and_apply(cursor.get_ref(), target.path(), deadline).unwrap();

        assert_eq!(
            std::fs::read_to_string(target.path().join("existing.txt")).unwrap(),
            "keep"
        );
        assert_eq!(
            std::fs::read_to_string(target.path().join("new.txt")).unwrap(),
            "added"
        );
    }

    #[test]
    fn extract_respects_expired_deadline() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        // Already expired deadline
        let deadline = Instant::now() - std::time::Duration::from_secs(1);

        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("a.txt", options).unwrap();
        writer.write_all(b"data").unwrap();
        let cursor = writer.finish().unwrap();

        // extract_and_apply should fail due to expired deadline
        let result = extract_and_apply(cursor.get_ref(), dir.path(), deadline);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[test]
    fn extract_zip_overwrites() {
        use std::io::Write;
        let target = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        std::fs::write(target.path().join("hello.txt"), "original").unwrap();

        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("hello.txt", options).unwrap();
        writer.write_all(b"overwritten").unwrap();
        let cursor = writer.finish().unwrap();

        extract_and_apply(cursor.get_ref(), target.path(), deadline).unwrap();

        assert_eq!(
            std::fs::read_to_string(target.path().join("hello.txt")).unwrap(),
            "overwritten"
        );
    }

    #[tokio::test]
    async fn run_empty_sources() {
        let cfg = PreSeedConfig::default();
        assert!(run(&cfg).await.is_ok());
    }

    #[tokio::test]
    async fn run_too_many_sources() {
        let cfg = PreSeedConfig {
            sources: vec!["s3://b/k.zip".into(); 6],
            ..Default::default()
        };
        assert!(run(&cfg).await.is_err());
    }

    #[test]
    fn default_has_correct_values() {
        let cfg = PreSeedConfig::default();
        assert_eq!(cfg.timeout_seconds, 300);
        assert_eq!(cfg.max_bytes, 100 * 1024 * 1024);
        assert_eq!(cfg.on_failure, OnFailure::Abort);
        assert!(cfg.sources.is_empty());
    }

    #[test]
    fn move_respects_deadline() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("f.txt"), "x").unwrap();

        let expired = Instant::now() - std::time::Duration::from_secs(1);
        let result = move_recursive(src.path(), dst.path(), expired);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[test]
    fn extract_rejects_exceeding_extracted_bytes() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);

        // Create a zip with 3 files of 10 bytes each (30 bytes total extracted)
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        for i in 0..3 {
            writer.start_file(format!("file_{i}.txt"), options).unwrap();
            writer.write_all(&[b'x'; 10]).unwrap();
        }
        let cursor = writer.finish().unwrap();

        // Set max extracted bytes to 20 — fails on 3rd file (cumulative 30 > 20)
        let result = extract_zip_budgeted(cursor.get_ref(), dir.path(), deadline, 10_000, 20);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("extracted size exceeds limit"),
            "should fail on extracted bytes limit"
        );
    }

    #[test]
    fn extract_rejects_exceeding_file_count() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);

        // Create a zip with 5 files
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        for i in 0..5 {
            writer.start_file(format!("f_{i}.txt"), options).unwrap();
            writer.write_all(b"x").unwrap();
        }
        let cursor = writer.finish().unwrap();

        // Set max file count to 3 — should fail (5 > 3)
        let result = extract_zip_budgeted(cursor.get_ref(), dir.path(), deadline, 3, u64::MAX);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("too many entries"),
            "should fail on file count limit"
        );
    }

    #[test]
    fn extract_tarball_basic() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let dir = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);

        let buf = Vec::new();
        let enc = GzEncoder::new(buf, Compression::default());
        let mut builder = tar::Builder::new(enc);

        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, "hello.txt", &b"world"[..])
            .unwrap();

        let mut header2 = tar::Header::new_gnu();
        header2.set_size(14);
        header2.set_mode(0o644);
        builder
            .append_data(&mut header2, "sub/nested.txt", &b"nested content"[..])
            .unwrap();

        let enc = builder.into_inner().unwrap();
        let tarball_bytes = enc.finish().unwrap();

        extract_tarball_with_limits(&tarball_bytes, dir.path(), deadline).unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
            "world"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sub/nested.txt")).unwrap(),
            "nested content"
        );
    }

    #[test]
    fn extract_and_apply_detects_tarball_via_magic_bytes() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let target = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);

        let buf = Vec::new();
        let enc = GzEncoder::new(buf, Compression::default());
        let mut builder = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, "hello.txt", &b"world"[..])
            .unwrap();
        let enc = builder.into_inner().unwrap();
        let tarball_bytes = enc.finish().unwrap();

        // Magic bytes detection — no URI needed
        extract_and_apply(&tarball_bytes, target.path(), deadline).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.path().join("hello.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn extract_tarball_respects_deadline() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let dir = tempfile::tempdir().unwrap();
        let expired = Instant::now() - std::time::Duration::from_secs(1);

        let buf = Vec::new();
        let enc = GzEncoder::new(buf, Compression::default());
        let mut builder = tar::Builder::new(enc);
        // Create > 10 files to trigger deadline check
        for i in 0..11 {
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            builder
                .append_data(&mut header, format!("f{i}.txt"), &b"x"[..])
                .unwrap();
        }
        let enc = builder.into_inner().unwrap();
        let tarball_bytes = enc.finish().unwrap();

        let result = extract_tarball_with_limits(&tarball_bytes, dir.path(), expired);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn move_recursive_preserves_symlinks() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);

        // Create a regular file and a symlink pointing to it
        std::fs::write(src.path().join("real.txt"), "content").unwrap();
        std::os::unix::fs::symlink("real.txt", src.path().join("link.txt")).unwrap();

        move_recursive(src.path(), dst.path(), deadline).unwrap();

        let dst_link = dst.path().join("link.txt");
        let meta = dst_link.symlink_metadata().unwrap();
        assert!(meta.is_symlink(), "destination should be a symlink");
        assert_eq!(std::fs::read_link(&dst_link).unwrap().to_str().unwrap(), "real.txt");
        assert_eq!(std::fs::read_to_string(dst.path().join("real.txt")).unwrap(), "content");
    }

    #[cfg(unix)]
    #[test]
    fn move_recursive_symlink_overwrites_existing_dir() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);

        // src has a symlink named "item"
        std::fs::write(src.path().join("target.txt"), "x").unwrap();
        std::os::unix::fs::symlink("target.txt", src.path().join("item")).unwrap();

        // dst has a directory named "item"
        std::fs::create_dir(dst.path().join("item")).unwrap();

        move_recursive(src.path(), dst.path(), deadline).unwrap();

        let dst_item = dst.path().join("item");
        let meta = dst_item.symlink_metadata().unwrap();
        assert!(meta.is_symlink(), "should have replaced directory with symlink");
    }

    #[cfg(unix)]
    #[test]
    fn extract_and_apply_succeeds_with_non_writable_parent() {
        use std::os::unix::fs::PermissionsExt;
        use std::io::Write;

        // Regression test: target is writable but target.parent() is read-only.
        // Old code used tempdir_in(target.parent()) which would fail here.
        // New code uses tempdir_in(target) which succeeds.
        let base = tempfile::tempdir().unwrap();
        let restricted = base.path().join("restricted");
        std::fs::create_dir(&restricted).unwrap();

        // Create target directory (writable)
        let target = restricted.join("target");
        std::fs::create_dir(&target).unwrap();

        // Lock down parent so tempdir_in(parent) would fail
        std::fs::set_permissions(&restricted, std::fs::Permissions::from_mode(0o555)).unwrap();

        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("test.txt", options).unwrap();
        writer.write_all(b"data").unwrap();
        let cursor = writer.finish().unwrap();

        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        let result = extract_and_apply(cursor.get_ref(), &target, deadline);

        // Restore permissions before asserting (for cleanup)
        std::fs::set_permissions(&restricted, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Should succeed because tempdir_in(target) works even with read-only parent
        result.unwrap();
        assert_eq!(std::fs::read_to_string(target.join("test.txt")).unwrap(), "data");
    }

    #[cfg(unix)]
    #[test]
    fn extract_and_apply_succeeds_with_writable_target() {
        use std::io::Write;

        let base = tempfile::tempdir().unwrap();
        let target = base.path().join("deep").join("target");
        // target doesn't exist yet — create_dir_all should handle it

        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("test.txt", options).unwrap();
        writer.write_all(b"hello").unwrap();
        let cursor = writer.finish().unwrap();

        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        extract_and_apply(cursor.get_ref(), &target, deadline).unwrap();

        assert_eq!(std::fs::read_to_string(target.join("test.txt")).unwrap(), "hello");
    }
}
