//! S3/R2-compatible object store for uploading file attachments
//! and returning presigned GET URLs.

use crate::config::FilestoreConfig;
use std::time::Duration;
use tracing::{error, info};

/// Manages uploads to an S3-compatible object store and generates presigned
/// GET URLs for retrieval without authentication.
pub struct Filestore {
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
    presigned_ttl: Duration,
    max_file_size: u64,
}

/// What `upload_and_presign` has always stored, kept named so the compatibility
/// wrapper and its doc comment cannot drift apart.
const TEXT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

const MAX_PRESIGNED_TTL: u64 = 7 * 24 * 60 * 60;

fn cap_presigned_ttl(configured: u64) -> u64 {
    if configured > MAX_PRESIGNED_TTL {
        tracing::warn!(
            configured,
            capped = MAX_PRESIGNED_TTL,
            "presigned_ttl exceeds 7-day maximum, capping"
        );
    }
    configured.min(MAX_PRESIGNED_TTL)
}

/// The lifetime as the agent reads it. `presigned_ttl` governs how long an
/// authorization lives, so a sub-minute value is rendered, never raised.
pub(crate) fn format_presigned_lifetime(ttl_secs: u64) -> String {
    match ttl_secs {
        1 => "1 second".to_string(),
        s if s < 60 => format!("{s} seconds"),
        // Unchanged from before the sub-minute branch existed, so the #738 hint
        // stays byte-identical at every TTL that path could already render.
        s => format!("{} minutes", s / 60),
    }
}

impl Filestore {
    /// Initialize a new Filestore from the given configuration.
    ///
    /// Builds an S3 client with optional custom endpoint and explicit credentials.
    /// Falls back to the standard AWS provider chain when credentials are not
    /// specified in config.
    pub async fn new(config: &FilestoreConfig) -> Self {
        let mut sdk_config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(config.region.clone()));

        if let (Some(access_key), Some(secret_key)) =
            (&config.access_key_id, &config.secret_access_key)
        {
            // Only use explicit credentials if non-empty. When env var expansion
            // produces an empty string (e.g. ${UNSET_VAR} → ""), fall back to
            // the standard AWS provider chain (IRSA, instance role, env, etc.).
            if !access_key.is_empty() && !secret_key.is_empty() {
                let creds = aws_sdk_s3::config::Credentials::new(
                    access_key.clone(),
                    secret_key.clone(),
                    None,
                    None,
                    "filestore-config",
                );
                sdk_config_loader = sdk_config_loader.credentials_provider(creds);
            }
        }

        let sdk_config = sdk_config_loader.load().await;

        let mut s3_config_builder = aws_sdk_s3::config::Builder::from(&sdk_config);

        if let Some(endpoint) = &config.endpoint {
            // Path-style access is required for most S3-compatible services
            // (R2, MinIO) but deprecated by AWS S3 itself.
            s3_config_builder = s3_config_builder
                .endpoint_url(endpoint.clone())
                .force_path_style(true);
        }

        let client = aws_sdk_s3::Client::from_conf(s3_config_builder.build());

        let ttl_secs = cap_presigned_ttl(config.presigned_ttl);

        // Cap max_file_size_mb at 500 MB absolute maximum.
        const ABSOLUTE_MAX_FILE_SIZE_MB: u64 = 500;
        let max_file_size_mb = config.max_file_size_mb.min(ABSOLUTE_MAX_FILE_SIZE_MB);
        if config.max_file_size_mb > ABSOLUTE_MAX_FILE_SIZE_MB {
            tracing::warn!(
                configured = config.max_file_size_mb,
                capped = ABSOLUTE_MAX_FILE_SIZE_MB,
                "max_file_size_mb exceeds 500 MB maximum, capping"
            );
        }

