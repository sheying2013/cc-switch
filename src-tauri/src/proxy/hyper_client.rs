//! Hyper-based HTTP client for proxy forwarding
//!
//! Uses raw TCP/TLS writes to preserve exact original header name casing.
//! Supports HTTP CONNECT tunneling through upstream proxies.
//! Falls back to hyper-util Client (title-case headers) when raw write is not feasible.

use super::ProxyError;
use bytes::Bytes;
use futures::{stream::Stream, StreamExt};
use http_body_util::BodyExt;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use std::sync::OnceLock;

/// Our own header case map: maps lowercase header name → original wire-casing bytes.
///
/// This is a backup mechanism independent of hyper's internal `HeaderCaseMap` (which is
/// `pub(crate)` and cannot be directly inspected or constructed from outside hyper).
///
/// Populated in `server.rs` by peeking at raw TCP bytes before hyper parses them.
/// Used in `send_request` to manually write headers with original casing when hyper's
/// own mechanism fails.
#[derive(Clone, Debug, Default)]
pub(crate) struct OriginalHeaderCases {
    /// Ordered list of (lowercase_name, original_wire_bytes) pairs.
    /// Multiple entries with the same name are allowed (for repeated headers).
    pub cases: Vec<(String, Vec<u8>)>,
}

impl OriginalHeaderCases {
    /// Parse raw HTTP request bytes (from TcpStream::peek) to extract original header casings.
    pub fn from_raw_bytes(buf: &[u8]) -> Self {
        let mut headers_buf = [httparse::EMPTY_HEADER; 128];
        let mut req = httparse::Request::new(&mut headers_buf);
        // We don't care if parsing is partial — we just want the header names we can get
        let _ = req.parse(buf);

        let mut cases = Vec::new();
        for header in req.headers.iter() {
            if header.name.is_empty() {
                break;
            }
            cases.push((
                header.name.to_ascii_lowercase(),
                header.name.as_bytes().to_vec(),
            ));
        }

        Self { cases }
    }
}

type HyperClient = Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    http_body_util::Full<Bytes>,
>;

/// Lazily-initialized hyper client with header-case preservation enabled.
fn global_hyper_client() -> &'static HyperClient {
    static CLIENT: OnceLock<HyperClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();

        Client::builder(TokioExecutor::new())
            .http1_preserve_header_case(true)
            .http1_title_case_headers(true)
            .build(connector)
    })
}

/// 响应体读取上限（128 MiB）。正常非流式补全响应只有几十到几百 KiB；超过则视为
/// 上游异常或恶意 payload，直接拒绝，避免代理进程被超大响应体/压缩炸弹耗尽内存。
pub(crate) const MAX_RESPONSE_BODY_BYTES: usize = 128 * 1024 * 1024;

/// Unified response wrapper that can hold either a hyper or reqwest response.
///
/// The hyper variant is used for the main (direct) path with header-case preservation.
/// The reqwest variant is the fallback when an upstream HTTP/SOCKS5 proxy is configured.
pub enum ProxyResponse {
    Hyper(hyper::Response<hyper::body::Incoming>),
    Reqwest(reqwest::Response),
    Buffered {
        status: http::StatusCode,
        headers: http::HeaderMap,
        body: Bytes,
    },
    Streamed {
        status: http::StatusCode,
        headers: http::HeaderMap,
        stream: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    },
}

impl ProxyResponse {
    pub fn buffered(status: http::StatusCode, headers: http::HeaderMap, body: Bytes) -> Self {
        Self::Buffered {
            status,
            headers,
            body,
        }
    }

    pub fn streamed(
        status: http::StatusCode,
        headers: http::HeaderMap,
        stream: impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    ) -> Self {
        Self::Streamed {
            status,
            headers,
            stream: Box::pin(stream),
        }
    }

    pub fn status(&self) -> http::StatusCode {
        match self {
            Self::Hyper(r) => r.status(),
            Self::Reqwest(r) => r.status(),
            Self::Buffered { status, .. } | Self::Streamed { status, .. } => *status,
        }
    }

    pub fn headers(&self) -> &http::HeaderMap {
        match self {
            Self::Hyper(r) => r.headers(),
            Self::Reqwest(r) => r.headers(),
            Self::Buffered { headers, .. } | Self::Streamed { headers, .. } => headers,
        }
    }

    /// Shortcut: extract `content-type` header value as `&str`.
    pub fn content_type(&self) -> Option<&str> {
        self.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
    }

