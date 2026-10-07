//! s3ctl：仓库自带的极简 S3 兼容存储客户端，供备份与 e2e 脚本使用，
//! 取代对任何外部 S3 CLI（mc/aws）的依赖。只实现脚本实际用到的操作：
//! stat/make bucket、put/get/delete、list、双向 mirror。凭据走 S3_*
//! 环境变量或命令行参数，与部署配置同名。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

type HmacSha256 = Hmac<Sha256>;

struct Config {
    endpoint: String,
    access_key: String,
    secret_key: String,
    region: String,
    bucket: String,
}

impl Config {
    fn resolve(overrides: &[(String, String)], bucket_override: Option<&str>) -> Result<Self, String> {
        fn pick(
            overrides: &[(String, String)],
            bucket_override: Option<&str>,
            flag: &str,
            env_name: &str,
        ) -> Result<String, String> {
            if let Some(value) = overrides.iter().find(|(name, _)| name == flag) {
                return Ok(value.1.clone());
            }
            if flag == "bucket" {
                if let Some(bucket) = bucket_override {
                    return Ok(bucket.to_string());
                }
            }
            std::env::var(env_name).map_err(|_| format!("{env_name} is required"))
        }
        Ok(Self {
            endpoint: pick(overrides, bucket_override, "endpoint", "S3_ENDPOINT")?
                .trim_end_matches('/')
                .to_string(),
            access_key: pick(overrides, bucket_override, "access-key", "S3_ACCESS_KEY")?,
            secret_key: pick(overrides, bucket_override, "secret-key", "S3_SECRET_KEY")?,
            region: pick(overrides, bucket_override, "region", "S3_REGION")
                .unwrap_or_else(|_| "us-east-1".into()),
            bucket: pick(overrides, bucket_override, "bucket", "S3_BUCKET")?,
        })
    }
}

fn hmac(key: &[u8], data: &str) -> Result<Vec<u8>, String> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|error| error.to_string())?;
    mac.update(data.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn uri_encode(value: &str, encode_slash: bool) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            b'/' if !encode_slash => encoded.push('/'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// AWS Signature V4 over the exact request we are about to send. The signing
/// scheme mirrors `infra-server::object_storage` so both sides stay identical.
async fn signed_request(
    config: &Config,
    method: &reqwest::Method,
    key: Option<&str>,
    query: &[(String, String)],
    body: Vec<u8>,
) -> Result<reqwest::Response, String> {
    let path = match key {
        Some(key) => format!("/{}/{}", config.bucket, uri_encode(key, false)),
        None => format!("/{}", config.bucket),
    };
    let sorted_query = {
        let mut pairs = query.to_vec();
        pairs.sort();
        pairs
            .iter()
            .map(|(name, value)| format!("{}={}", uri_encode(name, true), uri_encode(value, true)))
            .collect::<Vec<_>>()
            .join("&")
    };
    let url = reqwest::Url::parse(&format!(
        "{}{}{}",
        config.endpoint,
        path,
        if sorted_query.is_empty() {
            String::new()
        } else {
            format!("?{sorted_query}")
        }
    ))
    .map_err(|error| error.to_string())?;
    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_owned(),
    };
    let now = chrono_like_now();
    let payload_hash = hex_hash(&body);
    let headers = format!(
        "host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{now}\n"
    );
    let signed = "host;x-amz-content-sha256;x-amz-date";
    let canonical = format!(
        "{}\n{path}\n{sorted_query}\n{headers}\n{signed}\n{payload_hash}",
        method.as_str(),
    );
    let (date, _) = now.split_once('T').ok_or("invalid signing date")?;
    let scope = format!("{date}/{}/s3/aws4_request", config.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{now}\n{scope}\n{}",
        hex_hash(canonical.as_bytes())
    );
    let k_date = hmac(format!("AWS4{}", config.secret_key).as_bytes(), date)?;
    let k_region = hmac(&k_date, &config.region)?;
    let k_service = hmac(&k_region, "s3")?;
    let signing = hmac(&k_service, "aws4_request")?;
    let signature = hex::encode(hmac(&signing, &to_sign)?);
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
        config.access_key
    );
    let client = reqwest::Client::new();
    let request = client
        .request(method.clone(), url)
        .header("host", &host)
        .header("x-amz-content-sha256", &payload_hash)
        .header("x-amz-date", &now)
        .header("authorization", authorization)
        .body(body)
        .build()
        .map_err(|error| error.to_string())?;
    client
        .execute(request)
        .await
        .map_err(|error| format!("request failed: {error}"))
}