        Self {
            client,
            bucket: config.bucket.clone(),
            prefix: config.prefix.clone(),
            presigned_ttl: Duration::from_secs(ttl_secs),
            max_file_size: max_file_size_mb * 1024 * 1024,
        }
    }

    /// Uploads as `text/plain; charset=utf-8`. Kept at its original signature and
    /// content type for external callers; new code wants the typed method below.
    pub async fn upload_and_presign(&self, filename: &str, data: &[u8]) -> anyhow::Result<String> {
        self.upload_and_presign_with_content_type(filename, data, Some(TEXT_CONTENT_TYPE))
            .await
    }

    /// Upload a file to S3 and return a presigned GET URL.
    ///
    /// The object key is `{prefix}{uuid}_{filename}`. On success returns the
    /// presigned URL as a String. On failure logs the error and returns Err.
    pub async fn upload_and_presign_with_content_type(
        &self,
        filename: &str,
        data: &[u8],
        content_type: Option<&str>,
    ) -> anyhow::Result<String> {
        // Sanitize filename: strip path separators, traversal sequences,
        // double quotes (breaks Content-Disposition header parsing), and
        // non-ASCII chars. Limit length to prevent excessively long S3 keys.
        let safe_name: String = filename
            .replace(['/', '\\', '\0', '"'], "_")
            .replace("..", "_")
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(200)
            .collect();
        let safe_name = if safe_name.is_empty() { "unnamed" } else { &safe_name };
        let key = format!(
            "{}{}_{}",
            self.prefix,
            uuid::Uuid::new_v4(),
            safe_name
        );

        // Upload the object (3-minute timeout to prevent indefinite hangs)
        const UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
        let upload_fut = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .content_type(content_type.unwrap_or("application/octet-stream"))
            .content_disposition(format!("attachment; filename=\"{safe_name}\""))
            .body(aws_sdk_s3::primitives::ByteStream::from(data.to_vec()))
            .send();

        tokio::time::timeout(UPLOAD_TIMEOUT, upload_fut)
            .await
            .map_err(|_| {
                error!(bucket = %self.bucket, key = %key, "filestore upload timed out (180s)");
                anyhow::anyhow!("filestore upload timed out")
            })?
            .map_err(|e| {
                error!(bucket = %self.bucket, key = %key, error = %e, "filestore upload failed");
                anyhow::anyhow!("filestore upload failed: {e}")
            })?;

        info!(bucket = %self.bucket, key = %key, size = data.len(), "filestore upload complete");

        // Generate presigned GET URL
        let presigning_config =
            aws_sdk_s3::presigning::PresigningConfig::expires_in(self.presigned_ttl)
                .map_err(|e| anyhow::anyhow!("presigning config error: {e}"))?;

        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .presigned(presigning_config)
            .await
            .map_err(|e| {
                error!(bucket = %self.bucket, key = %key, error = %e, "presigned URL generation failed");
                anyhow::anyhow!("presigned URL generation failed: {e}")
            })?;

        Ok(presigned.uri().to_string())
    }

    /// Upload a file to S3 using multipart upload (streaming) and return a presigned GET URL.
    ///
    /// This method streams data in chunks to minimize memory usage.
    /// Each part is ~16 MB. Total file size is checked against max_file_size.
    /// Returns `(presigned_url, actual_bytes_uploaded)`.
    pub async fn stream_upload_and_presign(
        &self,
        filename: &str,
        mut stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
        reported_size: u64,
        content_type: Option<&str>,
    ) -> anyhow::Result<(String, u64)> {
        use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
        use futures_util::StreamExt;

        // Sanitize filename (same logic as upload_and_presign)
        let safe_name: String = filename
            .replace(['/', '\\', '\0', '"'], "_")
            .replace("..", "_")
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(200)
            .collect();
        let safe_name = if safe_name.is_empty() {
            "unnamed"
        } else {
            &safe_name
        };
        let key = format!("{}{}_{}",self.prefix, uuid::Uuid::new_v4(), safe_name);

        // Pre-check reported size
        if reported_size > self.max_file_size {
            return Err(anyhow::anyhow!(
                "reported file size ({reported_size}) exceeds max ({})",
                self.max_file_size
            ));
        }

        // Initiate multipart upload
        let create_resp = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(&key)
            .content_type(content_type.unwrap_or("application/octet-stream"))
            .content_disposition(format!("attachment; filename=\"{safe_name}\""))
            .send()
            .await
            .map_err(|e| {
                error!(bucket = %self.bucket, key = %key, error = %e, "create_multipart_upload failed");
                anyhow::anyhow!("create_multipart_upload failed: {e}")
            })?;

        let upload_id = create_resp
            .upload_id()
            .ok_or_else(|| anyhow::anyhow!("create_multipart_upload returned no upload_id"))?
            .to_string();

        // Stream in 16 MB chunks
        const PART_SIZE: usize = 16 * 1024 * 1024; // 16 MB
        let mut buffer = Vec::with_capacity(PART_SIZE);
        let mut total_bytes: u64 = 0;
        let mut parts: Vec<CompletedPart> = Vec::new();
        let mut part_number: i32 = 1;
        let mut upload_error: Option<anyhow::Error> = None;

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    upload_error = Some(anyhow::anyhow!("stream read error: {e}"));
                    break;
                }
            };
            total_bytes += chunk.len() as u64;

            if total_bytes > self.max_file_size {
                upload_error = Some(anyhow::anyhow!(
                    "file exceeds max size ({} > {})",
                    total_bytes,
                    self.max_file_size
                ));
                break;
            }

            buffer.extend_from_slice(&chunk);

            if buffer.len() >= PART_SIZE {
                let part_data: Vec<u8> = std::mem::take(&mut buffer);
                match self
                    .client
                    .upload_part()
                    .bucket(&self.bucket)
                    .key(&key)
                    .upload_id(&upload_id)
                    .part_number(part_number)
                    .body(aws_sdk_s3::primitives::ByteStream::from(part_data))
                    .send()
                    .await
                {
                    Ok(resp) => {
                        match resp.e_tag {
                            Some(tag) => {
                                parts.push(
                                    CompletedPart::builder()
                                        .part_number(part_number)
                                        .e_tag(tag)
                                        .build(),
                                );
                                part_number += 1;
                            }
                            None => {
                                upload_error = Some(anyhow::anyhow!("upload_part {part_number} returned no ETag"));
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        upload_error = Some(anyhow::anyhow!("upload_part {part_number} failed: {e}"));
                        break;
                    }
                }
            }
        }

        // Upload remaining buffer as the final part (if no error so far)
        if upload_error.is_none() && !buffer.is_empty() {
            match self
                .client
                .upload_part()
                .bucket(&self.bucket)
                .key(&key)
                .upload_id(&upload_id)
                .part_number(part_number)
                .body(aws_sdk_s3::primitives::ByteStream::from(std::mem::take(&mut buffer)))
                .send()
                .await
            {
                Ok(resp) => {
                    match resp.e_tag {
                        Some(tag) => {
                            parts.push(
                                CompletedPart::builder()
                                    .part_number(part_number)
                                    .e_tag(tag)
                                    .build(),
                            );
                        }
                        None => {
                            upload_error = Some(anyhow::anyhow!("upload_part {part_number} (final) returned no ETag"));
                        }
                    }
                }
                Err(e) => {
                    upload_error = Some(anyhow::anyhow!("upload_part {part_number} (final) failed: {e}"));
                }
            }
        }

        // On error, abort the multipart upload to clean up
        if let Some(e) = upload_error {
            error!(
                bucket = %self.bucket,
                key = %key,
                upload_id = %upload_id,
                error = %e,
                "streaming upload failed, aborting multipart upload"
            );
            if let Err(abort_err) = self
                .client
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(&key)
                .upload_id(&upload_id)
                .send()
                .await
            {
                error!(
                    bucket = %self.bucket,
                    key = %key,
                    upload_id = %upload_id,
                    error = %abort_err,
                    "abort_multipart_upload also failed"
                );
            }
            return Err(e);
        }

        // Edge case: stream produced no data. This should not happen for files
        // reported > 512 KB — it indicates a download failure or protocol error.
        // R2 and MinIO reject 0-byte UploadPart, so treat this as an error.
        if parts.is_empty() && total_bytes == 0 {
            error!(bucket = %self.bucket, key = %key, "stream produced no data, aborting");
            let _ = self
                .client
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(&key)
                .upload_id(&upload_id)
                .send()
                .await;
            return Err(anyhow::anyhow!("stream produced no data — file may be empty or download failed"));
        }

        // If buffer has remaining data but no parts yet (file < 16 MB), upload as single part
        if parts.is_empty() {
            let part_data = buffer;
            match self
                .client
                .upload_part()
                .bucket(&self.bucket)
                .key(&key)
                .upload_id(&upload_id)
                .part_number(1)
                .body(aws_sdk_s3::primitives::ByteStream::from(part_data))
                .send()
                .await
            {
                Ok(resp) => {
                    let e_tag = match resp.e_tag {
                        Some(tag) => tag,
                        None => {
                            error!(bucket = %self.bucket, key = %key, "upload single part returned no ETag, aborting");
                            let _ = self.client
                                .abort_multipart_upload()
                                .bucket(&self.bucket)
                                .key(&key)
                                .upload_id(&upload_id)
                                .send()
                                .await;
                            return Err(anyhow::anyhow!("upload single part returned no ETag"));
                        }
                    };
                    parts.push(
                        CompletedPart::builder()
                            .part_number(1)
                            .e_tag(e_tag)
                            .build(),
                    );
                }
                Err(e) => {
                    error!(bucket = %self.bucket, key = %key, error = %e, "upload single part failed, aborting");
                    let _ = self.client
                        .abort_multipart_upload()
                        .bucket(&self.bucket)
                        .key(&key)
                        .upload_id(&upload_id)
                        .send()
                        .await;
                    return Err(anyhow::anyhow!("upload single part failed: {e}"));
                }
            }
        }

        // Complete multipart upload
        let completed_upload = CompletedMultipartUpload::builder()
            .set_parts(Some(parts))
            .build();

        if let Err(e) = self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(&key)
            .upload_id(&upload_id)
            .multipart_upload(completed_upload)
            .send()
            .await
        {
            error!(bucket = %self.bucket, key = %key, error = %e, "complete_multipart_upload failed, aborting");
            // Abort the multipart upload to clean up orphaned parts
            let _ = self.client
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(&key)
                .upload_id(&upload_id)
                .send()
                .await;
            return Err(anyhow::anyhow!("complete_multipart_upload failed: {e}"));
        }

        info!(
            bucket = %self.bucket,
            key = %key,
            size = total_bytes,
            "filestore streaming upload complete"
        );

        // Generate presigned GET URL
        let presigning_config =
            aws_sdk_s3::presigning::PresigningConfig::expires_in(self.presigned_ttl)
                .map_err(|e| anyhow::anyhow!("presigning config error: {e}"))?;

        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .presigned(presigning_config)
            .await
            .map_err(|e| {
                error!(bucket = %self.bucket, key = %key, error = %e, "presigned URL generation failed");
                anyhow::anyhow!("presigned URL generation failed: {e}")
            })?;

        Ok((presigned.uri().to_string(), total_bytes))
    }

    /// Return the configured presigned TTL in seconds.
    pub fn presigned_ttl_secs(&self) -> u64 {
        self.presigned_ttl.as_secs()
    }

    /// Return the configured maximum file size in bytes.
    pub fn max_file_size(&self) -> u64 {
        self.max_file_size
    }
}

