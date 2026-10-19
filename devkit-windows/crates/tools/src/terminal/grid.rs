//! 终端屏幕网格：cells + 光标 + 滚动区域 + scrollback 环形缓冲。
//! 实现 vte::Perform 接收解析后的 ANSI 事件。

use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;

pub const MAX_SCROLLBACK: usize = 3000;

/// 单元格前景/背景色。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CellColor {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// 单元格属性位。
pub const ATTR_BOLD: u8 = 1;
pub const ATTR_ITALIC: u8 = 2;
pub const ATTR_UNDERLINE: u8 = 4;
pub const ATTR_INVERSE: u8 = 8;
/// 宽字符的续格（不可见占位）。
pub const ATTR_WIDE_CONT: u8 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: CellColor,
    pub bg: CellColor,
    pub attrs: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', fg: CellColor::Default, bg: CellColor::Default, attrs: 0 }
    }
}

impl Cell {
    fn is_blank(&self) -> bool {
        self.ch == ' ' && self.fg == CellColor::Default && self.bg == CellColor::Default
    }
}

/// 终端屏幕网格。
pub struct TermGrid {
    pub cols: usize,
    pub rows: usize,
    /// 行主序：rows * cols。
    pub cells: Vec<Cell>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    saved_cursor: (usize, usize),
    scroll_top: usize,
    scroll_bot: usize,
    cur_fg: CellColor,
    cur_bg: CellColor,
    cur_attrs: u8,
    /// 回滚缓冲（环形，超出上限丢最旧）。
    pub scrollback: VecDeque<Vec<Cell>>,
    /// 累计推入 scrollback 的总行数（判断是否已到上限：> len 即已弹出过）。
    pub scrollback_pushed: usize,
    pub title: String,
    /// 换行模式（LNM）：收到 CR/LF 时是否同时回车。多数场景关闭。
    pub autowrap: bool,
    /// 光标可见性（DECTCEM）。
    pub cursor_visible: bool,
    /// 脏标记：有内容变化待渲染。
    pub dirty: bool,
}

