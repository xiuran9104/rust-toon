use base64::Engine;
use chrono::Utc;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::{Method, Url, header};
use rust_toon_framework_resilience::{HttpResilienceConfig, ResilientHttpClient};
use rust_toon_framework_web::AppError;
use sha2::{Digest, Sha256};
use std::{
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Response, StatusCode, header as http_header},
};
use rust_toon_framework_security::CurrentUser;

use crate::{ToonState, shared::require, toonflow_episode_renders::ensure_project_access};

type HmacSha256 = Hmac<Sha256>;

fn timeout_from_env(name: &str, default: u64, minimum: u64, maximum: u64) -> Duration {
    Duration::from_secs(
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| (*value >= minimum) && (*value <= maximum))
            .unwrap_or(default),
    )
}

fn s3_connect_timeout() -> Duration {
    timeout_from_env("S3_CONNECT_TIMEOUT_SECONDS", 10, 1, 300)
}

fn s3_request_timeout() -> Duration {
    timeout_from_env("S3_REQUEST_TIMEOUT_SECONDS", 30, 1, 3_600)
}

fn s3_stream_timeout() -> Duration {
    timeout_from_env("S3_STREAM_TIMEOUT_SECONDS", 1_800, 30, 86_400)
}

struct S3Config {
    endpoint: String,
    access_key: String,
    secret_key: String,
    bucket: String,
    region: String,
}

fn config() -> S3Config {
    S3Config {
        endpoint: std::env::var("S3_ENDPOINT")
            .unwrap_or_else(|_| "http://127.0.0.1:9000".to_string())
            .trim_end_matches('/')
            .to_string(),
        access_key: std::env::var("S3_ACCESS_KEY").unwrap_or_else(|_| "rust_toon".to_string()),
        secret_key: std::env::var("S3_SECRET_KEY")
            .unwrap_or_else(|_| "rust_toon_password".to_string()),
        bucket: std::env::var("S3_BUCKET").unwrap_or_else(|_| "rust-toon".to_string()),
        region: std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hmac(key: &[u8], data: &str) -> Result<Vec<u8>, String> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|error| error.to_string())?;
    mac.update(data.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

async fn signed_request(
    method: Method,
    object_key: Option<&str>,
    body: Vec<u8>,
) -> Result<reqwest::Response, String> {
    signed_request_with_range(method, object_key, body, None).await
}

async fn signed_request_with_range(
    method: Method,
    object_key: Option<&str>,
    body: Vec<u8>,
    range: Option<&str>,
) -> Result<reqwest::Response, String> {
    let payload_hash = sha256_hex(&body);
    let timeout = if method == Method::GET {
        s3_stream_timeout()
    } else {
        s3_request_timeout()
    };
    signed_request_builder_with_timeout(method, object_key, &payload_hash, range, timeout)?
        .body(body)
        .pipe_execute(object_storage_client())
        .await
        .map_err(|error| error.to_string())
}

trait ResilientRequestExt {
    async fn pipe_execute(
        self,
        client: &ResilientHttpClient,
    ) -> Result<reqwest::Response, rust_toon_framework_resilience::ResilienceError>;
}

impl ResilientRequestExt for reqwest::RequestBuilder {
    async fn pipe_execute(
        self,
        client: &ResilientHttpClient,
    ) -> Result<reqwest::Response, rust_toon_framework_resilience::ResilienceError> {
        client.execute(self).await
    }
}

fn object_storage_client() -> &'static ResilientHttpClient {
    static CLIENT: OnceLock<ResilientHttpClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let client = reqwest::Client::builder()
            .connect_timeout(s3_connect_timeout())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        ResilientHttpClient::new(
            "object-storage",
            client,
            HttpResilienceConfig {
                timeout: s3_stream_timeout(),
                max_attempts: 3,
                max_concurrent_calls: 32,
                ..HttpResilienceConfig::default()
            },
        )
        .expect("static object storage resilience configuration must be valid")
    })
}

fn remote_media_client() -> &'static ResilientHttpClient {
    static CLIENT: OnceLock<ResilientHttpClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let client = reqwest::Client::builder()
            .connect_timeout(s3_connect_timeout())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        ResilientHttpClient::new(
            "media-download",
            client,
            HttpResilienceConfig {
                timeout: s3_stream_timeout(),
                max_attempts: 3,
                max_concurrent_calls: 16,
                ..HttpResilienceConfig::default()
            },
        )
        .expect("static media download resilience configuration must be valid")
    })
}

