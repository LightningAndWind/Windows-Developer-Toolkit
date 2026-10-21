//! JSON 工具状态：文本、缩进、视图模式；实现会话持久化。
//! 树形展开状态（expanded 集合）由 UI 侧持有。

use super::format::{compact, pretty, sort_keys};
use super::parser::parse;
use super::stats::Stats;
use super::{Indent, JsonError, JsonValue, SortMode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonTool {
    pub text: String,
    #[serde(default)]
    pub indent: Indent,
    /// 0=文本视图 1=树视图
    #[serde(default)]
    pub view_mode: u8,
}

impl Default for JsonTool {
    fn default() -> Self {
        Self { text: String::new(), indent: Indent::Two, view_mode: 0 }
    }
}

/// 树节点（扁平化后供 UI 渲染）。
#[derive(Debug, Clone)]
pub struct JsonTreeNodeData {
    pub key: String,
    pub kind: &'static str,
    pub preview: String,
    pub depth: usize,
    pub expanded: bool,
    pub has_children: bool,
    pub child_count: usize,
    /// 该节点在解析树中的索引路径（供展开/折叠与复制子树定位）。
    pub path: Vec<usize>,
}

impl JsonTool {
    pub fn parse(&self) -> Result<JsonValue, JsonError> {
        parse(&self.text)
    }

    /// 当前文本的统计（解析失败返回 None）。
    pub fn stats(&self) -> Option<Stats> {
        self.parse().ok().map(|v| Stats::compute(&v))
    }

    /// 格式化并返回新文本。
    pub fn formatted(&self) -> Result<String, JsonError> {
        Ok(pretty(&self.parse()?, self.indent))
    }

    /// 压缩并返回新文本。
    pub fn compressed(&self) -> Result<String, JsonError> {
        Ok(compact(&self.parse()?))
    }

    /// 键排序并返回新文本。
    pub fn sorted(&self, mode: SortMode) -> Result<String, JsonError> {
        let mut v = self.parse()?;
        sort_keys(&mut v, mode);
        Ok(pretty(&v, self.indent))
    }

    /// 扁平化树（仅展开的容器展开子级）。
    pub fn flatten_tree(&self, expanded: &HashSet<Vec<usize>>) -> Option<Vec<JsonTreeNodeData>> {
        let root = self.parse().ok()?;
        let mut out = Vec::new();
        walk(&root, String::new(), 0, &[], expanded, &mut out);
        Some(out)
    }

    /// 按索引路径取节点（路径来自 flatten_tree，用于「复制该节点」）。
    pub fn value_at(&self, path: &[usize]) -> Option<JsonValue> {
        let root = self.parse().ok()?;
        let mut cur = &root;
        for idx in path {
            cur = match cur {
                JsonValue::Array(items) => items.get(*idx)?,
                JsonValue::Object(entries) => &entries.get(*idx)?.1,
                _ => return None,
            };
        }
        Some(cur.clone())
    }
}

fn walk(
    v: &JsonValue,
    key: String,
    depth: usize,
    path: &[usize],
    expanded: &HashSet<Vec<usize>>,
    out: &mut Vec<JsonTreeNodeData>,
) {
    let (kind, has_children, child_count) = match v {
        JsonValue::Array(items) => ("数组", !items.is_empty(), items.len()),
        JsonValue::Object(entries) => ("对象", !entries.is_empty(), entries.len()),
        _ => (v.type_name(), false, 0),
    };
    let is_open = expanded.contains(&path.to_vec());
    out.push(JsonTreeNodeData {
        preview: preview_of(v),
        key,
        kind,
        depth,
        expanded: has_children && is_open,
        has_children,
        child_count,
        path: path.to_vec(),
    });
    if !is_open {
        return;
    }
    match v {
        JsonValue::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let mut p = path.to_vec();
                p.push(i);
                walk(item, format!("[{i}]"), depth + 1, &p, expanded, out);
            }
        }
        JsonValue::Object(entries) => {
            for (i, (k, val)) in entries.iter().enumerate() {
                let mut p = path.to_vec();
                p.push(i);
                walk(val, k.clone(), depth + 1, &p, expanded, out);
            }
        }
        _ => {}
    }
}

fn preview_of(v: &JsonValue) -> String {
    match v {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(n) => n.clone(),
        JsonValue::String(s) => {
            let t: String = s.chars().take(60).collect();
            let ell = if s.chars().count() > 60 { "…" } else { "" };
            format!("\"{t}\"{ell}")
        }
        JsonValue::Array(items) => format!("[…] {} 项", items.len()),
        JsonValue::Object(entries) => format!("{{…}} {} 项", entries.len()),
    }
}