/// UTC timestamp in SigV4 format without pulling chrono into this tool.
fn chrono_like_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0) as i64;
    let days_since_epoch = seconds.div_euclid(86_400);
    let (year, month, day) = civil_from_days(days_since_epoch);
    let time_of_day = seconds.rem_euclid(86_400);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
        time_of_day % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

fn extract_keys(xml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Key>") {
        let after = &rest[start + 5..];
        let Some(end) = after.find("</Key>") else {
            break;
        };
        keys.push(after[..end].to_string());
        rest = &after[end + 6..];
    }
    keys
}

/// Lists every key under an optional prefix, following continuation tokens.
async fn list_all_keys(config: &Config, prefix: &str) -> Result<Vec<String>, String> {
    let mut keys = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut query = vec![("list-type".into(), "2".into())];
        if !prefix.is_empty() {
            query.push(("prefix".into(), prefix.into()));
        }
        if let Some(token) = token.as_deref() {
            query.push(("continuation-token".into(), token.into()));
        }
        let response = signed_request(
            config,
            &reqwest::Method::GET,
            None,
            &query,
            Vec::new(),
        )
        .await?;
        if !response.status().is_success() {
            return Err(format!("list failed: HTTP {}", response.status()));
        }
        let body = response
            .text()
            .await
            .map_err(|error| error.to_string())?;
        keys.extend(extract_keys(&body));
        let truncated = extract_tag(&body, "IsTruncated")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        token = if truncated {
            Some(extract_tag(&body, "NextContinuationToken").ok_or("truncated list without token")?)
        } else {
            None
        };
        if token.is_none() {
            return Ok(keys);
        }
    }
}

fn read_stdin() -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut buffer = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buffer)
        .map_err(|error| format!("failed to read stdin: {error}"))?;
    Ok(buffer)
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

async fn mirror_to(config: &Config, target_dir: &Path) -> Result<(), String> {
    for key in list_all_keys(config, "").await? {
        if key.is_empty() {
            continue;
        }
        let destination = target_dir.join(&key);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let response = signed_request(config, &reqwest::Method::GET, Some(&key), &[], Vec::new())
            .await?;
        if !response.status().is_success() {
            return Err(format!("download {key} failed: HTTP {}", response.status()));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| error.to_string())?;
        std::fs::write(&destination, &bytes).map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn mirror_from(
    config: &Config,
    source_dir: &Path,
    remove_missing: bool,
) -> Result<(), String> {
    let mut files = Vec::new();
    collect_files(source_dir, &mut files);
    let mut desired_keys = Vec::new();
    for path in files {
        let key = path
            .strip_prefix(source_dir)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        if key.is_empty() {
            continue;
        }
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let response =
            signed_request(config, &reqwest::Method::PUT, Some(&key), &[], bytes).await?;
        if !response.status().is_success() {
            return Err(format!("upload {key} failed: HTTP {}", response.status()));
        }
        desired_keys.push(key);
    }
    if remove_missing {
        for key in list_all_keys(config, "").await? {
            if !desired_keys.contains(&key) {
                let response =
                    signed_request(config, &reqwest::Method::DELETE, Some(&key), &[], Vec::new())
                        .await?;
                if !response.status().is_success() {
                    return Err(format!("remove {key} failed: HTTP {}", response.status()));
                }
            }
        }
    }
    Ok(())
}

fn usage() -> ! {
    eprintln!(
        "usage: s3ctl [--endpoint URL] [--access-key K] [--secret-key S] [--region R] --bucket B <command>\n\
         commands:\n\
           stat-bucket                    check the bucket exists (HEAD)\n\
           make-bucket                    create the bucket if missing (idempotent)\n\
           put <key>                      upload stdin to the key\n\
           get <key>                      download the key to stdout\n\
           delete <key>                   delete the key\n\
           list [prefix]                  print keys under the prefix\n\
           mirror-to <dir>                download the whole bucket into dir\n\
           mirror-from <dir> [--remove-missing]  upload dir into the bucket\n\
         credentials also fall back to S3_ENDPOINT/S3_ACCESS_KEY/S3_SECRET_KEY/S3_BUCKET/S3_REGION"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut overrides: Vec<(String, String)> = Vec::new();
    let mut bucket_override: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--endpoint" | "--access-key" | "--secret-key" | "--region" => {
                index += 1;
                let Some(value) = args.get(index) else { usage() };
                overrides.push((arg[2..].to_string(), value.clone()));
            }
            "--bucket" => {
                index += 1;
                bucket_override = Some(args.get(index).cloned().unwrap_or_else(|| usage()));
            }
            _ => positionals.push(arg.clone()),
        }
        index += 1;
    }
    let config = match Config::resolve(&overrides, bucket_override.as_deref()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("s3ctl: {error}");
            std::process::exit(2);
        }
    };
    let mut commands = positionals;
    let Some(command) = commands.first().cloned() else { usage() };
    commands.remove(0);
    let result = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(run(&config, &command, commands));
    if let Err(error) = result {
        eprintln!("s3ctl: {error}");
        std::process::exit(1);
    }
}