fn signed_request_builder_with_timeout(
    method: Method,
    object_key: Option<&str>,
    payload_hash: &str,
    range: Option<&str>,
    timeout: Duration,
) -> Result<reqwest::RequestBuilder, String> {
    let config = config();
    let canonical_uri = match object_key {
        Some(key) => format!("/{}/{}", config.bucket, key.trim_start_matches('/')),
        None => format!("/{}", config.bucket),
    };
    let url = Url::parse(&format!("{}{}", config.endpoint, canonical_uri))
        .map_err(|error| error.to_string())?;
    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_string(),
    };
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n");
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";
    let canonical_request = format!(
        "{}\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        method.as_str(),
    );
    let scope = format!("{date}/{}/s3/aws4_request", config.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes()),
    );
    let date_key = hmac(format!("AWS4{}", config.secret_key).as_bytes(), &date)?;
    let region_key = hmac(&date_key, &config.region)?;
    let service_key = hmac(&region_key, "s3")?;
    let signing_key = hmac(&service_key, "aws4_request")?;
    let signature = hex::encode(hmac(&signing_key, &string_to_sign)?);
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        config.access_key,
    );
    let client = reqwest::Client::builder()
        .connect_timeout(s3_connect_timeout())
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client
        .request(method, url)
        .timeout(timeout)
        .header(header::HOST, host)
        .header("x-amz-content-sha256", payload_hash)
        .header("x-amz-date", amz_date)
        .header(header::AUTHORIZATION, authorization);
    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }
    Ok(request)
}

async fn ensure_bucket() -> Result<(), String> {
    let head = signed_request(Method::HEAD, None, Vec::new()).await?;
    if head.status().is_success() {
        return Ok(());
    }
    if head.status().as_u16() != 404 {
        return Err(format!("访问对象存储 bucket 失败：HTTP {}", head.status()));
    }
    let response = signed_request(Method::PUT, None, Vec::new()).await?;
    if response.status().is_success() || response.status().as_u16() == 409 {
        return Ok(());
    }
    Err(format!(
        "创建对象存储 bucket 失败：HTTP {}",
        response.status()
    ))
}

pub(crate) async fn check_bucket_readiness() -> Result<(), String> {
    let response = signed_request(Method::HEAD, None, Vec::new()).await?;
    if response.status().is_success() {
        return Ok(());
    }
    Err(format!(
        "对象存储 bucket 不可访问：HTTP {}",
        response.status()
    ))
}

pub(crate) async fn initialize_bucket() -> Result<(), String> {
    ensure_bucket().await
}

pub async fn persist_remote_image(url: &str, asset_id: i64) -> Result<String, String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Ok(url.to_string());
    }
    let (extension, bytes) = download_remote_image(url).await?;
    ensure_bucket().await?;
    let key = format!(
        "toonflow/assets/{asset_id}/{}.{}",
        uuid::Uuid::new_v4(),
        extension
    );
    let upload = signed_request(Method::PUT, Some(&key), bytes).await?;
    if !upload.status().is_success() {
        return Err(format!(
            "上传生成图片到对象存储失败：HTTP {}",
            upload.status()
        ));
    }
    Ok(format!("/toonflow/assets/files/{key}"))
}

pub(crate) async fn persist_remote_project_image(
    url: &str,
    project_id: i64,
    category: &str,
    object_name: &str,
) -> Result<String, String> {
    if let Some(key) = asset_image_key(url) {
        if key.starts_with(&format!("toonflow/{project_id}/assets/")) {
            return Ok(url.to_string());
        }
        return Err("生成图片返回了其他项目的对象路径".to_string());
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("生成图片返回了不受支持的非 HTTP 地址".to_string());
    }
    let (extension, bytes) = download_remote_image(url).await?;
    persist_asset_bytes_named(project_id, category, object_name, &extension, bytes).await
}

const MAX_GENERATED_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// Validate actual decoded pixels, never the provider's declared MIME or size.
/// This checks geometry and file integrity, not semantic anatomy/composition.
fn validate_generated_image_bytes(
    bytes: &[u8],
    canvas: crate::toonflow_image_contract::ImageCanvas,
    role_sheet: bool,
) -> Result<String, String> {
    let format = image::guess_format(bytes).map_err(|_| "生成结果不是可识别的图片".to_string())?;
    let extension = match format {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::WebP => "webp",
        _ => return Err("生成图片仅支持 PNG、JPEG 或 WebP".into()),
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| format!("生成图片损坏或超出解码限制：{error}"))?;
    canvas.validate_dimensions(image.width(), image.height())?;
    if role_sheet {
        crate::toonflow_image_contract::validate_role_margins(&image)?;
    }
    Ok(extension.into())
}

