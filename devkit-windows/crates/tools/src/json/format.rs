//! JSON 格式化：美化 / 压缩 / 键排序。

use super::{Indent, JsonValue, SortMode};

/// 美化输出（键序保持原样）。
pub fn pretty(v: &JsonValue, indent: Indent) -> String {
    let mut out = String::new();
    write_pretty(&mut out, v, indent, 0);
    out
}

fn write_pretty(out: &mut String, v: &JsonValue, indent: Indent, depth: usize) {
    match v {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        JsonValue::Number(n) => out.push_str(n),
        JsonValue::String(s) => write_string(out, s),
        JsonValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                push_indent(out, indent, depth + 1);
                write_pretty(out, item, indent, depth + 1);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, indent, depth);
            out.push(']');
        }
        JsonValue::Object(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (k, val)) in entries.iter().enumerate() {
                push_indent(out, indent, depth + 1);
                write_string(out, k);
                out.push_str(": ");
                write_pretty(out, val, indent, depth + 1);
                if i + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, indent, depth);
            out.push('}');
        }
    }
}

fn push_indent(out: &mut String, indent: Indent, depth: usize) {
    for _ in 0..depth {
        out.push_str(indent.text());
    }
}

/// 压缩（单行）。
pub fn compact(v: &JsonValue) -> String {
    let mut out = String::new();
    write_compact(&mut out, v);
    out
}

fn write_compact(out: &mut String, v: &JsonValue) {
    match v {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        JsonValue::Number(n) => out.push_str(n),
        JsonValue::String(s) => write_string(out, s),
        JsonValue::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact(out, item);
            }
            out.push(']');
        }
        JsonValue::Object(entries) => {
            out.push('{');
            for (i, (k, val)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, k);
                out.push(':');
                write_compact(out, val);
            }
            out.push('}');
        }
    }
}

/// 键排序。
pub fn sort_keys(v: &mut JsonValue, mode: SortMode) {
    let recursive = matches!(mode, SortMode::AscRecursive | SortMode::DescRecursive);
    let desc = matches!(mode, SortMode::DescRecursive | SortMode::DescTop);
    sort_inner(v, recursive, desc);
}

fn sort_inner(v: &mut JsonValue, recursive: bool, desc: bool) {
    match v {
        JsonValue::Array(items) => {
            if recursive {
                for item in items {
                    sort_inner(item, recursive, desc);
                }
            }
        }
        JsonValue::Object(entries) => {
            if recursive {
                for (_, val) in entries.iter_mut() {
                    sort_inner(val, recursive, desc);
                }
            }
            if desc {
                entries.sort_by(|a, b| b.0.cmp(&a.0));
            } else {
                entries.sort_by(|a, b| a.0.cmp(&b.0));
            }
        }
        _ => {}
    }
}

/// JSON 字符串转义（含 CJK 原样输出，不转 \u）。
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