    /// Check if the response is an SSE stream.
    pub fn is_sse(&self) -> bool {
        self.content_type()
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false)
    }

    /// Check whether the response explicitly declares a JSON media type.
    pub fn is_json(&self) -> bool {
        self.content_type()
            .map(|content_type| {
                let media_type = content_type
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                media_type == "application/json" || media_type.ends_with("+json")
            })
            .unwrap_or(false)
    }

    /// Consume the response and collect the full body into `Bytes`, aborting the
    /// read as soon as the accumulated body exceeds `max_bytes`.
    ///
    /// 所有变体都在累积过程中逐块检查、超限即断开（drop stream 中止上游连接），
    /// 而不是先收满再比较——否则超大明文 body 仍会完整进入内存，限制形同虚设。
    pub async fn bytes_with_limit(self, max_bytes: usize) -> Result<Bytes, ProxyError> {
        match self {
            Self::Buffered { body, .. } => {
                // 调用方已把 body 完整缓冲，无法中途截停，只能事后比较
                if body.len() > max_bytes {
                    return Err(ProxyError::ResponseBodyTooLarge(body.len()));
                }
                Ok(body)
            }
            response => {
                // Hyper / Reqwest / Streamed 统一走逐块流式累积，超预算立即报错
                let mut stream = response.bytes_stream();
                let mut body = bytes::BytesMut::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| {
                        ProxyError::ForwardFailed(format!("Failed to read response body: {e}"))
                    })?;
                    if body.len() + chunk.len() > max_bytes {
                        return Err(ProxyError::ResponseBodyTooLarge(body.len() + chunk.len()));
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(body.freeze())
            }
        }
    }

    /// Consume the response and return a byte-chunk stream (for SSE pass-through).
    pub fn bytes_stream(self) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
        use futures::StreamExt;

        match self {
            Self::Hyper(r) => {
                let body = r.into_body();
                let stream = futures::stream::unfold(body, |mut body| async {
                    match body.frame().await {
                        Some(Ok(frame)) => {
                            if let Ok(data) = frame.into_data() {
                                if data.is_empty() {
                                    Some((Ok(Bytes::new()), body))
                                } else {
                                    Some((Ok(data), body))
                                }
                            } else {
                                Some((Ok(Bytes::new()), body))
                            }
                        }
                        Some(Err(e)) => Some((Err(std::io::Error::other(e.to_string())), body)),
                        None => None,
                    }
                })
                .filter(|result| {
                    futures::future::ready(!matches!(result, Ok(ref b) if b.is_empty()))
                });
                Box::pin(stream)
                    as std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>
            }
            Self::Reqwest(r) => {
                let stream = r
                    .bytes_stream()
                    .map(|r| r.map_err(|e| std::io::Error::other(e.to_string())));
                Box::pin(stream)
            }
            Self::Buffered { body, .. } => Box::pin(futures::stream::once(async move { Ok(body) }))
                as std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
            Self::Streamed { stream, .. } => stream,
        }
    }
}

