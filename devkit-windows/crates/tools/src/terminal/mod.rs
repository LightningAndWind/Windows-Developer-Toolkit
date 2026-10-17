//! 终端子系统：屏幕网格 + ANSI/VT 解析 + ConPTY 会话。
//! 数据真源在 Rust；UI 侧仅镜像可见行。

pub mod grid;
pub mod pty;

pub use grid::{Cell, CellColor, TermGrid, MAX_SCROLLBACK};
pub use pty::PtySession;

/// 一个终端会话 = PTY + 解析器 + 网格。
pub struct TerminalSession {
    pub grid: TermGrid,
    parser: vte::Parser,
}

impl TerminalSession {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            grid: TermGrid::new(cols, rows),
            parser: vte::Parser::new(),
        }
    }

    /// 喂入 PTY 原始字节。
    pub fn feed(&mut self, bytes: &[u8]) {
        let grid = &mut self.grid;
        let mut performer = grid::Perform(grid);
        for &b in bytes {
            self.parser.advance(&mut performer, b);
        }
    }

    /// 请求重设行列（窗口尺寸变化）。
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.grid.resize(cols, rows);
    }
}

/// 终端配色：16 色索引表（明暗两套，对应 macOS 端 TerminalTheme 双调色板）。
#[derive(Clone)]
pub struct AnsiPalette {
    pub default_fg: [u8; 3],
    pub default_bg: [u8; 3],
    pub cursor: [u8; 3],
    pub colors: [[u8; 3]; 16],
}

/// 深色调色板（深色偏亮）。
pub fn dark_palette() -> AnsiPalette {
    AnsiPalette {
        default_fg: [220, 223, 228],
        default_bg: [24, 24, 27],
        cursor: [240, 240, 240],
        colors: [
            [45, 45, 50],    // 0 black
            [255, 106, 106], // 1 red
            [152, 224, 130], // 2 green
            [242, 204, 118], // 3 yellow
            [126, 178, 255], // 4 blue
            [214, 142, 255], // 5 magenta
            [102, 208, 208], // 6 cyan
            [208, 208, 210], // 7 white
            [100, 100, 108], // 8 bright black
            [255, 138, 128], // 9 bright red
            [176, 240, 160], // 10 bright green
            [255, 222, 148], // 11 bright yellow
            [156, 200, 255], // 12 bright blue
            [232, 168, 255], // 13 bright magenta
            [136, 226, 226], // 14 bright cyan
            [240, 240, 244], // 15 bright white
        ],
    }
}

/// 浅色调色板（浅色偏深）。
pub fn light_palette() -> AnsiPalette {
    AnsiPalette {
        default_fg: [34, 36, 40],
        default_bg: [252, 252, 253],
        cursor: [30, 30, 32],
        colors: [
            [90, 90, 96],    // 0 black
            [196, 42, 42],   // 1 red
            [22, 128, 57],   // 2 green
            [161, 106, 4],   // 3 yellow
            [22, 84, 190],   // 4 blue
            [146, 46, 190],  // 5 magenta
            [10, 128, 128],  // 6 cyan
            [110, 112, 118], // 7 white
            [140, 140, 146], // 8 bright black
            [224, 70, 60],   // 9 bright red
            [40, 160, 80],   // 10 bright green
            [190, 130, 20],  // 11 bright yellow
            [50, 110, 230],  // 12 bright blue
            [170, 70, 220],  // 13 bright magenta
            [20, 150, 150],  // 14 bright cyan
            [50, 50, 54],    // 15 bright white
        ],
    }
}
