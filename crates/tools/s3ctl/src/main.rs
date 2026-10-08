//! s3ctl：仓库自带的极简 S3 兼容存储客户端，供备份与 e2e 脚本使用，
//! 取代对任何外部 S3 CLI（mc/aws）的依赖。传输与签名由 `rust-s3` crate
//! 提供（MIT，非 MinIO）；本文件只保留命令行包装。命令面：stat/make
//! bucket、put/get/delete、list、双向 mirror。凭据走 S3_* 环境变量或
//! 命令行参数，与部署配置同名。
//!
//! 升级路径（2026-10-07 决策）：内部已采用 rust-s3；当出现单对象 >5GB
//! 分片、并发恢复的条件写入、多云后端，或希望网关与工具统一到同一
//! 客户端库时，切换到 `object_store`（Apache Arrow）。命令面保持不变。

use s3::creds::Credentials;
use s3::region::Region;
use s3::Bucket;
use std::path::{Path, PathBuf};

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

fn bucket_handle(config: &Config) -> Result<Box<Bucket>, String> {
    let credentials = Credentials::new(
        Some(&config.access_key),
        Some(&config.secret_key),
        None,
        None,
        None,
    )
    .map_err(|error| format!("invalid credentials: {error}"))?;
    let region = Region::Custom {
        region: config.region.clone(),
        endpoint: config.endpoint.clone(),
    };
    let bucket = Bucket::new(&config.bucket, region, credentials)
        .map_err(|error| format!("invalid bucket config: {error}"))?;
    // RustFS 和网关侧一致使用 path-style 寻址。
    Ok(bucket.with_path_style())
}

/// 列出前缀下全部对象键，自动跟随 continuation token。
async fn list_all_keys(bucket: &Bucket, prefix: &str) -> Result<Vec<String>, String> {
    let mut keys = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let (page, status) = bucket
            .list_page(
                prefix.to_string(),
                None,
                token,
                None,
                None,
            )
            .await
            .map_err(|error| format!("list failed: {error}"))?;
        if !(200..300).contains(&status) {
            return Err(format!("list failed: HTTP {status}"));
        }
        keys.extend(page.contents.into_iter().map(|object| object.key));
        token = page.next_continuation_token;
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

async fn mirror_to(bucket: &Bucket, target_dir: &Path) -> Result<(), String> {
    for key in list_all_keys(bucket, "").await? {
        if key.is_empty() {
            continue;
        }
        let destination = target_dir.join(&key);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let response = bucket
            .get_object(&key)
            .await
            .map_err(|error| format!("download {key} failed: {error}"))?;
        if !(200..300).contains(&response.status_code()) {
            return Err(format!("download {key} failed: HTTP {}", response.status_code()));
        }
        std::fs::write(&destination, response.bytes())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn mirror_from(
    bucket: &Bucket,
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
        let response = bucket
            .put_object(&key, &bytes)
            .await
            .map_err(|error| format!("upload {key} failed: {error}"))?;
        if !(200..300).contains(&response.status_code()) {
            return Err(format!("upload {key} failed: HTTP {}", response.status_code()));
        }
        desired_keys.push(key);
    }
    if remove_missing {
        for key in list_all_keys(bucket, "").await? {
            if !desired_keys.contains(&key) {
                let response = bucket
                    .delete_object(&key)
                    .await
                    .map_err(|error| format!("remove {key} failed: {error}"))?;
                if !(200..300).contains(&response.status_code()) {
                    return Err(format!("remove {key} failed: HTTP {}", response.status_code()));
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
    let bucket = bucket_handle(config)?;
    match command {
        "stat-bucket" => {
            let exists = bucket
                .exists()
                .await
                .map_err(|error| format!("bucket check failed: {error}"))?;
            if exists {
                Ok(())
            } else {
                Err("bucket does not exist".into())
            }
        }
        "make-bucket" => {
            // rust-s3 0.38 的 Bucket::create 内部不使用 path-style（自定义
            // endpoint 会拼出 bucket.127.0.0.1 非法主机），这里直接对空键
            // 发 PUT——即 path-style 的 PUT /{bucket}/ 建桶请求。
            let response = bucket
                .put_object("", &[])
                .await
                .map_err(|error| format!("bucket create failed: {error}"))?;
            match response.status_code() {
                200 | 201 | 204 | 409 => Ok(()),
                status => Err(format!("bucket create failed: HTTP {status}")),
            }
        }
        "put" => {
            let key = args.first().ok_or("put requires a key")?;
            let body = read_stdin()?;
            let response = bucket
                .put_object(key, &body)
                .await
                .map_err(|error| format!("put {key} failed: {error}"))?;
            if !(200..300).contains(&response.status_code()) {
                return Err(format!("put {key} failed: HTTP {}", response.status_code()));
            }
            Ok(())
        }
        "get" => {
            let key = args.first().ok_or("get requires a key")?;
            let response = bucket
                .get_object(key)
                .await
                .map_err(|error| format!("get {key} failed: {error}"))?;
            if !(200..300).contains(&response.status_code()) {
                return Err(format!("get {key} failed: HTTP {}", response.status_code()));
            }
            use std::io::Write;
            std::io::stdout()
                .write_all(&response.bytes())
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        "delete" => {
            let key = args.first().ok_or("delete requires a key")?;
            let response = bucket
                .delete_object(key)
                .await
                .map_err(|error| format!("delete {key} failed: {error}"))?;
            if !(200..300).contains(&response.status_code()) {
                return Err(format!("delete {key} failed: HTTP {}", response.status_code()));
            }
            Ok(())
        }
        "list" => {
            let prefix = args.first().cloned().unwrap_or_default();
            for key in list_all_keys(&bucket, &prefix).await? {
                println!("{key}");
            }
            Ok(())
        }
        "mirror-to" => {
            let dir = args.first().map(PathBuf::from).ok_or("mirror-to requires a directory")?;
            mirror_to(&bucket, &dir).await
        }
        "mirror-from" => {
            let mut rest = args;
            let dir = rest
                .first()
                .map(PathBuf::from)
                .ok_or("mirror-from requires a directory")?;
            rest.remove(0);
            let remove_missing = rest.iter().any(|flag| flag == "--remove-missing");
            mirror_from(&bucket, &dir, remove_missing).await
        }
        _ => usage(),
    }
}