/// Send an HTTP request with header-case preservation.
///
/// Uses a two-tier strategy:
/// 1. Primary: raw HTTP/1.1 write via TLS stream with exact original header casing
///    (from `OriginalHeaderCases` captured by peek in server.rs), then hand off to
///    hyper for response parsing.
/// 2. Fallback: hyper-util Client with `title_case_headers(true)` when raw write
///    isn't feasible (e.g., missing original cases).
///
/// The caller is expected to include `Host` in the supplied `headers` at the
/// correct position.
///
/// `proxy_url`: optional upstream HTTP proxy URL (e.g. `http://127.0.0.1:7890`).
/// When set, the raw write path uses HTTP CONNECT tunneling through the proxy,
/// so header-case preservation works even when an upstream proxy is configured.
///
/// `log_display` is a caller-supplied, already-sanitized string used only for
/// logging; this layer never derives a log value from the raw `uri`.
///
/// `front_proxy_url`: optional front proxy URL (e.g. global outbound proxy when chaining)
/// `target_proxy_url`: optional target proxy URL (e.g. provider individual proxy or global proxy)
#[allow(clippy::too_many_arguments)]
pub async fn send_request(
    uri: http::Uri,
    log_display: &str,
    method: http::Method,
    headers: http::HeaderMap,
    original_extensions: http::Extensions,
    body: Vec<u8>,
    timeout: std::time::Duration,
    front_proxy_url: Option<&str>,
    target_proxy_url: Option<&str>,
) -> Result<ProxyResponse, ProxyError> {
    let mut hops = Vec::new();
    if let Some(f) = front_proxy_url.filter(|s| !s.trim().is_empty()) {
        hops.push(ProxyHop::parse(f)?);
    }
    if let Some(t) = target_proxy_url.filter(|s| !s.trim().is_empty()) {
        hops.push(ProxyHop::parse(t)?);
    }

    // Extract our own OriginalHeaderCases if available
    let original_cases = original_extensions.get::<OriginalHeaderCases>().cloned();
    let has_cases = original_cases
        .as_ref()
        .map(|c| !c.cases.is_empty())
        .unwrap_or(false);
    let has_hops = !hops.is_empty();

    log::debug!(
        "[HyperClient] Sending request: target={}, header_count={},          has_host={}, has_original_cases={has_cases}, hops={}",
        log_display,
        headers.len(),
        headers.contains_key(http::header::HOST),
        hops.len(),
    );

    if has_hops || has_cases {
        let cases = original_cases.unwrap_or_default();
        let result = tokio::time::timeout(
            timeout,
            send_raw_request(&uri, &method, &headers, &cases, &body, &hops),
        )
        .await
        .map_err(|_| ProxyError::Timeout(format!("请求超时: {}s", timeout.as_secs())))?;

        match result {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                if has_hops {
                    return Err(e);
                }
                log::warn!("[HyperClient] Raw write failed, falling back to hyper-util: {e}");
            }
        }
    }

    // Fallback: hyper-util Client (title-case headers, direct only)
    let mut req = http::Request::builder()
        .method(method)
        .uri(&uri)
        .body(http_body_util::Full::new(Bytes::from(body)))
        .map_err(|e| ProxyError::ForwardFailed(format!("Failed to build request: {e}")))?;

    *req.headers_mut() = headers;
    *req.extensions_mut() = original_extensions;

    let client = global_hyper_client();
    let resp = tokio::time::timeout(timeout, client.request(req))
        .await
        .map_err(|_| ProxyError::Timeout(format!("请求超时: {}s", timeout.as_secs())))?
        .map_err(|e| ProxyError::ForwardFailed(format!("上游请求失败: {e}")))?;

    Ok(ProxyResponse::Hyper(resp))
}

pub trait AsyncStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> AsyncStream for T {}
pub type BoxedStream = Box<dyn AsyncStream>;

#[derive(Debug, Clone)]
pub struct ProxyHop {
    pub url: String,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl ProxyHop {
    pub fn parse(proxy_url: &str) -> Result<Self, ProxyError> {
        let parsed = url::Url::parse(proxy_url)
            .map_err(|e| ProxyError::ForwardFailed(format!("Invalid proxy URL '{}': {e}", super::http_client::mask_url(proxy_url))))?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        if !["http", "https", "socks5", "socks5h"].contains(&scheme.as_str()) {
            return Err(ProxyError::ForwardFailed(format!(
                "Unsupported proxy scheme '{}'. Supported: http, https, socks5, socks5h",
                scheme
            )));
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| ProxyError::ForwardFailed(format!("Proxy URL '{}' has no host", super::http_client::mask_url(proxy_url))))?
            .to_string();
        let port = parsed.port().unwrap_or(if scheme == "https" {
            443
        } else if scheme.starts_with("socks5") {
            1080
        } else {
            80
        });
        let username = parsed.username().to_string();
        let password = parsed.password().unwrap_or("").to_string();
        Ok(Self {
            url: proxy_url.to_string(),
            scheme,
            host,
            port,
            username,
            password,
        })
    }
}

/// Perform SOCKS5 client handshake and connect to target destination
async fn socks5_handshake<S>(
    stream: &mut S,
    username: &str,
    password: &str,
    target_host: &str,
    target_port: u16,
) -> Result<(), ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // 1. Version & Auth negotiation
    if !username.is_empty() {
        stream
            .write_all(&[0x05, 0x02, 0x00, 0x02])
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 greeting write failed: {e}")))?;
    } else {
        stream
            .write_all(&[0x05, 0x01, 0x00])
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 greeting write failed: {e}")))?;
    }
    stream
        .flush()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 greeting flush failed: {e}")))?;

    let mut method_buf = [0u8; 2];
    stream
        .read_exact(&mut method_buf)
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 read method choice failed: {e}")))?;

    if method_buf[0] != 0x05 {
        return Err(ProxyError::ForwardFailed(format!(
            "Invalid SOCKS version: {}",
            method_buf[0]
        )));
    }

