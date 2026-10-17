//! 终端帧同步：把 TermGrid 内容构建为 Slint 行模型（只更新脏区）。
//! 模型 = [scrollback 尾部窗口行..., 屏幕行...]；滚动进历史的行只构建一次。

use crate::{TermLine, TermSpan};
use devkit_tools::terminal::grid::{Cell, CellColor, ATTR_WIDE_CONT};
use devkit_tools::terminal::AnsiPalette;
use slint::{Color, Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;

/// 模型中镜像的 scrollback 最大行数（完整 3000 行镜像在 pops 时代价高）。
const VIEW_MAX: usize = 600;

pub struct TermFrame {
    pub model: Rc<VecModel<TermLine>>,
    palette: AnsiPalette,
    sb_rows_in_model: usize,
    prev_sb_len: usize,
}

impl TermFrame {
    pub fn new(model: Rc<VecModel<TermLine>>, palette: AnsiPalette) -> Self {
        Self { model, palette, sb_rows_in_model: 0, prev_sb_len: 0 }
    }

    pub fn set_palette(&mut self, p: AnsiPalette) {
        self.palette = p;
        // 外观切换后由上层重建整帧（换新 Frame + 空模型）
        self.sb_rows_in_model = 0;
        self.prev_sb_len = 0;
    }

    /// 与网格同步。调用时机：PTY 有新输出 / resize / 调色板变化。
    pub fn sync(&mut self, grid: &devkit_tools::terminal::grid::TermGrid) {
        let sb_len = grid.scrollback.len();
        let capped = grid.scrollback_pushed > sb_len;

        // 1) scrollback 尾部窗口
        let delta = sb_len.saturating_sub(self.prev_sb_len);
        if delta > 0 {
            if capped {
                // 每次推入伴随一次弹出：窗口整体滑动
                let pops = delta.min(self.sb_rows_in_model);
                for _ in 0..pops {
                    self.model.remove(0);
                }
                self.sb_rows_in_model -= pops;
            }
            let avail = (VIEW_MAX - self.sb_rows_in_model).min(delta);
            let start = sb_len - delta;
            for i in 0..avail {
                let row = &grid.scrollback[start + i];
                let line = build_row(row, &self.palette, None);
                self.model.insert(self.sb_rows_in_model, line);
                self.sb_rows_in_model += 1;
            }
        }
        self.prev_sb_len = sb_len;

        // 2) 屏幕行区
        let target = self.sb_rows_in_model + grid.rows;
        while self.model.row_count() < target {
            self.model.push(TermLine { spans: ModelRc::from(Rc::new(VecModel::default())) });
        }
        while self.model.row_count() > target {
            let n = self.model.row_count();
            self.model.remove(n - 1);
        }
        let cursor_on = grid.cursor_visible;
        for r in 0..grid.rows {
            let cursor_col = if cursor_on && r == grid.cursor_row {
                Some(grid.cursor_col)
            } else {
                None
            };
            let line = build_row(grid.row_view(r), &self.palette, cursor_col);
            self.model.set_row_data(self.sb_rows_in_model + r, line);
        }
    }
}

/// 把一行 cells 合并为同色跨段；行内光标格做反色。
fn build_row(cells: &[Cell], pal: &AnsiPalette, cursor_col: Option<usize>) -> TermLine {
    let mut spans: Vec<TermSpan> = Vec::new();
    // 找最后一个非空（无背景无内容）格，截断尾部空白
    let mut end = cells.len();
    while end > 0 {
        let c = &cells[end - 1];
        let has_cursor = cursor_col == Some(end - 1);
        if c.ch == ' ' && c.bg == CellColor::Default && c.fg == CellColor::Default && !has_cursor {
            end -= 1;
        } else {
            break;
        }
    }

    struct Cur {
        text: String,
        fg: Option<Color>,
        bg: Option<Color>,
        bold: bool,
        underline: bool,
    }
    fn flush(cur: &mut Cur, out: &mut Vec<TermSpan>) {
        if !cur.text.is_empty() {
            out.push(TermSpan {
                text: SharedString::from(cur.text.clone()),
                fg: cur.fg.unwrap_or(Color::from_rgb_u8(0, 0, 0)),
                bg: cur.bg.unwrap_or(Color::from_argb_encoded(0)),
                bold: cur.bold,
                underline: cur.underline,
            });
        }
        cur.text.clear();
    }

    let mut cur = Cur { text: String::new(), fg: None, bg: None, bold: false, underline: false };

    for (i, cell) in cells[..end].iter().enumerate() {
        if cell.attrs & ATTR_WIDE_CONT != 0 {
            continue; // 宽字符续格不单独渲染
        }
        // 解析颜色（含反色与光标反色）
        let cursor_here = cursor_col == Some(i);
        let mut fg = resolve_fg(cell.fg, pal);
        let mut bg = resolve_bg(cell.bg, pal);
        let mut bold = cell.attrs & 1 != 0;
        let underline = cell.attrs & 4 != 0;
        if cell.attrs & 8 != 0 {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cursor_here {
            let cf = Color::from_rgb_u8(pal.cursor[0], pal.cursor[1], pal.cursor[2]);
            let cb = fg;
            fg = cb;
            bg = Some(cf);
            bold = true;
        }
        let fg = fg.unwrap_or_else(|| {
            Color::from_rgb_u8(pal.default_fg[0], pal.default_fg[1], pal.default_fg[2])
        });
        let bg = bg.unwrap_or(Color::from_argb_encoded(0));

        let same_style = cur.fg == Some(fg) && cur.bg == Some(bg) && cur.bold == bold
            && cur.underline == underline;
        if !same_style {
            flush(&mut cur, &mut spans);
            cur.fg = Some(fg);
            cur.bg = Some(bg);
            cur.bold = bold;
            cur.underline = underline;
        }
        cur.text.push(cell.ch);
    }
    flush(&mut cur, &mut spans);

    TermLine { spans: ModelRc::from(Rc::new(VecModel::from(spans))) }
}

fn indexed(idx: u8, pal: &AnsiPalette) -> Color {
    let rgb = match idx {
        0..=15 => pal.colors[idx as usize],
        16..=231 => {
            let v = idx as u32 - 16;
            let r = (v / 36) as u8;
            let g = ((v % 36) / 6) as u8;
            let b = (v % 6) as u8;
            let conv = |c: u8| if c == 0 { 0 } else { 55 + 40 * (c - 1) };
            [conv(r), conv(g), conv(b)]
        }
        _ => {
            let gray = 8 + 10 * (idx as u32 - 232);
            [gray as u8, gray as u8, gray as u8]
        }
    };
    Color::from_rgb_u8(rgb[0], rgb[1], rgb[2])
}

fn resolve_fg(c: CellColor, pal: &AnsiPalette) -> Option<Color> {
    match c {
        CellColor::Default => None,
        CellColor::Indexed(i) => Some(indexed(i, pal)),
        CellColor::Rgb(r, g, b) => Some(Color::from_rgb_u8(r, g, b)),
    }
}

fn resolve_bg(c: CellColor, pal: &AnsiPalette) -> Option<Color> {
    resolve_fg(c, pal)
}
