//! PTY 会话：基于 portable-pty（Windows 走 ConPTY）。
//! 读线程把 PTY 原始输出经 channel 推给 UI 侧；写侧直接写字节流。

use anyhow::{anyhow, Result};
use crossbeam_channel::{Receiver, Sender};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct PtySession {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: Receiver<Vec<u8>>,
    alive: Arc<AtomicBool>,
}

impl PtySession {
    /// 启动一个 shell 会话。`shell` 为 None 时自动选择 PowerShell。
    pub fn spawn(shell: Option<&str>, rows: u16, cols: u16, cwd: Option<&str>) -> Result<Self> {
        let program = shell.map(|s| s.to_string()).unwrap_or_else(default_shell);
        let args: Vec<String> = if program.ends_with("powershell.exe")
            || program.ends_with("pwsh.exe")
        {
            vec!["-NoLogo".into()]
        } else {
            Vec::new()
        };
        Self::spawn_command(&program, &args, rows, cols, cwd)
    }

    /// 启动任意命令（SSH 子进程等）。「ssh」经 toolchain 解析：
    /// 内置便携版优先于系统 PATH（见 toolchain 模块）。
    pub fn spawn_command(
        program: &str,
        args: &[String],
        rows: u16,
        cols: u16,
        cwd: Option<&str>,
    ) -> Result<Self> {
        let resolved;
        let program = if program.eq_ignore_ascii_case("ssh") {
            resolved = crate::toolchain::ssh();
            resolved.as_str()
        } else {
            program
        };
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| anyhow!("PTY 打开失败：{e}"))?;

        let mut cmd = CommandBuilder::new(program);
        cmd.args(args.iter().map(|s| s.as_str()));
        if let Some(dir) = cwd {
            if std::path::Path::new(dir).is_dir() {
                cmd.cwd(dir);
            }
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow!("进程启动失败：{e}"))?;

        let mut reader = pair.master.try_clone_reader().map_err(|e| anyhow!("{e}"))?;
        let writer = pair.master.take_writer().map_err(|e| anyhow!("{e}"))?;

        let (tx, rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        let alive = Arc::new(AtomicBool::new(true));
        let alive2 = alive.clone();
        std::thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                alive2.store(false, Ordering::SeqCst);
            })
            .ok();

        Ok(Self { writer, master: pair.master, child, rx, alive })
    }

    /// 写入原始字节（键盘输入）。
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes).map_err(|e| anyhow!("PTY 写入失败：{e}"))
    }

    /// 取出全部已就绪输出块。
    pub fn drain_output(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Ok(chunk) = self.rx.try_recv() {
            out.push(chunk);
        }
        out
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| anyhow!("PTY resize 失败：{e}"))
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// 终止子进程（关标签时调用，避免孤儿 shell 泄漏）。
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.kill();
    }
}

/// 默认 shell：优先 PowerShell 7（pwsh），回退 Windows PowerShell。
pub fn default_shell() -> String {
    for name in ["pwsh.exe", "powershell.exe"] {
        if which_in_path(name) {
            return name.to_string();
        }
    }
    "cmd.exe".to_string()
}

fn which_in_path(name: &str) -> bool {
    let path = std::env::var("PATH").unwrap_or_default();
    let exts = ["", ".exe", ".cmd", ".bat"];
    for dir in path.split(';') {
        if dir.is_empty() {
            continue;
        }
        for ext in exts {
            let p = std::path::Path::new(dir).join(format!("{name}{ext}"));
            if p.is_file() {
                return true;
            }
        }
    }
    false
}