    match method_buf[1] {
        0x00 => {
            // No authentication required
        }
        0x02 => {
            // Username / Password authentication (RFC 1929)
            let mut auth_req = Vec::with_capacity(3 + username.len() + password.len());
            auth_req.push(0x01);
            auth_req.push(username.len() as u8);
            auth_req.extend_from_slice(username.as_bytes());
            auth_req.push(password.len() as u8);
            auth_req.extend_from_slice(password.as_bytes());

            stream
                .write_all(&auth_req)
                .await
                .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 auth write failed: {e}")))?;
            stream
                .flush()
                .await
                .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 auth flush failed: {e}")))?;

            let mut auth_resp = [0u8; 2];
            stream
                .read_exact(&mut auth_resp)
                .await
                .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 auth response read failed: {e}")))?;

            if auth_resp[1] != 0x00 {
                return Err(ProxyError::AuthError(format!(
                    "SOCKS5 authentication rejected with code: {}",
                    auth_resp[1]
                )));
            }
        }
        0xFF => {
            return Err(ProxyError::AuthError(
                "SOCKS5 server rejected authentication methods (0xFF)".to_string(),
            ));
        }
        other => {
            return Err(ProxyError::ForwardFailed(format!(
                "Unsupported SOCKS5 auth method: {other}"
            )));
        }
    }

    // 2. CONNECT request
    let mut connect_req = Vec::with_capacity(10 + target_host.len());
    connect_req.extend_from_slice(&[0x05, 0x01, 0x00]);

    if let Ok(ipv4) = target_host.parse::<std::net::Ipv4Addr>() {
        connect_req.push(0x01); // ATYP: IPv4
        connect_req.extend_from_slice(&ipv4.octets());
    } else if let Ok(ipv6) = target_host.parse::<std::net::Ipv6Addr>() {
        connect_req.push(0x04); // ATYP: IPv6
        connect_req.extend_from_slice(&ipv6.octets());
    } else {
        let host_bytes = target_host.as_bytes();
        if host_bytes.len() > 255 {
            return Err(ProxyError::ForwardFailed(
                "Target host is too long for SOCKS5 (> 255 bytes)".to_string(),
            ));
        }
        connect_req.push(0x03); // ATYP: Domain
        connect_req.push(host_bytes.len() as u8);
        connect_req.extend_from_slice(host_bytes);
    }
    connect_req.extend_from_slice(&target_port.to_be_bytes());

    stream
        .write_all(&connect_req)
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 connect command failed: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 connect flush failed: {e}")))?;

    // 3. Read reply
    let mut reply_header = [0u8; 4];
    stream
        .read_exact(&mut reply_header)
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 reply header read failed: {e}")))?;

    if reply_header[0] != 0x05 {
        return Err(ProxyError::ForwardFailed(format!(
            "Invalid SOCKS version in reply: {}",
            reply_header[0]
        )));
    }

    let rep = reply_header[1];
    if rep != 0x00 {
        return Err(ProxyError::ForwardFailed(format!(
            "SOCKS5 connect failed: error code {rep}"
        )));
    }

    match reply_header[3] {
        0x01 => {
            let mut addr_port = [0u8; 6];
            stream
                .read_exact(&mut addr_port)
                .await
                .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 drain ipv4 failed: {e}")))?;
        }
        0x04 => {
            let mut addr_port = [0u8; 18];
            stream
                .read_exact(&mut addr_port)
                .await
                .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 drain ipv6 failed: {e}")))?;
        }
        0x03 => {
            let mut len_buf = [0u8; 1];
            stream
                .read_exact(&mut len_buf)
                .await
                .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 drain domain len failed: {e}")))?;
            let mut domain_and_port = vec![0u8; len_buf[0] as usize + 2];
            stream
                .read_exact(&mut domain_and_port)
                .await
                .map_err(|e| ProxyError::ForwardFailed(format!("SOCKS5 drain domain failed: {e}")))?;
        }
        atyp => {
            return Err(ProxyError::ForwardFailed(format!(
                "Invalid ATYP in SOCKS5 reply: {atyp}"
            )));
        }
    }

    log::debug!("[HyperClient] SOCKS5 tunnel established to {target_host}:{target_port}");
    Ok(())
}