impl TermGrid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let mut g = Self {
            cols: 1,
            rows: 1,
            cells: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor: (0, 0),
            scroll_top: 0,
            scroll_bot: 0,
            cur_fg: CellColor::Default,
            cur_bg: CellColor::Default,
            cur_attrs: 0,
            scrollback: VecDeque::new(),
            scrollback_pushed: 0,
            title: String::new(),
            autowrap: true,
            cursor_visible: true,
            dirty: true,
        };
        g.resize(cols, rows.max(1));
        g
    }

    /// 重设行列尺寸：保留已写入内容（按左上对齐裁剪/补空），scrollback 保留。
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(2);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let mut new_cells = vec![Cell::default(); cols * rows];
        // 构造期 cells 尚未分配（cols/rows 的 1 只是占位，实际尺寸由本次 resize 落定），
        // 无旧内容可保留，直接跳过拷贝；否则会按旧尺寸索引空 Vec 越界 panic。
        if !self.cells.is_empty() {
            for r in 0..rows.min(self.rows) {
                for c in 0..cols.min(self.cols) {
                    new_cells[r * cols + c] = self.cells[r * self.cols + c];
                }
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = new_cells;
        self.cursor_row = self.cursor_row.min(rows - 1);
        self.cursor_col = self.cursor_col.min(cols - 1);
        self.scroll_top = 0;
        self.scroll_bot = rows - 1;
        self.dirty = true;
    }

    #[inline]
    fn row_mut(&mut self, row: usize) -> &mut [Cell] {
        let start = row * self.cols;
        &mut self.cells[start..start + self.cols]
    }

    #[inline]
    fn row(&self, row: usize) -> &[Cell] {
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }

    /// 公开只读行访问。
    #[inline]
    pub fn row_view(&self, row: usize) -> &[Cell] {
        self.row(row)
    }

    /// 把屏幕第 row 行送入 scrollback。
    pub fn push_to_scrollback(&mut self, row: usize) {
        let cells = self.row(row).to_vec();
        if self.scrollback.len() >= MAX_SCROLLBACK {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(cells);
        self.scrollback_pushed += 1;
    }

    /// 屏幕整体下滚 n 行（顶部 n 行进 scrollback，底部补空行）。
    pub fn scroll_up(&mut self, n: usize) {
        let n = n.max(1).min(self.scroll_bot - self.scroll_top + 1);
        for _ in 0..n {
            let top = self.scroll_top;
            if top == 0 {
                self.push_to_scrollback(0);
            }
            // 行整体上移
            for r in top..self.scroll_bot {
                let (a, b) = self.cells.split_at_mut((r + 1) * self.cols);
                a[r * self.cols..(r + 1) * self.cols].swap_with_slice(&mut b[0..self.cols]);
            }
            let bot = self.scroll_bot;
            self.row_mut(bot).fill(Cell::default());
        }
        self.dirty = true;
    }

    /// 屏幕整体上滚 n 行（底部补空行，不动 scrollback）。
    pub fn scroll_down(&mut self, n: usize) {
        let n = n.max(1).min(self.scroll_bot - self.scroll_top + 1);
        for _ in 0..n {
            for r in (self.scroll_top..self.scroll_bot).rev() {
                let (a, b) = self.cells.split_at_mut((r + 1) * self.cols);
                a[r * self.cols..(r + 1) * self.cols].swap_with_slice(&mut b[0..self.cols]);
            }
            let top = self.scroll_top;
            self.row_mut(top).fill(Cell::default());
        }
        self.dirty = true;
    }

    pub fn linefeed(&mut self) {
        if self.cursor_row == self.scroll_bot {
            self.scroll_up(1);
        } else if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        }
        self.dirty = true;
    }

    pub fn reverse_index(&mut self) {
        if self.cursor_row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
        }
        self.dirty = true;
    }

    pub fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        }
    }

    /// 水平制表：默认 8 列。
    pub fn tab(&mut self) {
        let next = (self.cursor_col / 8 + 1) * 8;
        self.cursor_col = next.min(self.cols - 1);
    }

    /// 写入一个字符（处理宽字符续格与自动换行）。
    pub fn put_char(&mut self, ch: char) {
        if ch == '\0' {
            return;
        }
        let w = ch.width().unwrap_or(1).max(1) as usize;
        // 自动换行
        if self.cursor_col + w > self.cols {
            if self.autowrap {
                self.carriage_return();
                self.linefeed();
            } else {
                self.cursor_col = self.cols - w.min(self.cols);
            }
        }
        let idx = self.cursor_row * self.cols + self.cursor_col;
        self.cells[idx] =
            Cell { ch, fg: self.cur_fg, bg: self.cur_bg, attrs: self.cur_attrs };
        if w == 2 && self.cursor_col + 1 < self.cols {
            // 宽字符续格
            self.cells[idx + 1] =
                Cell { ch: ' ', fg: self.cur_fg, bg: self.cur_bg, attrs: self.cur_attrs | ATTR_WIDE_CONT };
        }
        // 光标前进（跳过续格）
        self.cursor_col += w;
        if self.cursor_col >= self.cols {
            self.cursor_col = self.cols - 1;
        }
        self.dirty = true;
    }

    pub fn cursor_to(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.rows - 1);
        self.cursor_col = col.min(self.cols - 1);
        self.dirty = true;
    }

    pub fn cursor_up(&mut self, n: usize) {
        let floor = self.scroll_top;
        self.cursor_row = self.cursor_row.saturating_sub(n).max(floor);
        self.dirty = true;
    }

    pub fn cursor_down(&mut self, n: usize) {
        let ceil = self.scroll_bot;
        self.cursor_row = (self.cursor_row + n).min(ceil).min(self.rows - 1);
        self.dirty = true;
    }

    pub fn cursor_forward(&mut self, n: usize) {
        self.cursor_col = (self.cursor_col + n).min(self.cols - 1);
        self.dirty = true;
    }

    pub fn cursor_back(&mut self, n: usize) {
        self.cursor_col = self.cursor_col.saturating_sub(n);
        self.dirty = true;
    }

    /// 清屏：mode 0 光标以下、1 光标以上、2 全部（3 同 2 并清 scrollback）。
    pub fn erase_in_display(&mut self, mode: u8) {
        match mode {
            0 => {
                self.erase_in_line(0);
                let start = (self.cursor_row + 1) * self.cols;
                self.cells[start..].fill(Cell::default());
            }
            1 => {
                self.cells[..self.cursor_row * self.cols + self.cursor_col + 1]
                    .fill(Cell::default());
            }
            _ => {
                self.cells.fill(Cell::default());
                if mode == 3 {
                    self.scrollback.clear();
                }
            }
        }
        self.dirty = true;
    }

    /// 清行：mode 0 光标起、1 光标止、2 整行。
    pub fn erase_in_line(&mut self, mode: u8) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let cols = self.cols;
        match mode {
            0 => {
                let r = self.row_mut(row);
                for c in r.iter_mut().take(cols).skip(col) {
                    *c = Cell::default();
                }
            }
            1 => {
                let r = self.row_mut(row);
                for c in r.iter_mut().take(col + 1) {
                    *c = Cell::default();
                }
            }
            _ => {
                self.row_mut(row).fill(Cell::default());
            }
        }
        self.dirty = true;
    }

    pub fn set_scroll_region(&mut self, top: usize, bot: usize) {
        let bot = bot.min(self.rows - 1);
        if top < bot {
            self.scroll_top = top;
            self.scroll_bot = bot;
        }
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor = (self.cursor_row, self.cursor_col);
    }

    pub fn restore_cursor(&mut self) {
        let (r, c) = self.saved_cursor;
        self.cursor_row = r.min(self.rows - 1);
        self.cursor_col = c.min(self.cols - 1);
        self.dirty = true;
    }

    pub fn set_fg(&mut self, c: CellColor) {
        self.cur_fg = c;
    }

    pub fn set_bg(&mut self, c: CellColor) {
        self.cur_bg = c;
    }

    pub fn set_attrs(&mut self, f: impl FnOnce(&mut u8)) {
        f(&mut self.cur_attrs);
    }

    pub fn attrs(&self) -> u8 {
        self.cur_attrs
    }

    /// 插入 n 个空行（光标行起，滚动区域内，向下挤）。
    pub fn insert_lines(&mut self, n: usize) {
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bot {
            return;
        }
        let n = n.max(1).min(self.scroll_bot - self.cursor_row + 1);
        for _ in 0..n {
            for r in (self.cursor_row..self.scroll_bot).rev() {
                let (a, b) = self.cells.split_at_mut((r + 1) * self.cols);
                a[r * self.cols..(r + 1) * self.cols].swap_with_slice(&mut b[0..self.cols]);
            }
            let bot = self.scroll_bot;
            self.row_mut(bot).fill(Cell::default());
        }
        self.dirty = true;
    }

    /// 删除 n 行（光标行起，向上挤，底部补空）。
    pub fn delete_lines(&mut self, n: usize) {
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bot {
            return;
        }
        let n = n.max(1).min(self.scroll_bot - self.cursor_row + 1);
        for _ in 0..n {
            for r in self.cursor_row..self.scroll_bot {
                let (a, b) = self.cells.split_at_mut((r + 1) * self.cols);
                a[r * self.cols..(r + 1) * self.cols].swap_with_slice(&mut b[0..self.cols]);
            }
            let bot = self.scroll_bot;
            self.row_mut(bot).fill(Cell::default());
        }
        self.dirty = true;
    }

    /// 光标处插入 n 个空格。
    pub fn insert_chars(&mut self, n: usize) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let cols = self.cols;
        let n = n.max(1).min(cols - col);
        let r = self.row_mut(row);
        for i in (col..cols - n).rev() {
            r[i + n] = r[i];
        }
        for c in r.iter_mut().take(col + n).skip(col) {
            *c = Cell::default();
        }
        self.dirty = true;
    }

    /// 光标处删除 n 个字符（左侧补空）。
    pub fn delete_chars(&mut self, n: usize) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let cols = self.cols;
        let n = n.max(1).min(cols - col);
        let r = self.row_mut(row);
        for i in col..cols - n {
            r[i] = r[i + n];
        }
        for c in r.iter_mut().take(cols).skip(cols - n) {
            *c = Cell::default();
        }
        self.dirty = true;
    }

    /// 全量重置（RIS）。
    pub fn reset(&mut self) {
        let (cols, rows) = (self.cols, self.rows);
        *self = TermGrid::new(cols, rows);
    }

    /// 行内容是否全空白。
    pub fn row_is_blank(&self, row: usize) -> bool {
        self.row(row).iter().all(Cell::is_blank)
    }
}

