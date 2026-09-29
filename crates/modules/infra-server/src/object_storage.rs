use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::{Method, Url, header};
use rust_toon_framework_resilience::{HttpResilienceConfig, ResilientHttpClient};
use sha2::{Digest, Sha256};
use std::{sync::OnceLock, time::Duration};

type HmacSha256 = Hmac<Sha256>;

fn value(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn hex_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hmac(key: &[u8], data: &str) -> Result<Vec<u8>, String> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|error| error.to_string())?;
    mac.update(data.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

async fn request(method: Method, key: &str, body: Vec<u8>) -> Result<reqwest::Response, String> {
    let endpoint = value("S3_ENDPOINT", "http://127.0.0.1:9000")
        .trim_end_matches('/')
        .to_owned();
    let access = value("S3_ACCESS_KEY", "rust_toon");
    let secret = value("S3_SECRET_KEY", "rust_toon_password");
    let bucket = value("S3_BUCKET", "rust-toon");
    let region = value("S3_REGION", "us-east-1");
    let uri = format!("/{bucket}/{}", key.trim_start_matches('/'));
    let url = Url::parse(&format!("{endpoint}{uri}")).map_err(|error| error.to_string())?;
    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_owned(),
    };
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let payload = hex_hash(&body);
    let headers = format!("host:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{amz_date}\n");
    let signed = "host;x-amz-content-sha256;x-amz-date";
    let canonical = format!(
        "{}\n{uri}\n\n{headers}\n{signed}\n{payload}",
        method.as_str()
    );
    let scope = format!("{date}/{region}/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_hash(canonical.as_bytes())
    );
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), &date)?;
    let k_region = hmac(&k_date, &region)?;
    let k_service = hmac(&k_region, "s3")?;
    let signing = hmac(&k_service, "aws4_request")?;
    let signature = hex::encode(hmac(&signing, &to_sign)?);
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={signature}"
    );
    storage_client()
        .execute(
            reqwest::Client::new()
                .request(method, url)
                .header(header::HOST, host)
                .header("x-amz-content-sha256", payload)
                .header("x-amz-date", amz_date)
                .header(header::AUTHORIZATION, authorization)
                .body(body),
        )
        .await
        .map_err(|error| error.to_string())
}

fn storage_client() -> &'static ResilientHttpClient {
    static CLIENT: OnceLock<ResilientHttpClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let timeout = Duration::from_secs(
            std::env::var("S3_REQUEST_TIMEOUT_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(30)
                .clamp(1, 3_600),
        );
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        ResilientHttpClient::new(
            "object-storage",
            client,
            HttpResilienceConfig {
                timeout,
                max_attempts: 3,
                max_concurrent_calls: 32,
                ..HttpResilienceConfig::default()
            },
        )
        .expect("static object storage resilience configuration must be valid")
    })
}

pub async fn put(key: &str, bytes: Vec<u8>) -> Result<(), String> {
    let bucket = request(Method::PUT, "", Vec::new()).await?;
    if !bucket.status().is_success() && bucket.status().as_u16() != 409 {
        return Err(format!("对象存储 bucket 初始化失败：HTTP {}", bucket.status()));
    }
    let response = request(Method::PUT, key, bytes).await?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("对象存储上传失败：HTTP {}", response.status()))
    }
}

pub async fn get(key: &str) -> Result<Vec<u8>, String> {
    let response = request(Method::GET, key, Vec::new()).await?;
    if !response.status().is_success() {
        return Err(format!("对象存储读取失败：HTTP {}", response.status()));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| error.to_string())
}

pub async fn get_bounded(key: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut response = request(Method::GET, key, Vec::new()).await?;
    if !response.status().is_success() {
        return Err("读取上传文件失败".into());
    }
    if response.content_length().is_some_and(|length| length > max_bytes as u64) {
        return Err("上传文件超过知识库读取上限".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "读取上传文件失败")? {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err("上传文件超过知识库读取上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