/// Format the hint block returned to the agent when a large file is uploaded
/// to the filestore instead of being inlined.
pub fn format_filestore_hint(filename: &str, size_bytes: u64, presigned_url: &str, ttl_secs: u64) -> String {
    let size_kb = size_bytes / 1024;
    let lifetime = format_presigned_lifetime(ttl_secs);
    // Sanitize filename for prompt safety — strip control characters
    let safe_filename: String = filename.chars().filter(|c| !c.is_control()).take(200).collect();
    format!(
        "[File: {safe_filename}]\n\
         This file ({size_kb} KB) exceeds the 512 KB inline limit. \
         It has been uploaded to temporary storage. \
         Fetch the contents using the URL below:\n\
         {presigned_url}\n\
         Note: this URL expires in {lifetime}."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compatibility_wrapper_still_stores_what_it_always_did() {
        // External callers of the two-argument `upload_and_presign` get this value,
        // so it is the API contract, not an implementation detail.
        assert_eq!(TEXT_CONTENT_TYPE, "text/plain; charset=utf-8");
    }

    #[test]
    fn presigned_ttl_is_capped_but_never_raised() {
        // The value is an authorization lifetime, so only the upper bound may move it.
        for configured in [0, 1, 59, 60, 3600, MAX_PRESIGNED_TTL] {
            assert_eq!(cap_presigned_ttl(configured), configured, "{configured}");
        }
        assert_eq!(cap_presigned_ttl(MAX_PRESIGNED_TTL + 1), MAX_PRESIGNED_TTL);
    }

    #[test]
    fn a_sub_minute_lifetime_renders_in_seconds_not_as_zero_minutes() {
        for (secs, expected) in [
            (0, "0 seconds"),
            (1, "1 second"),
            (30, "30 seconds"),
            (59, "59 seconds"),
        ] {
            assert_eq!(format_presigned_lifetime(secs), expected);
        }
    }

    #[test]
    fn a_minute_or_more_renders_exactly_as_it_did_before() {
        // Pins the #738 hint: these are the `ttl / 60` values main produced.
        for (secs, expected) in [
            (60, "1 minutes"),
            (900, "15 minutes"),
            (3600, "60 minutes"),
            (MAX_PRESIGNED_TTL, "10080 minutes"),
        ] {
            assert_eq!(format_presigned_lifetime(secs), expected);
        }
    }

    #[test]
    fn filestore_config_deserializes_with_defaults() {
        let toml_str = r#"
bucket = "my-oab-files"
region = "us-west-2"
"#;
        let config: FilestoreConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.bucket, "my-oab-files");
        assert_eq!(config.region, "us-west-2");
        assert!(config.endpoint.is_none());
        assert_eq!(config.prefix, "incoming/");
        assert_eq!(config.presigned_ttl, 3600);
        assert!(config.access_key_id.is_none());
        assert!(config.secret_access_key.is_none());
    }

    #[test]
    fn filestore_config_deserializes_full() {
        let toml_str = r#"
bucket = "my-bucket"
region = "eu-west-1"
endpoint = "https://abc123.r2.cloudflarestorage.com"
prefix = "uploads/"
presigned_ttl = 7200
access_key_id = "AKID"
secret_access_key = "SECRET"
"#;
        let config: FilestoreConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.bucket, "my-bucket");
        assert_eq!(config.region, "eu-west-1");
        assert_eq!(
            config.endpoint.as_deref(),
            Some("https://abc123.r2.cloudflarestorage.com")
        );
        assert_eq!(config.prefix, "uploads/");
        assert_eq!(config.presigned_ttl, 7200);
        assert_eq!(config.access_key_id.as_deref(), Some("AKID"));
        assert_eq!(config.secret_access_key.as_deref(), Some("SECRET"));
    }

    #[test]
    fn format_filestore_hint_produces_expected_output() {
        let hint = format_filestore_hint(
            "big-log.txt",
            1_048_576, // 1 MB
            "https://bucket.s3.amazonaws.com/incoming/uuid_big-log.txt?X-Amz-...",
            3600,
        );
        assert!(hint.contains("[File: big-log.txt]"));
        assert!(hint.contains("1024 KB"));
        assert!(hint.contains("exceeds the 512 KB inline limit"));
        assert!(hint.contains("https://bucket.s3.amazonaws.com/incoming/uuid_big-log.txt?X-Amz-..."));
        assert!(hint.contains("expires in 60 minutes"));
    }

    #[test]
    fn format_filestore_hint_short_ttl() {
        let hint = format_filestore_hint("data.csv", 600_000, "https://example.com/file", 900);
        assert!(hint.contains("585 KB"));
        assert!(hint.contains("expires in 15 minutes"));
    }
}
