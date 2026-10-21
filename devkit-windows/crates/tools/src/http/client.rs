//! HTTP 客户端：reqwest + cookie store + 空闲超时 + 10MB 响应封顶 + 取消。
//! 对应 macOS 端 Tools/HTTP/Logic/HTTPClient.swift。

use super::model::{HTTPRequestModel, HTTPResponseModel, MAX_BODY_BYTES};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum SendError {
    Cancelled,
    Network(String),
    InvalidInput(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Cancelled => write!(f, "请求已取消"),
            SendError::Network(m) => write!(f, "网络错误：{m}"),
            SendError::InvalidInput(m) => write!(f, "{m}"),
        }
    }
}

static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

/// 共享 tokio 运行时（SFTP 等异步后端复用）。
pub fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime 初始化失败")
    })
}

/// 阻塞发送（供后台线程调用）。
pub fn send_blocking(
    req: &HTTPRequestModel,
    cancel: Arc<AtomicBool>,
) -> Result<HTTPResponseModel, SendError> {
    runtime().block_on(send_async(req.clone(), cancel))
}

pub async fn send_async(
    req: HTTPRequestModel,
    cancel: Arc<AtomicBool>,
) -> Result<HTTPResponseModel, SendError> {
    let url = req.effective_url();
    if url.is_empty() {
        return Err(SendError::InvalidInput("URL 不能为空".into()));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(SendError::InvalidInput("URL 必须以 http:// 或 https:// 开头".into()));
    }
    // 空闲超时语义：每收到一段数据就重置计时
    let idle = Duration::from_secs(req.timeout_secs.clamp(1, 3600) as u64);
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .read_timeout(idle)
        .build()
        .map_err(|e| SendError::Network(e.to_string()))?;

    let method = reqwest::Method::from_bytes(req.method.as_bytes())
        .map_err(|_| SendError::InvalidInput(format!("无效的 HTTP 方法：{}", req.method)))?;

    let mut builder = client.request(method, &url);
    for (k, v) in req.final_headers() {
        builder = builder.header(k, v);
    }
    if let Some(ct) = req.suggested_content_type() {
        builder = builder.header("Content-Type", ct);
    }
    let body = req.body_bytes();
    if !body.is_empty() {
        builder = builder.body(body);
    }

    let started = Instant::now();
    // 等待响应头时也要能取消：服务器迟迟不回首字节时，「停止」应立刻生效
    let mut resp = match tokio::select! {
        r = builder.send() => r,
        _ = watch_cancel(&cancel) => return Err(SendError::Cancelled),
    } {
        Ok(r) => r,
        Err(e) => {
            if cancel.load(Ordering::Relaxed) {
                return Err(SendError::Cancelled);
            }
            return Err(SendError::Network(e.to_string()));
        }
    };

    let status_code = resp.status().as_u16();
    let status_text = resp
        .status()
        .canonical_reason()
        .unwrap_or("")
        .to_string();
    let mut headers = Vec::new();
    for (k, v) in resp.headers() {
        if let Ok(v) = v.to_str() {
            headers.push((k.as_str().to_string(), v.to_string()));
        }
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // 流式接收 + 封顶 + 取消检查
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    loop {
        tokio::select! {
            chunk = resp.chunk() => {
                match chunk {
                    Ok(Some(b)) => {
                        if buf.len() + b.len() > MAX_BODY_BYTES {
                            let remain = MAX_BODY_BYTES - buf.len();
                            buf.extend_from_slice(&b[..remain]);
                            truncated = true;
                            break;
                        }
                        buf.extend_from_slice(&b);
                        if cancel.load(Ordering::Relaxed) {
                            return Err(SendError::Cancelled);
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        if cancel.load(Ordering::Relaxed) {
                            return Err(SendError::Cancelled);
                        }
                        return Err(SendError::Network(e.to_string()));
                    }
                }
            }
            _ = watch_cancel(&cancel) => return Err(SendError::Cancelled),
        }
    }

    let body_text = String::from_utf8_lossy(&buf).to_string();
    let size = buf.len();
    Ok(HTTPResponseModel {
        status_code,
        status_text,
        headers,
        body: body_text,
        body_raw: buf,
        duration_ms: started.elapsed().as_millis() as u64,
        size_bytes: size,
        content_type,
        truncated,
    })
}

async fn watch_cancel(cancel: &AtomicBool) {
    let mut iv = tokio::time::interval(Duration::from_millis(100));
    loop {
        iv.tick().await;
        if cancel.load(Ordering::Relaxed) {
            return;
        }
    }
}
