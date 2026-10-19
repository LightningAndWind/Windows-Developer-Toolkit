//! PTY → 终端网格 冒烟测试：验证本机 ConPTY 管线能产出可读输出。
//! 仅在 Windows 下有意义；CI 无桌面会话时可能超时，标记 ignore 由本地手动运行。

#![cfg(target_os = "windows")]

use devkit_tools::terminal::pty::PtySession;
use devkit_tools::terminal::TerminalSession;
use std::time::{Duration, Instant};

fn collect_grid_text(rows: u16, cols: u16) -> String {
    let mut pty = PtySession::spawn(None, rows, cols, None).expect("spawn local shell");
    let mut session = TerminalSession::new(cols as usize, rows as usize);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got_prompt = false;
    while Instant::now() < deadline {
        let chunks = pty.drain_output();
        if !chunks.is_empty() {
            for c in &chunks {
                session.feed(c);
            }
            // 光标列回到 0 且第 0 行有内容，近似认为已出现提示符
            let first: String = session
                .grid
                .row_view(0)
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end()
                .to_string();
            if !first.is_empty() {
                got_prompt = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = got_prompt;
    let mut text = String::new();
    for r in 0..session.grid.rows {
        let line: String = session.grid.row_view(r).iter().map(|c| c.ch).collect();
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

#[test]
fn local_shell_produces_output() {
    let text = collect_grid_text(30, 100);
    println!("---- grid ----\n{text}---- end ----");
    assert!(text.trim().len() > 0, "网格无任何文本：ConPTY 管线不通");
}
