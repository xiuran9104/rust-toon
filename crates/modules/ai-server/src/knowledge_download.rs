use std::{net::{IpAddr, Ipv4Addr, SocketAddr}, time::Duration};

use futures_util::StreamExt;
use reqwest::{Client, Url, header::LOCATION, redirect::Policy};
use rust_toon_framework_web::AppError;
use sqlx::PgPool;
use tokio::sync::Semaphore;

const MAX_BYTES: usize = 20 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 3;
static DOWNLOADS: Semaphore = Semaphore::const_new(4);

pub(crate) async fn download_text(pool: &PgPool, source: &str) -> Result<String, AppError> {
    let _permit = DOWNLOADS.try_acquire()
        .map_err(|_| AppError::bad_request("文档下载繁忙，请稍后重试"))?;
    tokio::time::timeout(TIMEOUT, async {
        let bytes = if source.starts_with("/upload/") || source.starts_with("/api/upload/") {
            rust_toon_infra_server::read_uploaded_file(pool, source, MAX_BYTES).await?
        } else {
            let allowed = allowed_origins(std::env::var("KNOWLEDGE_DOWNLOAD_ALLOWED_ORIGINS").unwrap_or_default().as_str())?;
            download_remote(source, &allowed, MAX_BYTES, TIMEOUT).await?
        };
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }).await.map_err(|_| AppError::bad_request("文档下载超时"))?
}

fn allowed_origins(value: &str) -> Result<Vec<String>, AppError> {
    value.split(',').map(str::trim).filter(|value| !value.is_empty()).map(|value| {
        let url = parse_url(value)?;
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err(AppError::bad_request("文档下载允许来源必须是完整 origin，不包含路径或参数"));
        }
        Ok(url.origin().ascii_serialization())
    }).collect()
}

fn parse_url(value: &str) -> Result<Url, AppError> {
    let url = Url::parse(value).map_err(|_| AppError::bad_request("无效的文档 URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::bad_request("文档 URL 仅允许不含账号密码的 HTTP(S) 地址"));
    }
    Ok(url)
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let address = u32::from(ip);
    let blocked = [
        (Ipv4Addr::new(0, 0, 0, 0), 8),
        (Ipv4Addr::new(10, 0, 0, 0), 8),
        (Ipv4Addr::new(100, 64, 0, 0), 10),
        (Ipv4Addr::new(127, 0, 0, 0), 8),
        (Ipv4Addr::new(169, 254, 0, 0), 16),
        (Ipv4Addr::new(172, 16, 0, 0), 12),
        (Ipv4Addr::new(192, 0, 0, 0), 24),
        (Ipv4Addr::new(192, 0, 2, 0), 24),
        (Ipv4Addr::new(192, 88, 99, 0), 24),
        (Ipv4Addr::new(192, 168, 0, 0), 16),
        (Ipv4Addr::new(198, 18, 0, 0), 15),
        (Ipv4Addr::new(198, 51, 100, 0), 24),
        (Ipv4Addr::new(203, 0, 113, 0), 24),
        (Ipv4Addr::new(224, 0, 0, 0), 3),
    ];
    !blocked.into_iter().any(|(network, bits)| {
        let mask = u32::MAX << (32 - bits);
        address & mask == u32::from(network) & mask
    })
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() { return public_v4(ip); }
            let segments = ip.segments();
            // Restrict IPv6 to global unicast, excluding special-use and
            // transition prefixes that can encode a different destination.
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] < 0x0200 || segments[1] == 0x0db8))
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] < 0x1000)
        }
    }
}