pub(crate) async fn validate_and_persist_generated_image(
    url: &str,
    project_id: Option<i64>,
    canvas: crate::toonflow_image_contract::ImageCanvas,
    role_sheet: bool,
) -> Result<String, String> {
    let bytes = if url.starts_with("http://") || url.starts_with("https://") {
        download_remote_image(url).await?.1
    } else if let Some((metadata, data)) = url.strip_prefix("data:").and_then(|v| v.split_once(','))
    {
        if !metadata.starts_with("image/")
            || !metadata.ends_with(";base64")
            || data.len() > MAX_GENERATED_IMAGE_BYTES * 4 / 3 + 4
        {
            return Err("生成图片 Base64 类型或长度不合法".into());
        }
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| "生成图片 Base64 无效".to_string())?
    } else if let Some(key) = asset_image_key(url) {
        let project_id = project_id.ok_or_else(|| "读取生成图片需要项目上下文".to_string())?;
        if !key.starts_with(&format!("toonflow/{project_id}/assets/")) {
            return Err("生成图片返回了其他项目的对象路径".into());
        }
        read_asset_bytes(url).await?
    } else {
        return Err("生成图片返回了不受支持的地址".into());
    };
    if bytes.len() > MAX_GENERATED_IMAGE_BYTES {
        return Err("生成图片超过 64 MiB 限制".into());
    }
    let (extension, bytes) = tokio::task::spawn_blocking(move || {
        validate_generated_image_bytes(&bytes, canvas, role_sheet)
            .map(|extension| (extension, bytes))
    })
    .await
    .map_err(|error| format!("生成图片检查失败：{error}"))??;
    if let Some(project_id) = project_id {
        if asset_image_key(url).is_some() {
            return Ok(url.to_string());
        }
        persist_asset_bytes(project_id, "generated-images", &extension, bytes).await
    } else {
        Ok(url.to_string())
    }
}

