//! JSON 工具逻辑：手写递归下降解析器（精确行列 + 尾逗号/单引号诊断）、
//! 格式化/压缩、键排序、统计。对应 macOS 端 Tools/JSON/Logic。

pub mod format;
pub mod parser;
pub mod stats;
pub mod tool;

use serde::{Deserialize, Serialize};

/// JSON 值。对象保持键序（Vec），数字保留原始文本以便往返。
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    /// 保留原始字面量（如 1.50、1e3），格式化时原样输出。
    Number(String),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            JsonValue::Null => "空",
            JsonValue::Bool(_) => "布尔",
            JsonValue::Number(_) => "数字",
            JsonValue::String(_) => "字符串",
            JsonValue::Array(_) => "数组",
            JsonValue::Object(_) => "对象",
        }
    }
}

/// 缩进风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Indent {
    #[default]
    Two,
    Four,
    Tab,
}

impl Indent {
    pub fn text(&self) -> &'static str {
        match self {
            Indent::Two => "  ",
            Indent::Four => "    ",
            Indent::Tab => "\t",
        }
    }
}

/// 解析错误（含行列）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "第 {} 行第 {} 列：{}", self.line, self.col, self.message)
    }
}

/// 键排序模式：升/降 × 递归/仅顶层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    AscRecursive,
    DescRecursive,
    AscTop,
    DescTop,
}