async fn checked_client(url: &Url, allowed: &[String], timeout: Duration) -> Result<Client, AppError> {
    let host = url.host_str().ok_or_else(|| AppError::bad_request("文档 URL 缺少主机"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = url.port_or_known_default().ok_or_else(|| AppError::bad_request("无效的文档端口"))?;
    let addresses: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        tokio::net::lookup_host((host, port)).await
            .map_err(|_| AppError::bad_request("无法解析文档主机"))?.collect()
    };
    let trusted = allowed.contains(&url.origin().ascii_serialization());
    if addresses.is_empty() || (!trusted && addresses.iter().any(|address| !public_ip(address.ip()))) {
        return Err(AppError::bad_request("文档下载不允许访问该网络地址"));
    }
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(timeout.min(Duration::from_secs(5)))
        .timeout(timeout)
        // Pin the checked addresses. A second DNS lookup must not be able to
        // replace a public address with an internal destination.
        .resolve_to_addrs(host, &addresses)
        .build().map_err(|_| AppError::internal("无法创建文档下载客户端"))
}

async fn download_remote(source: &str, allowed: &[String], max_bytes: usize, timeout: Duration) -> Result<Vec<u8>, AppError> {
    let mut url = parse_url(source)?;
    for hop in 0..=MAX_REDIRECTS {
        let client = checked_client(&url, allowed, timeout).await?;
        let response = client.get(url.clone()).send().await
            .map_err(|_| AppError::bad_request("文档下载失败或超时"))?;
        if response.status().is_redirection() {
            if hop == MAX_REDIRECTS { return Err(AppError::bad_request("文档重定向次数过多")); }
            let location = response.headers().get(LOCATION).and_then(|value| value.to_str().ok())
                .ok_or_else(|| AppError::bad_request("无效的文档重定向"))?;
            let next = url.join(location).map_err(|_| AppError::bad_request("无效的文档重定向"))?;
            let next = parse_url(next.as_str())?;
            if url.scheme() == "https" && next.scheme() != "https" {
                return Err(AppError::bad_request("文档重定向不能降低连接安全性"));
            }
            url = next;
            continue;
        }
        if !response.status().is_success() { return Err(AppError::bad_request("文档服务器返回错误状态")); }
        if response.content_length().is_some_and(|length| length > max_bytes as u64) {
            return Err(AppError::bad_request("文档超过下载大小上限"));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| AppError::bad_request("文档响应读取失败或超时"))?;
            if bytes.len().saturating_add(chunk.len()) > max_bytes {
                return Err(AppError::bad_request("文档超过下载大小上限"));
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(bytes);
    }
    Err(AppError::bad_request("文档重定向次数过多"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Response, routing::get};

    #[test]
    fn rejects_private_special_use_and_encoded_ip_destinations() {
        for value in ["0.1.2.3", "10.0.0.1", "100.64.0.1", "127.0.0.1", "169.254.169.254", "172.16.0.1", "192.168.1.1", "198.18.0.1", "224.0.0.1", "255.255.255.255", "::1", "::", "fc00::1", "fe80::1", "::ffff:127.0.0.1", "2002:7f00:1::", "2001:db8::1", "3fff::1"] {
            assert!(!public_ip(value.parse().unwrap()), "allowed {value}");
        }
        for value in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(public_ip(value.parse().unwrap()));
        }
        for source in ["http://2130706433/a", "http://0x7f000001/a", "http://127.1/a"] {
            let url = parse_url(source).unwrap();
            assert!(!public_ip(url.host_str().unwrap().parse().unwrap()));
        }
        for source in ["file:///etc/passwd", "ftp://example.com/a", "http://user:pass@example.com/a"] {
            assert!(parse_url(source).is_err());
        }
        assert!(allowed_origins("http://127.0.0.1:8080/path").is_err());
        assert_eq!(allowed_origins("https://EXAMPLE.COM:443/").unwrap(), ["https://example.com"]);
    }

    async fn mock_server() -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/ok", get(|| async { "document" }))
            .route("/large", get(|| async { "a".repeat(1024) }))
            .route("/chunked", get(|| async {
                Body::from_stream(futures_util::stream::iter([
                    Ok::<_, std::convert::Infallible>("12345"), Ok("67890")
                ]))
            }))
            .route("/slow", get(|| async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                "late"
            }))
            .route("/redirect", get(|| async {
                Response::builder().status(302).header(LOCATION, "/ok").body(Body::empty()).unwrap()
            }))
            .route("/loop", get(|| async {
                Response::builder().status(302).header(LOCATION, "/loop").body(Body::empty()).unwrap()
            }))
            .route("/internal", get(|| async {
                Response::builder().status(302).header(LOCATION, "http://127.0.0.1:1/secret").body(Body::empty()).unwrap()
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        (origin, server)
    }

    #[tokio::test]
    async fn bounds_downloads_and_revalidates_every_redirect() {
        let (origin, server) = mock_server().await;
        let allowed = vec![origin.clone()];
        let timeout = Duration::from_secs(2);
        assert!(download_remote(&format!("{origin}/ok"), &[], 32, timeout).await.is_err());
        assert_eq!(download_remote(&format!("{origin}/redirect"), &allowed, 32, timeout).await.unwrap(), b"document");
        for path in ["/large", "/chunked", "/loop", "/internal"] {
            assert!(download_remote(&format!("{origin}{path}"), &allowed, 8, timeout).await.is_err(), "accepted {path}");
        }
        assert!(download_remote(&format!("{origin}/slow"), &allowed, 32, Duration::from_millis(50)).await.is_err());
        server.abort();
    }
}