async fn download_remote_image(url: &str) -> Result<(String, Vec<u8>), String> {
    let response = remote_media_client()
        .execute(
            reqwest::Client::builder()
                .connect_timeout(s3_connect_timeout())
                .timeout(s3_stream_timeout())
                .build()
                .map_err(|error| format!("创建图片下载客户端失败：{error}"))?
                .get(url),
        )
        .await
        .map_err(|error| format!("下载生成图片失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("下载生成图片失败：HTTP {}", response.status()));
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_GENERATED_IMAGE_BYTES as u64)
    {
        return Err("生成图片超过 64 MiB 限制".into());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("下载生成图片失败：{error}"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_GENERATED_IMAGE_BYTES {
            return Err("生成图片超过 64 MiB 限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let extension = if content_type.contains("png") {
        "png"
    } else if content_type.contains("webp") {
        "webp"
    } else {
        "jpg"
    };
    Ok((extension.to_string(), bytes))
}

fn existing_project_video_path(url: &str, project_id: i64) -> Result<Option<String>, String> {
    if let Some(key) = asset_image_key(url) {
        if key.starts_with(&format!("toonflow/{project_id}/assets/")) {
            return Ok(Some(url.to_string()));
        }
        return Err("生成视频返回了其他项目的对象路径".to_string());
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("生成视频返回了不受支持的非 HTTP 地址".to_string());
    }
    Ok(None)
}

#[derive(Default)]
struct ProviderTempQuota {
    reserved_bytes: u64,
}

static PROVIDER_TEMP_QUOTA: OnceLock<Mutex<ProviderTempQuota>> = OnceLock::new();

struct ProviderTempReservation {
    bytes: u64,
}

impl Drop for ProviderTempReservation {
    fn drop(&mut self) {
        if let Some(quota) = PROVIDER_TEMP_QUOTA.get()
            && let Ok(mut quota) = quota.lock()
        {
            quota.reserved_bytes = quota.reserved_bytes.saturating_sub(self.bytes);
        }
    }
}

fn provider_video_max_bytes() -> u64 {
    std::env::var("TOON_PROVIDER_VIDEO_MAX_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (1024 * 1024..=100 * 1024 * 1024 * 1024).contains(value))
        .unwrap_or(2 * 1024 * 1024 * 1024)
}

fn provider_video_temp_quota_bytes() -> u64 {
    std::env::var("TOON_PROVIDER_VIDEO_TEMP_QUOTA_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (1024 * 1024..=500 * 1024 * 1024 * 1024).contains(value))
        .unwrap_or(4 * 1024 * 1024 * 1024)
}

fn reserve_provider_temp(bytes: u64) -> Result<ProviderTempReservation, String> {
    let quota = PROVIDER_TEMP_QUOTA.get_or_init(|| Mutex::new(ProviderTempQuota::default()));
    let mut quota = quota
        .lock()
        .map_err(|_| "生成视频临时空间配额锁不可用".to_string())?;
    let limit = provider_video_temp_quota_bytes();
    let next = quota
        .reserved_bytes
        .checked_add(bytes)
        .ok_or_else(|| "生成视频临时空间配额溢出".to_string())?;
    if next > limit {
        return Err(format!(
            "生成视频临时空间不足：已预留 {} 字节，单任务上限 {} 字节，总配额 {} 字节",
            quota.reserved_bytes, bytes, limit
        ));
    }
    quota.reserved_bytes = next;
    Ok(ProviderTempReservation { bytes })
}

fn provider_video_temp_directory() -> std::path::PathBuf {
    std::env::temp_dir().join("rust-toon/provider-videos")
}

/// Gateway startup owns this directory, so every `.download` file is residue
/// from an interrupted previous process and can be removed before accepting traffic.
pub async fn cleanup_provider_video_temp_on_startup() -> Result<u64, String> {
    let directory = provider_video_temp_directory();
    let mut entries = match tokio::fs::read_dir(&directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("读取生成视频临时目录失败：{error}")),
    };
    let mut removed = 0;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("遍历生成视频临时目录失败：{error}"))?
    {
        let path = entry.path();
        let is_download = path.extension().and_then(|value| value.to_str()) == Some("download");
        if is_download
            && entry
                .file_type()
                .await
                .map_err(|error| format!("读取生成视频临时文件类型失败：{error}"))?
                .is_file()
        {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|error| format!("清理生成视频临时文件失败：{error}"))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Reserve the final object path in PostgreSQL before any object-storage write. The
/// delayed cleanup row is an outbox: a crash after PUT but before the video
/// row update still leaves enough durable information to delete the orphan.
pub async fn persist_remote_video_for_row(
    pool: &sqlx::PgPool,
    url: &str,
    project_id: i64,
    video_id: i64,
) -> Result<String, String> {
    if let Some(existing) = existing_project_video_path(url, project_id)? {
        return Ok(existing);
    }

    let object_name = format!("video-{video_id}-{}", uuid::Uuid::new_v4());
    let file_path = asset_file_path_named(project_id, "videos", &object_name, "mp4")?;
    let cleanup_delay = crate::toonflow_video_export::source_download_timeout()
        .as_secs()
        .saturating_add(s3_stream_timeout().as_secs())
        .saturating_add(3_600)
        .min(i64::MAX as u64) as i64;
    sqlx::query(
        "INSERT INTO toonflow.storage_cleanup_tasks(
           object_path,resource_type,resource_id,error_reason,attempts,state,
           next_attempt_at,create_time,update_time
         ) VALUES(
           $1,'generated_video_reservation',$2,
           '生成视频对象预留：成功落库后由引用检查安全跳过，否则清理孤儿对象',
           0,'pending',now()+make_interval(secs => $3::double precision),
           (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
           (extract(epoch FROM clock_timestamp()) * 1000)::bigint
         )",
    )
    .bind(&file_path)
    .bind(video_id)
    .bind(cleanup_delay)
    .execute(pool)
    .await
    .map_err(|error| format!("预留生成视频对象清理记录失败：{error}"))?;

    struct TemporaryVideo(std::path::PathBuf);
    impl Drop for TemporaryVideo {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    let max_bytes = provider_video_max_bytes();
    let _quota = reserve_provider_temp(max_bytes)?;
    let directory = provider_video_temp_directory();
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| format!("创建生成视频临时目录失败：{error}"))?;
    let temporary = TemporaryVideo(directory.join(format!("{object_name}.download")));
    crate::toonflow_video_export::download_external_video(url, &temporary.0, max_bytes).await?;
    persist_asset_file_named(project_id, "videos", &object_name, "mp4", &temporary.0).await
}

pub async fn persist_asset_bytes(
    project_id: i64,
    category: &str,
    extension: &str,
    bytes: Vec<u8>,
) -> Result<String, String> {
    persist_asset_bytes_named(
        project_id,
        category,
        &uuid::Uuid::new_v4().to_string(),
        extension,
        bytes,
    )
    .await
}

/// Reserve a continuity-frame path before the object-storage PUT. The delayed cleanup
/// record survives a process crash between object upload and the cache-row
/// upsert; once referenced, the normal reference check safely keeps it.
pub(crate) async fn persist_continuity_frame_with_reservation(
    pool: &sqlx::PgPool,
    project_id: i64,
    previous_video_id: i64,
    bytes: Vec<u8>,
) -> Result<String, String> {
    let object_name = format!("frame-{previous_video_id}-{}", uuid::Uuid::new_v4());
    let file_path = asset_file_path_named(project_id, "continuity-frames", &object_name, "png")?;
    let cleanup_delay = s3_request_timeout()
        .as_secs()
        .saturating_add(3_600)
        .min(i64::MAX as u64) as i64;
    sqlx::query(
        "INSERT INTO toonflow.storage_cleanup_tasks(
           object_path,resource_type,resource_id,error_reason,attempts,state,
           next_attempt_at,create_time,update_time
         ) VALUES(
           $1,'continuity_frame_reservation',$2,
           '连续帧对象预留：成功落库后由引用检查安全跳过，否则清理孤儿对象',
           0,'pending',now()+make_interval(secs => $3::double precision),
           (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
           (extract(epoch FROM clock_timestamp()) * 1000)::bigint
         )",
    )
    .bind(&file_path)
    .bind(previous_video_id)
    .bind(cleanup_delay)
    .execute(pool)
    .await
    .map_err(|error| format!("预留连续帧对象清理记录失败：{error}"))?;
    persist_asset_bytes_named(project_id, "continuity-frames", &object_name, "png", bytes).await
}

/// Store an in-memory asset at an explicitly named object key.
pub(crate) async fn persist_asset_bytes_named(
    project_id: i64,
    category: &str,
    object_name: &str,
    extension: &str,
    bytes: Vec<u8>,
) -> Result<String, String> {
    ensure_bucket().await?;
    let file_path = asset_file_path_named(project_id, category, object_name, extension)?;
    let key = asset_image_key(&file_path).ok_or_else(|| "无法生成资产对象路径".to_string())?;
    let upload = signed_request(Method::PUT, Some(key), bytes).await?;
    if !upload.status().is_success() {
        return Err(format!("上传资产到对象存储失败：HTTP {}", upload.status()));
    }
    Ok(file_path)
}

pub(crate) fn asset_file_path_named(
    project_id: i64,
    category: &str,
    object_name: &str,
    extension: &str,
) -> Result<String, String> {
    let safe_category = category
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .collect::<String>();
    let safe_object_name = object_name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .collect::<String>();
    let safe_extension = extension
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    if safe_category.is_empty() || safe_object_name.is_empty() {
        return Err("资产类别和对象名不能为空".into());
    }
    let key = format!(
        "toonflow/{project_id}/assets/{safe_category}/{safe_object_name}.{}",
        if safe_extension.is_empty() {
            "bin"
        } else {
            &safe_extension
        }
    );
    Ok(format!("/toonflow/assets/files/{key}"))
}

/// Stream a local file into object storage without buffering an entire video
/// in the worker heap. The content hash is calculated in a bounded buffer and
/// is then used for the normal SigV4 payload signature.
pub(crate) async fn persist_asset_file_named(
    project_id: i64,
    category: &str,
    object_name: &str,
    extension: &str,
    source: &std::path::Path,
) -> Result<String, String> {
    ensure_bucket().await?;
    let file_path = asset_file_path_named(project_id, category, object_name, extension)?;
    let key = asset_image_key(&file_path).ok_or_else(|| "无法生成资产对象路径".to_string())?;
    let metadata = tokio::fs::metadata(source)
        .await
        .map_err(|error| format!("读取待上传资产失败：{error}"))?;
    let mut hash_file = tokio::fs::File::open(source)
        .await
        .map_err(|error| format!("打开待上传资产失败：{error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = hash_file
            .read(&mut buffer)
            .await
            .map_err(|error| format!("计算资产摘要失败：{error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let payload_hash = hex::encode(hasher.finalize());
    let upload_file = tokio::fs::File::open(source)
        .await
        .map_err(|error| format!("重新打开待上传资产失败：{error}"))?;
    let body = reqwest::Body::wrap_stream(ReaderStream::new(upload_file));
    let upload = signed_request_builder_with_timeout(
        Method::PUT,
        Some(key),
        &payload_hash,
        None,
        s3_stream_timeout(),
    )?
    .header(header::CONTENT_LENGTH, metadata.len())
    .body(body)
    .pipe_execute(object_storage_client())
    .await
    .map_err(|error| format!("上传资产到对象存储失败：{error}"))?;
    if !upload.status().is_success() {
        return Err(format!("上传资产到对象存储失败：HTTP {}", upload.status()));
    }
    Ok(file_path)
}

async fn read_image(key: &str) -> Result<(String, Vec<u8>), String> {
    let response = signed_request(Method::GET, Some(key), Vec::new()).await?;
    if !response.status().is_success() {
        return Err(format!("读取对象存储图片失败：HTTP {}", response.status()));
    }
    let mut content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    if is_generic_binary_content_type(&content_type) {
        content_type = stored_content_type_for_key(key).to_string();
    }
    let bytes = response.bytes().await.map_err(|error| error.to_string())?;
    Ok((content_type, bytes.to_vec()))
}

fn stored_content_type_for_key(key: &str) -> &'static str {
    match key
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp3" | "mpeg" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "aiff" => "audio/aiff",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        _ => "application/octet-stream",
    }
}

fn is_generic_binary_content_type(content_type: &str) -> bool {
    content_type.split(';').next().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "application/octet-stream" | "binary/octet-stream"
        )
    })
}

pub async fn image_data_url(file_path: &str) -> Result<String, String> {
    let key = asset_image_key(file_path).ok_or_else(|| "不支持的资产图片路径".to_string())?;
    let (stored_content_type, bytes) = read_image(key).await?;
    let content_type = image_content_type(&bytes).ok_or_else(|| {
        format!(
            "资产参考图不是有效的 PNG、JPEG、GIF 或 WebP 图片（存储类型：{stored_content_type}）"
        )
    })?;
    Ok(format!(
        "data:{content_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

pub(crate) fn is_asset_image_path(file_path: &str) -> bool {
    asset_image_key(file_path).is_some()
}

pub(crate) async fn delete_asset_file(file_path: &str) -> Result<(), String> {
    let Some(key) = asset_image_key(file_path) else {
        return Ok(());
    };
    let response = signed_request(Method::DELETE, Some(key), Vec::new()).await?;
    if response.status().is_success() || response.status().as_u16() == 404 {
        Ok(())
    } else {
        Err(format!(
            "删除对象存储资产文件失败：HTTP {}",
            response.status()
        ))
    }
}

pub(crate) async fn record_cleanup_failure(
    pool: &sqlx::PgPool,
    object_path: &str,
    resource_type: &str,
    resource_id: Option<i64>,
    error: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO toonflow.storage_cleanup_tasks(object_path,resource_type,resource_id,error_reason,attempts,state,create_time,update_time)
         VALUES(
           $1,$2,$3,$4,1,'pending',
           (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
           (extract(epoch FROM clock_timestamp()) * 1000)::bigint
         )",
    )
    .bind(object_path)
    .bind(resource_type)
    .bind(resource_id)
    .bind(error)
    .execute(pool)
    .await;
}

pub(crate) async fn enqueue_cleanup_paths(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    paths: &[String],
    resource_type: &str,
    resource_id: Option<i64>,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    if paths.is_empty() {
        return Ok(0);
    }
    sqlx::query(
        "INSERT INTO toonflow.storage_cleanup_tasks(
           object_path,resource_type,resource_id,error_reason,attempts,state,create_time,update_time
         )
         SELECT DISTINCT path,$2,$3,$4,0,'pending',
           (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
           (extract(epoch FROM clock_timestamp()) * 1000)::bigint
         FROM unnest($1::text[]) AS path
         WHERE btrim(path)<>''",
    )
    .bind(paths)
    .bind(resource_type)
    .bind(resource_id)
    .bind(reason)
    .execute(&mut **tx)
    .await
    .map(|result| result.rows_affected())
}

fn asset_image_key(file_path: &str) -> Option<&str> {
    file_path
        .split(['?', '#'])
        .next()
        .unwrap_or(file_path)
        .strip_prefix("/toonflow/assets/files/")
        .or_else(|| file_path.strip_prefix("/api/toonflow/assets/files/"))
        .filter(|key| !key.is_empty())
}

pub(crate) fn asset_object_key(file_path: &str) -> Option<&str> {
    asset_image_key(file_path)
}

/// Check that a persisted Toonflow asset is readable from the configured object store.
///
/// This is intentionally shared with the production pipeline verification so tests validate
/// the same S3-backed storage path used in production instead of assuming local `/upload`
/// files.
#[cfg(test)]
pub(crate) async fn asset_exists(file_path: &str) -> Result<bool, String> {
    let Some(key) = asset_image_key(file_path) else {
        return Ok(false);
    };
    let response = signed_request(Method::GET, Some(key), Vec::new()).await?;
    Ok(response.status().is_success())
}

pub(crate) async fn read_asset_bytes(file_path: &str) -> Result<Vec<u8>, String> {
    let Some(key) = asset_image_key(file_path) else {
        return Err("不支持的对象存储资产路径".into());
    };
    let response = signed_request(Method::GET, Some(key), Vec::new()).await?;
    if !response.status().is_success() {
        return Err(format!("读取对象存储资产失败：HTTP {}", response.status()));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| error.to_string())
}

pub(crate) async fn copy_asset_to_file(
    file_path: &str,
    destination: &std::path::Path,
    max_bytes: u64,
) -> Result<u64, String> {
    let Some(key) = asset_image_key(file_path) else {
        return Err("不支持的对象存储资产路径".into());
    };
    let response = signed_request(Method::GET, Some(key), Vec::new()).await?;
    if !response.status().is_success() {
        return Err(format!("读取对象存储资产失败：HTTP {}", response.status()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err(format!("源视频超过大小限制（最大 {max_bytes} 字节）"));
    }
    let mut destination_file = tokio::fs::File::create(destination)
        .await
        .map_err(|error| format!("创建源视频临时文件失败：{error}"))?;
    let mut total = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取对象存储视频流失败：{error}"))?;
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| "源视频大小溢出".to_string())?;
        if total > max_bytes {
            return Err(format!("源视频超过大小限制（最大 {max_bytes} 字节）"));
        }
        destination_file
            .write_all(&chunk)
            .await
            .map_err(|error| format!("写入源视频临时文件失败：{error}"))?;
    }
    destination_file
        .flush()
        .await
        .map_err(|error| format!("刷新源视频临时文件失败：{error}"))?;
    Ok(total)
}

fn image_content_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

async fn authorize_asset_key(
    state: &ToonState,
    user: &CurrentUser,
    key: &str,
) -> Result<(), AppError> {
    let segments = key.split('/').collect::<Vec<_>>();
    let project_id = if let ["toonflow", project_id, "assets", ..] = segments.as_slice() {
        project_id
            .parse::<i64>()
            .map_err(|_| AppError::not_found("asset file not found"))?
    } else if let ["toonflow", "assets", asset_id, ..] = segments.as_slice() {
        let asset_id = asset_id
            .parse::<i64>()
            .map_err(|_| AppError::not_found("asset file not found"))?;
        sqlx::query_scalar::<_, i64>("SELECT project_id FROM toonflow.assets WHERE id=$1")
            .bind(asset_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| {
                tracing::error!(asset_id, %error, "failed to authorize asset file");
                AppError::internal("failed to authorize asset file")
            })?
            .ok_or_else(|| AppError::not_found("asset file not found"))?
    } else {
        return Err(AppError::not_found("asset file not found"));
    };
    ensure_project_access(&state.pool, user, project_id).await?;
    Ok(())
}

pub async fn serve_image(
    user: CurrentUser,
    State(state): State<ToonState>,
    Path(key): Path<String>,
    request_headers: HeaderMap,
) -> Result<Response<Body>, AppError> {
    require(&user, "toon:project:read")?;
    authorize_asset_key(&state, &user, &key).await?;
    let range = request_headers
        .get(http_header::RANGE)
        .and_then(|value| value.to_str().ok());
    let upstream = signed_request_with_range(Method::GET, Some(&key), Vec::new(), range)
        .await
        .map_err(|_| AppError::not_found("asset file not found"))?;
    if !upstream.status().is_success() {
        return Err(AppError::not_found("asset file not found"));
    }
    let status = if upstream.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let upstream_headers = upstream.headers().clone();
    let content_type = upstream_headers
        .get(http_header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !is_generic_binary_content_type(value))
        .map(str::to_string)
        .unwrap_or_else(|| stored_content_type_for_key(&key).to_string());
    let mut response = Response::builder()
        .status(status)
        .body(Body::from_stream(upstream.bytes_stream()))
        .map_err(|_| AppError::internal("failed to build asset response"))?;
    response.headers_mut().insert(
        http_header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .map_err(|_| AppError::internal("invalid asset content type"))?,
    );
    response.headers_mut().insert(
        http_header::ACCEPT_RANGES,
        HeaderValue::from_static("bytes"),
    );
    if let Some(content_length) = upstream_headers.get(http_header::CONTENT_LENGTH) {
        response
            .headers_mut()
            .insert(http_header::CONTENT_LENGTH, content_length.clone());
    }
    response.headers_mut().insert(
        http_header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=300"),
    );
    response.headers_mut().insert(
        http_header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        http_header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(content_range) = upstream_headers.get(http_header::CONTENT_RANGE) {
        response
            .headers_mut()
            .insert(http_header::CONTENT_RANGE, content_range.clone());
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    #[test]
    fn validates_pixels_and_rejects_corrupt_provider_results() {
        use crate::toonflow_image_contract::ImageCanvas;
        let canvas = ImageCanvas::parse("192x128").unwrap();
        let png = |width, height| {
            let mut output = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(width, height)
                .write_to(&mut output, image::ImageFormat::Png)
                .unwrap();
            output.into_inner()
        };
        assert_eq!(
            super::validate_generated_image_bytes(&png(192, 128), canvas, false).unwrap(),
            "png"
        );
        assert!(
            super::validate_generated_image_bytes(&png(2048, 512), canvas, false)
                .unwrap_err()
                .contains("比例不合格")
        );
        assert!(
            super::validate_generated_image_bytes(b"<html>upstream failed</html>", canvas, false)
                .is_err()
        );
        let mut damaged = png(192, 128);
        damaged.truncate(40);
        assert!(super::validate_generated_image_bytes(&damaged, canvas, false).is_err());
    }

    #[tokio::test]
    async fn base64_provider_results_receive_the_same_validation() {
        use base64::Engine;
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(b"not an image")
        );
        let error = super::validate_and_persist_generated_image(
            &url,
            None,
            crate::toonflow_image_contract::ImageCanvas::parse("2560x1696").unwrap(),
            false,
        )
        .await
        .unwrap_err();
        assert!(error.contains("不是可识别的图片"));
    }
    use super::*;

    #[test]
    fn detects_reference_image_content_type_from_file_signature() {
        assert_eq!(
            image_content_type(b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(image_content_type(b"\xff\xd8\xffrest"), Some("image/jpeg"));
        assert_eq!(image_content_type(b"GIF89arest"), Some("image/gif"));
        assert_eq!(image_content_type(b"RIFF1234WEBPrest"), Some("image/webp"));
        assert_eq!(image_content_type(b"not-an-image"), None);
    }

    #[test]
    fn accepts_asset_paths_with_or_without_api_gateway_prefix() {
        let key = "toonflow/assets/1/reference.jpg";
        assert_eq!(
            asset_image_key(&format!("/toonflow/assets/files/{key}")),
            Some(key)
        );
        assert_eq!(
            asset_image_key(&format!("/api/toonflow/assets/files/{key}")),
            Some(key)
        );
        assert!(is_asset_image_path(&format!(
            "/api/toonflow/assets/files/{key}"
        )));
        assert_eq!(asset_image_key("/api/toonflow/assets/files/"), None);
    }

    #[test]
    fn named_export_paths_preserve_the_attempt_fencing_token() {
        let path = asset_file_path_named(
            7,
            "exports",
            "task-42-lease-8db4fda4-72e6-4e0c-9b66-067b46c284a4",
            "mp4",
        )
        .expect("valid attempt path");
        assert_eq!(
            path,
            "/toonflow/assets/files/toonflow/7/assets/exports/task-42-lease-8db4fda4-72e6-4e0c-9b66-067b46c284a4.mp4"
        );
    }

    #[test]
    fn detects_video_content_type_from_object_key() {
        assert_eq!(
            stored_content_type_for_key("toonflow/assets/1/video.mp4"),
            "video/mp4"
        );
        assert_eq!(
            stored_content_type_for_key("toonflow/assets/1/video.webm"),
            "video/webm"
        );
    }

    #[test]
    fn treats_s3_generic_binary_types_as_missing_metadata() {
        assert!(is_generic_binary_content_type("application/octet-stream"));
        assert!(is_generic_binary_content_type("binary/octet-stream"));
        assert!(is_generic_binary_content_type(
            "Binary/Octet-Stream; charset=binary"
        ));
        assert!(!is_generic_binary_content_type("image/jpeg"));
    }

    #[test]
    fn generated_video_paths_must_stay_inside_the_target_project() {
        let own_path = "/toonflow/assets/files/toonflow/7/assets/videos/generated.mp4";
        assert_eq!(
            existing_project_video_path(own_path, 7).expect("same-project object"),
            Some(own_path.to_string())
        );
        assert!(existing_project_video_path(own_path, 8).is_err());
        assert!(existing_project_video_path("/tmp/provider.mp4", 7).is_err());
    }

    #[tokio::test]
    #[ignore = "requires the local object storage service"]
    async fn s3_round_trip() {
        ensure_bucket().await.expect("create test bucket");
        let key = format!("toonflow/tests/{}.txt", uuid::Uuid::new_v4());
        let uploaded = signed_request(Method::PUT, Some(&key), b"rust-toon".to_vec())
            .await
            .expect("upload object");
        assert!(uploaded.status().is_success());
        let (_, bytes) = read_image(&key).await.expect("read object");
        assert_eq!(bytes, b"rust-toon");
    }
}