/// Perform HTTP CONNECT tunnel handshake
async fn http_connect_handshake<S>(
    stream: &mut S,
    username: &str,
    password: &str,
    target_host: &str,
    target_port: u16,
) -> Result<(), ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use base64::Engine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let proxy_auth = if !username.is_empty() {
        let credentials = format!("{username}:{password}");
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        Some(format!("Proxy-Authorization: Basic {encoded}\r\n"))
    } else {
        None
    };

    let mut connect_req = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\nUser-Agent: CC-Switch\r\nProxy-Connection: Keep-Alive\r\n"
    );
    if let Some(auth) = &proxy_auth {
        connect_req.push_str(auth);
    }
    connect_req.push_str("\r\n");

    stream
        .write_all(connect_req.as_bytes())
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("CONNECT write failed: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("CONNECT flush failed: {e}")))?;

    // Read response headers byte-by-byte up to CRLF CRLF so we do not buffer/consume
    // any payload bytes belonging to the tunneled protocol.
    let mut header_bytes = Vec::with_capacity(512);
    let mut b = [0u8; 1];
    while !header_bytes.ends_with(b"\r\n\r\n") && !header_bytes.ends_with(b"\n\n") {
        if header_bytes.len() > 16384 {
            return Err(ProxyError::ForwardFailed(
                "Proxy CONNECT response header too large (> 16KB)".to_string(),
            ));
        }
        let n = stream
            .read(&mut b)
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("CONNECT response read failed: {e}")))?;
        if n == 0 {
            return Err(ProxyError::ForwardFailed(
                "Proxy CONNECT connection closed unexpectedly".to_string(),
            ));
        }
        header_bytes.push(b[0]);
    }

    let response_str = String::from_utf8_lossy(&header_bytes);
    let status_line = response_str.lines().next().unwrap_or("");
    if !status_line.contains(" 200 ") && !status_line.ends_with(" 200") {
        if status_line.contains(" 407 ") || status_line.ends_with(" 407") {
            return Err(ProxyError::AuthError(format!(
                "Proxy authentication required (407): {}",
                status_line.trim()
            )));
        }
        return Err(ProxyError::ForwardFailed(format!(
            "Proxy CONNECT rejected: {}",
            status_line.trim()
        )));
    }

    log::debug!("[HyperClient] HTTP CONNECT tunnel established to {target_host}:{target_port}");
    Ok(())
}

/// Connect through a sequence of proxy hops (or direct if empty)
async fn connect_hops(
    hops: &[ProxyHop],
    target_host: &str,
    target_port: u16,
) -> Result<BoxedStream, ProxyError> {
    if hops.is_empty() {
        let tcp = tokio::net::TcpStream::connect((target_host, target_port))
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("TCP connect failed: {e}")))?;
        return Ok(Box::new(tcp));
    }

    // 1. Connect TCP to first proxy hop
    let first = &hops[0];
    let tcp = tokio::net::TcpStream::connect((first.host.as_str(), first.port))
        .await
        .map_err(|e| {
            ProxyError::ForwardFailed(format!(
                "Failed to connect to proxy {}: {e}",
                super::http_client::mask_url(&first.url)
            ))
        })?;

    let mut stream: BoxedStream = if first.scheme == "https" {
        let tls_connector = global_tls_connector();
        let server_name = rustls::pki_types::ServerName::try_from(first.host.clone())
            .map_err(|e| ProxyError::ForwardFailed(format!("Invalid proxy server name: {e}")))?;
        let tls_stream = tls_connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("Proxy TLS handshake failed: {e}")))?;
        Box::new(tls_stream)
    } else {
        Box::new(tcp)
    };

    // 2. Tunnel through each hop to the next hop or target
    for i in 0..hops.len() {
        let current_hop = &hops[i];
        let (dest_host, dest_port) = if i + 1 < hops.len() {
            (hops[i + 1].host.as_str(), hops[i + 1].port)
        } else {
            (target_host, target_port)
        };

        if current_hop.scheme.starts_with("socks5") {
            socks5_handshake(
                &mut stream,
                &current_hop.username,
                &current_hop.password,
                dest_host,
                dest_port,
            )
            .await?;
        } else {
            http_connect_handshake(
                &mut stream,
                &current_hop.username,
                &current_hop.password,
                dest_host,
                dest_port,
            )
            .await?;
        }
    }

    Ok(stream)
}

/// Send request via raw TCP/TLS with exact original header casing.
async fn send_raw_request(
    uri: &http::Uri,
    method: &http::Method,
    headers: &http::HeaderMap,
    original_cases: &OriginalHeaderCases,
    body: &[u8],
    hops: &[ProxyHop],
) -> Result<ProxyResponse, ProxyError> {
    use tokio::io::AsyncWriteExt;

    let scheme = uri.scheme_str().unwrap_or("https");
    let host = uri
        .host()
        .ok_or_else(|| ProxyError::ForwardFailed("URI has no host".into()))?;
    let port = uri
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");

    let raw = build_raw_request(method, path_and_query, headers, original_cases, body);

    let mut stream: BoxedStream = connect_hops(hops, host, port).await?;

    if scheme == "https" {
        let tls_connector = global_tls_connector();
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|e| ProxyError::ForwardFailed(format!("Invalid server name: {e}")))?;
        let tls_stream = tls_connector
            .connect(server_name, stream)
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("TLS handshake failed: {e}")))?;
        stream = Box::new(tls_stream);
    }

    stream
        .write_all(&raw)
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Write failed: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Flush failed: {e}")))?;

    let filtered = WriteFilter::new(stream);
    do_hyper_response(filtered, method.clone()).await
}