/// vte Perform 实现：把解析事件映射到网格操作。
pub struct Perform<'a>(pub &'a mut TermGrid);

impl vte::Perform for Perform<'_> {
    fn print(&mut self, c: char) {
        self.0.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.0.carriage_return(),
            b'\n' | 0x0b | 0x0c => self.0.linefeed(),
            b'\t' => self.0.tab(),
            0x08 => self.0.backspace(),
            0x07 => {} // BEL
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let g = &mut *self.0;
        let private = _intermediates.first() == Some(&b'?');
        let p: Vec<u16> = params.iter().map(|s| s.first().copied().unwrap_or(0)).collect();
        let p0 = |i: usize, def: u16| -> u16 {
            p.get(i).copied().filter(|&v| v != 0).unwrap_or(def)
        };
        match action {
            'm' => sgr(g, &p),
            'A' => g.cursor_up(p0(0, 1) as usize),
            'B' | 'e' => g.cursor_down(p0(0, 1) as usize),
            'C' | 'a' => g.cursor_forward(p0(0, 1) as usize),
            'D' => g.cursor_back(p0(0, 1) as usize),
            'E' => {
                g.cursor_down(p0(0, 1) as usize);
                g.carriage_return();
            }
            'F' => {
                g.cursor_up(p0(0, 1) as usize);
                g.carriage_return();
            }
            'G' | '`' => {
                let col = (p0(0, 1) as usize).saturating_sub(1);
                g.cursor_col = col.min(g.cols - 1);
                g.dirty = true;
            }
            'H' | 'f' => {
                let row = (p0(0, 1) as usize).saturating_sub(1);
                let col = (p0(1, 1) as usize).saturating_sub(1);
                g.cursor_to(row, col);
            }
            'd' => {
                let row = (p0(0, 1) as usize).saturating_sub(1);
                g.cursor_row = row.min(g.rows - 1);
                g.dirty = true;
            }
            'J' => g.erase_in_display(p.first().copied().unwrap_or(0) as u8),
            'K' => g.erase_in_line(p.first().copied().unwrap_or(0) as u8),
            'L' => g.insert_lines(p0(0, 1) as usize),
            'M' => g.delete_lines(p0(0, 1) as usize),
            'P' => g.delete_chars(p0(0, 1) as usize),
            '@' => g.insert_chars(p0(0, 1) as usize),
            'X' => {
                let n = p0(0, 1) as usize;
                let row = g.cursor_row;
                let col = g.cursor_col;
                let cols = g.cols;
                let r = g.row_mut(row);
                for c in r.iter_mut().take((col + n).min(cols)).skip(col) {
                    *c = Cell::default();
                }
                g.dirty = true;
            }
            'S' => g.scroll_up(p0(0, 1) as usize),
            'T' => g.scroll_down(p0(0, 1) as usize),
            'r' => {
                let top = (p0(0, 1) as usize).saturating_sub(1);
                let bot = (p0(1, g.rows as u16) as usize).saturating_sub(1);
                g.set_scroll_region(top, bot);
                g.cursor_to(0, 0);
            }
            's' => g.save_cursor(),
            'u' => g.restore_cursor(),
            'h' | 'l' => {
                if private {
                    let set = action == 'h';
                    for &v in &p {
                        match v {
                            1049 | 47 => {
                                if set {
                                    g.save_cursor();
                                    g.erase_in_display(2);
                                } else {
                                    g.restore_cursor();
                                }
                            }
                            25 => g.cursor_visible = set,
                            7 => g.autowrap = set,
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, action: u8) {
        let g = &mut *self.0;
        match action {
            b'D' => g.linefeed(),
            b'E' => {
                g.carriage_return();
                g.linefeed();
            }
            b'M' => g.reverse_index(),
            b'c' => g.reset(),
            b'7' => g.save_cursor(),
            b'8' => g.restore_cursor(),
            b'(' | b')' | b'*' | b'+' | b'#' => {} // 字符集：忽略
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 0/2 = 窗口标题
        if params.len() >= 2 {
            let code = params[0].first().copied().unwrap_or(255);
            if code == 0 || code == 2 {
                if let Ok(s) = std::str::from_utf8(params[1]) {
                    self.0.title = s.to_string();
                }
            }
        }
    }
}

/// SGR（设置图形属性）。
fn sgr(g: &mut TermGrid, p: &[u16]) {
    if p.is_empty() {
        g.set_fg(CellColor::Default);
        g.set_bg(CellColor::Default);
        g.set_attrs(|a| *a = 0);
        return;
    }
    let mut i = 0;
    while i < p.len() {
        let v = p[i];
        match v {
            0 => {
                g.set_fg(CellColor::Default);
                g.set_bg(CellColor::Default);
                g.set_attrs(|a| *a = 0);
            }
            1 => g.set_attrs(|a| *a |= ATTR_BOLD),
            2 => {} // dim：忽略
            3 => g.set_attrs(|a| *a |= ATTR_ITALIC),
            4 => g.set_attrs(|a| *a |= ATTR_UNDERLINE),
            7 => g.set_attrs(|a| *a |= ATTR_INVERSE),
            22 => g.set_attrs(|a| *a &= !(ATTR_BOLD)),
            23 => g.set_attrs(|a| *a &= !(ATTR_ITALIC)),
            24 => g.set_attrs(|a| *a &= !(ATTR_UNDERLINE)),
            27 => g.set_attrs(|a| *a &= !(ATTR_INVERSE)),
            30..=37 => g.set_fg(CellColor::Indexed((v - 30) as u8)),
            38 => {
                i += parse_color_ext(g, &p[i..], true);
            }
            39 => g.set_fg(CellColor::Default),
            40..=47 => g.set_bg(CellColor::Indexed((v - 40) as u8)),
            48 => {
                i += parse_color_ext(g, &p[i..], false);
            }
            49 => g.set_bg(CellColor::Default),
            90..=97 => g.set_fg(CellColor::Indexed((v - 90 + 8) as u8)),
            100..=107 => g.set_bg(CellColor::Indexed((v - 100 + 8) as u8)),
            _ => {}
        }
        i += 1;
    }
}

/// 解析 256 色 / 真彩色扩展参数，返回消耗的额外参数个数。
fn parse_color_ext(g: &mut TermGrid, p: &[u16], fg: bool) -> usize {
    match p.get(1).copied() {
        Some(5) => {
            if let Some(c) = p.get(2).copied() {
                let idx = (c as u8).min(255);
                let color = CellColor::Indexed(idx);
                if fg {
                    g.set_fg(color);
                } else {
                    g.set_bg(color);
                }
            }
            2
        }
        Some(2) => {
            if let (Some(r), Some(gc), Some(b)) = (p.get(2), p.get(3), p.get(4)) {
                let color = CellColor::Rgb(*r as u8, *gc as u8, *b as u8);
                if fg {
                    g.set_fg(color);
                } else {
                    g.set_bg(color);
                }
                4
            } else {
                0
            }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalSession;

    /// 回归：TermGrid::new 经 resize 从空 cells 拷贝，不得越界（修复前必 panic）。
    #[test]
    fn new_allocates_grid_without_panic() {
        let g = TermGrid::new(100, 30);
        assert_eq!((g.cols, g.rows), (100, 30));
        assert_eq!(g.cells.len(), 100 * 30);
        assert_eq!(g.row_view(0).len(), 100);
    }

    /// resize 保留旧内容（放大 / 缩小两个方向），且空网格 resize 安全。
    #[test]
    fn resize_preserves_content() {
        let mut g = TermGrid::new(10, 4);
        g.cursor_to(1, 0);
        g.put_char('A');
        g.resize(20, 6);
        assert_eq!((g.cols, g.rows), (20, 6));
        assert_eq!(g.row_view(1)[0].ch, 'A');
        g.resize(5, 3);
        assert_eq!((g.cols, g.rows), (5, 3));
        assert_eq!(g.row_view(1)[0].ch, 'A');
    }

    /// 会话级冒烟：解析并渲染一段带 CR/LF 的文本。
    #[test]
    fn session_feed_smoke() {
        let mut s = TerminalSession::new(80, 24);
        s.feed(b"hello\r\nworld");
        assert_eq!(s.grid.row_view(0)[0].ch, 'h');
        assert_eq!(s.grid.row_view(1)[0].ch, 'w');
        assert_eq!((s.grid.cursor_row, s.grid.cursor_col), (1, 5));
    }
}
