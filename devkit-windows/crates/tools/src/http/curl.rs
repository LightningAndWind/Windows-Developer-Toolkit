//! cURL 导出：把当前请求导出为可粘贴执行的 curl 命令。

use super::model::{BodyKind, HTTPRequestModel};

pub fn to_curl(req: &HTTPRequestModel) -> String {
    let mut parts: Vec<String> = vec!["curl".into()];
    let method = if req.method == "GET" { "" } else { req.method.as_str() };
    if !method.is_empty() {
        parts.push("-X".into());
        parts.push(quote(method));
    }
    parts.push(quote(&req.effective_url()));
    for (k, v) in req.final_headers() {
        parts.push("-H".into());
        parts.push(quote(&format!("{k}: {v}")));
    }
    match req.body.kind {
        BodyKind::None => {}
        BodyKind::Json => {
            parts.push("--data-raw".into());
            parts.push(quote(&req.body.text));
        }
        BodyKind::Raw => {
            parts.push("--data-raw".into());
            parts.push(quote(&req.body.text));
        }
        BodyKind::Form => {
            for kv in &req.body.form {
                if kv.enabled && !kv.key.trim().is_empty() {
                    parts.push("--data-urlencode".into());
                    parts.push(quote(&format!("{}={}", kv.key.trim(), kv.value)));
                }
            }
        }
    }
    parts.join(" ")
}

/// POSIX 单引号转义。
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