/// Test a proxy or chained proxy connection by connecting to test targets
pub async fn test_proxy_chain(
    front_proxy: Option<&str>,
    target_proxy: &str,
) -> Result<u64, ProxyError> {
    use std::time::Instant;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut hops = Vec::new();
    if let Some(f) = front_proxy.filter(|s| !s.trim().is_empty()) {
        hops.push(ProxyHop::parse(f)?);
    }
    hops.push(ProxyHop::parse(target_proxy)?);

    let start = Instant::now();
    let test_targets = [
        ("httpbin.org", 443, "/get"),
        ("www.google.com", 443, "/"),
        ("api.anthropic.com", 443, "/"),
    ];

    let mut last_err = None;
    for (host, port, path) in test_targets {
        match connect_hops(&hops, host, port).await {
            Ok(stream) => {
                let tls_connector = global_tls_connector();
                let server_name = match rustls::pki_types::ServerName::try_from(host.to_string()) {
                    Ok(sn) => sn,
                    Err(e) => {
                        last_err = Some(ProxyError::ForwardFailed(e.to_string()));
                        continue;
                    }
                };

                match tls_connector.connect(server_name, stream).await {
                    Ok(mut tls_stream) => {
                        let req = format!("HEAD {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: CC-Switch\r\nConnection: close\r\n\r\n");
                        if tls_stream.write_all(req.as_bytes()).await.is_ok()
                            && tls_stream.flush().await.is_ok()
                        {
                            let mut reader = BufReader::new(tls_stream);
                            let mut status_line = String::new();
                            if reader.read_line(&mut status_line).await.is_ok()
                                && (status_line.contains(" 200 ")
                                    || status_line.contains(" 301 ")
                                    || status_line.contains(" 302 ")
                                    || status_line.contains(" 403 ")
                                    || status_line.contains(" 404 ")
                                    || status_line.contains(" 405 "))
                            {
                                return Ok(start.elapsed().as_millis() as u64);
                            }
                        }
                    }
                    Err(e) => {
                        last_err = Some(ProxyError::ForwardFailed(format!("TLS handshake failed: {e}")));
                    }
                }
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| ProxyError::ForwardFailed("All test targets failed".to_string())))
}

/// Lazily-initialized TLS connector for raw connections.
///
/// Loads both webpki roots AND native system certificates so that
/// proxy MITM CAs (e.g. Clash, mitmproxy) installed in the system
/// keychain are trusted through the CONNECT tunnel.
fn global_tls_connector() -> &'static tokio_rustls::TlsConnector {
    static CONNECTOR: OnceLock<tokio_rustls::TlsConnector> = OnceLock::new();
    CONNECTOR.get_or_init(|| {
        let mut root_store = rustls::RootCertStore::empty();
        // Baseline: Mozilla/webpki roots
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // Native system certs (includes user-installed proxy CAs)
        let native = rustls_native_certs::load_native_certs();
        let (added, _errors) = root_store.add_parsable_certificates(native.certs);
        log::debug!("[HyperClient] TLS root store: webpki + {added} native certs");
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        tokio_rustls::TlsConnector::from(std::sync::Arc::new(config))
    })
}

