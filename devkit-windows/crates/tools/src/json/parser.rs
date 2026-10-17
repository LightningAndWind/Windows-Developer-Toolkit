//! 手写递归下降 JSON 解析器：错误带精确行列，识别尾逗号与单引号。

use super::{JsonError, JsonValue};

pub fn parse(text: &str) -> Result<JsonValue, JsonError> {
    let mut p = Parser { bytes: text.as_bytes(), pos: 0 };
    p.skip_ws();
    let v = p.parse_value()?;
    p.skip_ws();
    if p.pos < p.bytes.len() {
        return Err(p.err("JSON 文档结束后仍有多余内容"));
    }
    Ok(v)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn line_col(&self, pos: usize) -> (usize, usize) {
        let mut line = 1usize;
        let mut col = 1usize;
        for &b in &self.bytes[..pos.min(self.bytes.len())] {
            if b == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    fn err(&self, msg: &str) -> JsonError {
        let (line, col) = self.line_col(self.pos);
        JsonError { line, col, message: msg.to_string() }
    }

    fn err_at(&self, pos: usize, msg: &str) -> JsonError {
        let (line, col) = self.line_col(pos);
        JsonError { line, col, message: msg.to_string() }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, JsonError> {
        match self.peek() {
            None => Err(self.err("意外的输入结尾，JSON 不完整")),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(JsonValue::String(self.parse_string()?)),
            Some(b'\'') => Err(self.err_at(
                self.pos,
                "JSON 不支持单引号字符串，请使用双引号（\"）",
            )),
            Some(b't') => self.parse_lit("true", JsonValue::Bool(true)),
            Some(b'f') => self.parse_lit("false", JsonValue::Bool(false)),
            Some(b'n') => self.parse_lit("null", JsonValue::Null),
            Some(b'N') => Err(self.err_at(self.pos, "JSON 不支持 NaN，请使用 null")),
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            Some(_) => Err(self.err("意外的字符")),
        }
    }

    fn parse_lit(&mut self, lit: &str, v: JsonValue) -> Result<JsonValue, JsonError> {
        if self.bytes[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            Ok(v)
        } else {
            Err(self.err("无效的字面量"))
        }
    }

    fn parse_number(&mut self) -> Result<JsonValue, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        // 整数部分
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.err("无效的数字")),
        }
        // 小数
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("小数点后缺少数字"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        // 指数
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("指数部分缺少数字"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| self.err("无效的数字"))?
            .to_string();
        Ok(JsonValue::Number(raw))
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        debug_assert_eq!(self.peek(), Some(b'"'));
        let open = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(b) = self.peek() else {
                return Err(self.err_at(open, "字符串缺少闭合引号"));
            };
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let Some(esc) = self.peek() else {
                        return Err(self.err_at(open, "字符串缺少闭合引号"));
                    };
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let cp = self.parse_hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                // 代理对
                                if self.bytes[self.pos..].starts_with(b"\\u") {
                                    self.pos += 2;
                                    let low = self.parse_hex4()?;
                                    if (0xDC00..0xE000).contains(&low) {
                                        let c = 0x10000
                                            + ((cp - 0xD800) << 10)
                                            + (low - 0xDC00);
                                        out.push(
                                            char::from_u32(c)
                                                .ok_or_else(|| self.err("无效的代理对"))?,
                                        );
                                    } else {
                                        return Err(self.err("无效的低代理项"));
                                    }
                                } else {
                                    return Err(self.err("高代理项后缺少低代理项"));
                                }
                            } else if (0xDC00..0xE000).contains(&cp) {
                                return Err(self.err("孤立的高代理项"));
                            } else {
                                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            }
                        }
                        _ => return Err(self.err("无效的转义字符")),
                    }
                }
                0x00..=0x1f => return Err(self.err("字符串中包含未转义的控制字符")),
                _ => {
                    // UTF-8 多字节直接拷贝
                    let len = utf8_len(b);
                    let end = (self.pos + len).min(self.bytes.len());
                    match std::str::from_utf8(&self.bytes[self.pos..end]) {
                        Ok(s) => out.push_str(s),
                        Err(_) => return Err(self.err("无效的 UTF-8 序列")),
                    }
                    self.pos = end;
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, JsonError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(self.err("Unicode 转义不完整"));
        }
        let s = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
            .map_err(|_| self.err("无效的 Unicode 转义"))?;
        let v = u32::from_str_radix(s, 16).map_err(|_| self.err("无效的 Unicode 转义"))?;
        self.pos += 4;
        Ok(v)
    }

    fn parse_array(&mut self) -> Result<JsonValue, JsonError> {
        let open = self.pos;
        self.pos += 1; // [
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JsonValue::Array(items));
        }
        loop {
            self.skip_ws();
            if self.peek() == Some(b']') {
                return Err(self.err_at(self.pos, "数组末尾有多余的逗号（尾逗号）"));
            }
            if self.peek().is_none() {
                return Err(self.err_at(open, "数组缺少闭合的 ]"));
            }
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(JsonValue::Array(items));
                }
                Some(_) => return Err(self.err("数组元素之间缺少逗号")),
                None => return Err(self.err_at(open, "数组缺少闭合的 ]")),
            }
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue, JsonError> {
        let open = self.pos;
        self.pos += 1; // {
        let mut entries: Vec<(String, JsonValue)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonValue::Object(entries));
        }
        loop {
            self.skip_ws();
            if self.peek() == Some(b'}') {
                return Err(self.err_at(self.pos, "对象末尾有多余的逗号（尾逗号）"));
            }
            if self.peek().is_none() {
                return Err(self.err_at(open, "对象缺少闭合的 }"));
            }
            match self.peek() {
                Some(b'"') => {}
                Some(b'\'') => {
                    return Err(self.err_at(self.pos, "键名不支持单引号，请使用双引号"));
                }
                _ => return Err(self.err("对象键名必须是字符串")),
            }
            let key = self.parse_string()?;
            self.skip_ws();
            match self.peek() {
                Some(b':') => self.pos += 1,
                _ => return Err(self.err("键名后缺少冒号")),
            }
            self.skip_ws();
            let value = self.parse_value()?;
            // 重复键：后者覆盖
            if let Some(slot) = entries.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = value;
            } else {
                entries.push((key, value));
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(JsonValue::Object(entries));
                }
                Some(_) => return Err(self.err("对象键值对之间缺少逗号")),
                None => return Err(self.err_at(open, "对象缺少闭合的 }")),
            }
        }
    }
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}
