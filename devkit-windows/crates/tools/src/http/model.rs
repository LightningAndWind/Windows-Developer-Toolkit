//! HTTP 请求/响应数据模型。对应 macOS 端 Tools/HTTP/Models。

use serde::{Deserialize, Serialize};

/// 键值对（Params / Headers 表格行，支持单行启用禁用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HTTPKV {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Body 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BodyKind {
    #[default]
    None,
    Json,
    Form,
    Raw,
}

/// 认证类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AuthKind {
    #[default]
    None,
    Bearer,
    Basic,
}

/// 请求体。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HTTPBody {
    pub kind: BodyKind,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub form: Vec<HTTPKV>,
}

/// 认证配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HTTPAuth {
    pub kind: AuthKind,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

/// 一个 HTTP 请求（整条序列化存 SQLite）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HTTPRequestModel {
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub params: Vec<HTTPKV>,
    #[serde(default)]
    pub headers: Vec<HTTPKV>,
    #[serde(default)]
    pub body: HTTPBody,
    #[serde(default)]
    pub auth: HTTPAuth,
    /// 空闲超时（秒）：等待下一段响应数据的最长时间；1-3600，默认 30。
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,
}

fn default_timeout() -> u32 {
    30
}

impl Default for HTTPRequestModel {
    fn default() -> Self {
        Self {
            method: "GET".into(),
            url: String::new(),
            params: Vec::new(),
            headers: Vec::new(),
            body: HTTPBody::default(),
            auth: HTTPAuth::default(),
            timeout_secs: 30,
        }
    }
}

/// 响应模型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HTTPResponseModel {
    pub status_code: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    /// 以 UTF-8 lossy 解码的响应体。
    pub body: String,
    /// 原始字节（预览图片等二进制用；不入库）。
    #[serde(skip, default)]
    pub body_raw: Vec<u8>,
    pub duration_ms: u64,
    pub size_bytes: usize,
    pub content_type: String,
    /// 超过 10MB 封顶被截断。
    pub truncated: bool,
}

impl HTTPRequestModel {
    /// 启用的 headers。
    pub fn effective_headers(&self) -> Vec<(String, String)> {
        self.headers
            .iter()
            .filter(|kv| kv.enabled && !kv.key.trim().is_empty())
            .map(|kv| (kv.key.trim().to_string(), kv.value.clone()))
            .collect()
    }

    /// 由认证配置派生的 Authorization 头（预览用）。
    pub fn auth_header(&self) -> Option<(String, String)> {
        match self.auth.kind {
            AuthKind::None => None,
            AuthKind::Bearer => {
                if self.auth.token.trim().is_empty() {
                    None
                } else {
                    Some(("Authorization".into(), format!("Bearer {}", self.auth.token.trim())))
                }
            }
            AuthKind::Basic => {
                use base64::Engine;
                let raw = format!("{}:{}", self.auth.username, self.auth.password);
                Some((
                    "Authorization".into(),
                    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(raw)),
                ))
            }
        }
    }

    /// 最终请求头（启用 headers + Authorization）。
    pub fn final_headers(&self) -> Vec<(String, String)> {
        let mut out = self.effective_headers();
        if let Some(a) = self.auth_header() {
            out.retain(|(k, _)| !k.eq_ignore_ascii_case("Authorization"));
            out.push(a);
        }
        out
    }

    /// 合成最终 URL：启用参数覆盖同名 query，禁用参数被移除。
    pub fn effective_url(&self) -> String {
        let base = self.url.trim().to_string();
        if base.is_empty() {
            return base;
        }
        let (scheme_host_path, existing_query) = match base.split_once('?') {
            Some((a, b)) => (a.to_string(), Some(b.to_string())),
            None => (base.clone(), None),
        };
        // 已有 query 解析为键值序列
        let mut pairs: Vec<(String, Option<String>)> = Vec::new();
        if let Some(q) = existing_query {
            for pair in q.split('&') {
                if pair.is_empty() {
                    continue;
                }
                match pair.split_once('=') {
                    Some((k, v)) => pairs.push((k.to_string(), Some(v.to_string()))),
                    None => pairs.push((pair.to_string(), None)),
                }
            }
        }
        // 应用启用/禁用参数
        for kv in &self.params {
            let key = kv.key.trim();
            if key.is_empty() {
                continue;
            }
            if kv.enabled {
                if let Some(slot) = pairs.iter_mut().find(|(k, _)| k == key) {
                    slot.1 = Some(kv.value.clone());
                } else {
                    pairs.push((key.to_string(), Some(kv.value.clone())));
                }
            } else {
                pairs.retain(|(k, _)| k != key);
            }
        }
        if pairs.is_empty() {
            return scheme_host_path;
        }
        let query = pairs
            .iter()
            .map(|(k, v)| match v {
                Some(v) => format!("{}={}", encode_query(k), encode_query(v)),
                None => encode_query(k),
            })
            .collect::<Vec<_>>()
            .join("&");
        format!("{scheme_host_path}?{query}")
    }

    /// 最终请求体字节。
    pub fn body_bytes(&self) -> Vec<u8> {
        match self.body.kind {
            BodyKind::None => Vec::new(),
            BodyKind::Json => self.body.text.clone().into_bytes(),
            BodyKind::Raw => self.body.text.clone().into_bytes(),
            BodyKind::Form => {
                let pairs: Vec<String> = self
                    .body
                    .form
                    .iter()
                    .filter(|kv| kv.enabled && !kv.key.trim().is_empty())
                    .map(|kv| {
                        format!(
                            "{}={}",
                            encode_query(kv.key.trim()),
                            encode_query(&kv.value)
                        )
                    })
                    .collect();
                pairs.join("&").into_bytes()
            }
        }
    }

    /// Content-Type 建议（未显式设置时）。
    pub fn suggested_content_type(&self) -> Option<&'static str> {
        let has_ct = self
            .effective_headers()
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("Content-Type"));
        if has_ct {
            return None;
        }
        match self.body.kind {
            BodyKind::Json => Some("application/json"),
            BodyKind::Form => Some("application/x-www-form-urlencoded"),
            _ => None,
        }
    }
}

/// query 编码（保留少量安全字符，空格转 %20）。
pub fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 历史记录摘要（列表页轻量行，不读响应/请求体）。
#[derive(Debug, Clone)]
pub struct HistorySummary {
    pub id: String,
    pub created_at: f64,
    pub method: String,
    pub url: String,
    pub status_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub size_bytes: Option<i64>,
    pub saved_request_id: Option<String>,
}

/// 响应体封顶。
pub const MAX_BODY_BYTES: usize = 10_000_000;