/// Build raw HTTP/1.1 request bytes with original header casing.
fn build_raw_request(
    method: &http::Method,
    path_and_query: &str,
    headers: &http::HeaderMap,
    original_cases: &OriginalHeaderCases,
    body: &[u8],
) -> Vec<u8> {
    let mut raw = Vec::with_capacity(4096 + body.len());

    // Request line
    raw.extend_from_slice(method.as_str().as_bytes());
    raw.extend_from_slice(b" ");
    raw.extend_from_slice(path_and_query.as_bytes());
    raw.extend_from_slice(b" HTTP/1.1\r\n");

    // Headers with original casing, emitted in original wire order.
    //
    // Strategy:
    // 1. Walk `original_cases.cases` in order — this preserves the exact
    //    header sequence the client sent.  For each entry, emit the stored
    //    original-casing name plus the current value from `headers` (the
    //    proxy may have rewritten the value, e.g. Authorization).
    //    Repeated headers with the same name are handled by tracking a
    //    per-name value cursor so we step through `get_all()` in order.
    // 2. After the original headers, append any headers that exist in
    //    `headers` but were not present in the original request (i.e. added
    //    by the proxy).  These are emitted in lowercase.
    //
    // This replaces the old `for name in headers.keys()` loop which iterated
    // in hash-map order, destroying the original header sequence.
    let mut emitted: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(original_cases.cases.len());
    // Per-name cursor: how many values we have already emitted for each name.
    let mut value_cursor: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(original_cases.cases.len());

    for (lower_name, orig_name_bytes) in &original_cases.cases {
        if let Ok(header_name) = http::header::HeaderName::from_bytes(lower_name.as_bytes()) {
            let all_values: Vec<_> = headers.get_all(&header_name).iter().collect();
            let cursor = value_cursor.entry(lower_name.clone()).or_insert(0);
            if let Some(value) = all_values.get(*cursor) {
                raw.extend_from_slice(orig_name_bytes);
                raw.extend_from_slice(b": ");
                raw.extend_from_slice(value.as_bytes());
                raw.extend_from_slice(b"\r\n");
                *cursor += 1;
                emitted.insert(lower_name.clone());
            }
        }
    }

    // Append proxy-added headers (not present in the original request).
    for name in headers.keys() {
        let lower = name.as_str().to_ascii_lowercase();
        if !emitted.contains(&lower) {
            for value in headers.get_all(name) {
                raw.extend_from_slice(name.as_str().as_bytes());
                raw.extend_from_slice(b": ");
                raw.extend_from_slice(value.as_bytes());
                raw.extend_from_slice(b"\r\n");
            }
            emitted.insert(lower);
        }
    }

    // Add Content-Length if not already present
    if !headers.contains_key(http::header::CONTENT_LENGTH) {
        raw.extend_from_slice(b"Content-Length: ");
        raw.extend_from_slice(body.len().to_string().as_bytes());
        raw.extend_from_slice(b"\r\n");
    }

    // End of headers + body
    raw.extend_from_slice(b"\r\n");
    raw.extend_from_slice(body);

    raw
}

/// Use hyper's low-level client to parse the response on a stream where we've
/// already written the request.
///
/// `WriteFilter` discards any writes from hyper (it would try to send its own
/// request encoding), while passing reads through transparently.
async fn do_hyper_response<S>(
    stream: WriteFilter<S>,
    method: http::Method,
) -> Result<ProxyResponse, ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let io = hyper_util::rt::TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .preserve_header_case(true)
        .handshake::<_, http_body_util::Full<Bytes>>(io)
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Handshake failed: {e}")))?;

    // Spawn the connection driver (reads responses from the stream)
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            log::debug!("[HyperClient] raw conn driver error: {e}");
        }
    });

    // Send a dummy request through hyper — hyper will encode this and try to write it,
    // but WriteFilter discards all writes. Hyper will then read the response from the stream.
    let dummy_req = http::Request::builder()
        .method(method)
        .uri("/")
        .body(http_body_util::Full::new(Bytes::new()))
        .map_err(|e| ProxyError::ForwardFailed(format!("Build dummy request: {e}")))?;

    let resp = sender
        .send_request(dummy_req)
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Response parse failed: {e}")))?;

    Ok(ProxyResponse::Hyper(resp))
}

/// A stream wrapper that discards all writes but passes reads through.
///
/// This lets hyper's connection driver think it sent a request (its encoded bytes
/// go to /dev/null), while correctly parsing the response that the upstream server
/// sends in reply to our raw-written request.
struct WriteFilter<S> {
    inner: S,
}

impl<S> WriteFilter<S> {
    fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for WriteFilter<S> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        // Pass reads through to the underlying stream
        let inner = std::pin::Pin::new(&mut self.get_mut().inner);
        inner.poll_read(cx, buf)
    }
}