async fn run(config: &Config, command: &str, args: Vec<String>) -> Result<(), String> {
    match command {
        "stat-bucket" => {
            let response = signed_request(config, &reqwest::Method::HEAD, None, &[], Vec::new())
                .await?;
            match response.status().as_u16() {
                200 | 301 | 302 | 403 => Ok(()),
                status => Err(format!("bucket check failed: HTTP {status}")),
            }
        }
        "make-bucket" => {
            let response = signed_request(config, &reqwest::Method::PUT, None, &[], Vec::new())
                .await?;
            match response.status().as_u16() {
                200 | 201 | 204 | 409 => Ok(()),
                status => Err(format!("bucket create failed: HTTP {status}")),
            }
        }
        "put" => {
            let key = args.first().ok_or("put requires a key")?;
            let body = read_stdin()?;
            let response =
                signed_request(config, &reqwest::Method::PUT, Some(key), &[], body).await?;
            match response.status().as_u16() {
                200 | 201 | 204 => Ok(()),
                status => Err(format!("put {key} failed: HTTP {status}")),
            }
        }
        "get" => {
            let key = args.first().ok_or("get requires a key")?;
            let response =
                signed_request(config, &reqwest::Method::GET, Some(key), &[], Vec::new()).await?;
            match response.status().as_u16() {
                200 | 204 => {
                    use std::io::Write;
                    let bytes = response
                        .bytes()
                        .await
                        .map_err(|error| error.to_string())?;
                    std::io::stdout()
                        .write_all(&bytes)
                        .map_err(|error| error.to_string())?;
                    Ok(())
                }
                status => Err(format!("get {key} failed: HTTP {status}")),
            }
        }
        "delete" => {
            let key = args.first().ok_or("delete requires a key")?;
            let response =
                signed_request(config, &reqwest::Method::DELETE, Some(key), &[], Vec::new())
                    .await?;
            match response.status().as_u16() {
                200 | 202 | 204 => Ok(()),
                status => Err(format!("delete {key} failed: HTTP {status}")),
            }
        }
        "list" => {
            let prefix = args.first().cloned().unwrap_or_default();
            for key in list_all_keys(config, &prefix).await? {
                println!("{key}");
            }
            Ok(())
        }
        "mirror-to" => {
            let dir = args.first().map(PathBuf::from).ok_or("mirror-to requires a directory")?;
            mirror_to(config, &dir).await
        }
        "mirror-from" => {
            let mut rest = args;
            let dir = rest
                .first()
                .map(PathBuf::from)
                .ok_or("mirror-from requires a directory")?;
            rest.remove(0);
            let remove_missing = rest.iter().any(|flag| flag == "--remove-missing");
            mirror_from(config, &dir, remove_missing).await
        }
        _ => usage(),
    }
}