impl<S: Unpin> tokio::io::AsyncWrite for WriteFilter<S> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        // Discard all writes — pretend they succeeded
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffered_with_content_type(content_type: Option<&str>) -> ProxyResponse {
        let mut headers = http::HeaderMap::new();
        if let Some(content_type) = content_type {
            headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_str(content_type).unwrap(),
            );
        }
        ProxyResponse::buffered(http::StatusCode::OK, headers, Bytes::new())
    }

    #[test]
    fn json_content_type_detection_accepts_json_suffixes() {
        assert!(buffered_with_content_type(Some("application/json; charset=utf-8")).is_json());
        assert!(buffered_with_content_type(Some("application/problem+json")).is_json());
        assert!(!buffered_with_content_type(Some("text/event-stream")).is_json());
        assert!(!buffered_with_content_type(None).is_json());
    }

    #[tokio::test]
    async fn bytes_with_limit_rejects_oversized_buffered_response() {
        let oversized = Bytes::from(vec![0u8; MAX_RESPONSE_BODY_BYTES + 1]);
        let response =
            ProxyResponse::buffered(http::StatusCode::OK, http::HeaderMap::new(), oversized);

        let result = response.bytes_with_limit(MAX_RESPONSE_BODY_BYTES).await;
        assert!(matches!(result, Err(ProxyError::ResponseBodyTooLarge(_))));
    }

    #[tokio::test]
    async fn bytes_with_limit_rejects_oversized_streamed_response() {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        let response =
            ProxyResponse::streamed(http::StatusCode::OK, http::HeaderMap::new(), stream);

        tokio::spawn(async move {
            let _ = tx.send(Ok(Bytes::from(vec![0u8; 64 * 1024]))).await;
            let _ = tx
                .send(Ok(Bytes::from(vec![0u8; MAX_RESPONSE_BODY_BYTES])))
                .await;
        });

        let result = response.bytes_with_limit(MAX_RESPONSE_BODY_BYTES).await;
        assert!(matches!(result, Err(ProxyError::ResponseBodyTooLarge(_))));
    }

    /// 启动一个最小 HTTP/1.1 服务器：响应 `Content-Length: body_len` 的全零 body，
    /// 分块写出并统计实际写成功的字节数（客户端断开后写入失败即停）。
    async fn spawn_fixed_body_server(
        body_len: usize,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let written = Arc::new(AtomicUsize::new(0));
        let written_report = written.clone();

        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            // 读完请求头（内容不重要）
            let mut buf = [0u8; 4096];
            let mut filled = 0;
            loop {
                if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                let Ok(n) = socket.read(&mut buf[filled..]).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                filled += n;
            }
            let header = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: {body_len}\r\nconnection: close\r\n\r\n"
            );
            if socket.write_all(header.as_bytes()).await.is_err() {
                return;
            }
            let chunk = [0u8; 16 * 1024];
            let mut remaining = body_len;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                if socket.write_all(&chunk[..n]).await.is_err() {
                    break;
                }
                written.fetch_add(n, Ordering::SeqCst);
                remaining -= n;
            }
        });

        (port, written_report)
    }

    /// 客户端断开到服务器写入失败之间有时延（loopback 缓冲区会再吞一部分），
    /// 稍等再读计数。只要客户端真的中途截停，服务器绝不可能写出大半个 body。
    async fn assert_server_aborted_early(
        written: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        body_len: usize,
    ) {
        use std::sync::atomic::Ordering;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let written = written.load(Ordering::SeqCst);
        assert!(
            written < body_len / 2,
            "客户端应在预算耗尽后立即断开，服务器不应写出大部分 body（实际已写 {written}/{body_len} 字节）"
        );
    }

    #[tokio::test]
    async fn bytes_with_limit_aborts_hyper_response_before_full_body_arrives() {
        const BODY_LEN: usize = 16 * 1024 * 1024;
        const LIMIT: usize = 64 * 1024;
        let (port, written) = spawn_fixed_body_server(BODY_LEN).await;

        // 构造真实的 hyper::Response<Incoming>：手工建立 http1 客户端连接
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let io = hyper_util::rt::TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let request = http::Request::builder()
            .uri(format!("http://127.0.0.1:{port}/"))
            .body(http_body_util::Empty::<Bytes>::new())
            .unwrap();
        let response = sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);

        let result = ProxyResponse::Hyper(response).bytes_with_limit(LIMIT).await;
        assert!(matches!(result, Err(ProxyError::ResponseBodyTooLarge(_))));

        // 关键断言：若退回"先 collect 收满再比较"，服务器会把 16 MiB 全部写完
        assert_server_aborted_early(written, BODY_LEN).await;
    }

    #[tokio::test]
    async fn bytes_with_limit_aborts_reqwest_response_before_full_body_arrives() {
        const BODY_LEN: usize = 16 * 1024 * 1024;
        const LIMIT: usize = 64 * 1024;
        let (port, written) = spawn_fixed_body_server(BODY_LEN).await;

        let response = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        let result = ProxyResponse::Reqwest(response)
            .bytes_with_limit(LIMIT)
            .await;
        assert!(matches!(result, Err(ProxyError::ResponseBodyTooLarge(_))));

        assert_server_aborted_early(written, BODY_LEN).await;
    }
}
