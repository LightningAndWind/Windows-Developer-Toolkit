//! 外部运行工具（git / OpenSSH）解析。
//!
//! 解析顺序：环境变量覆盖 → exe 旁 `tools/`（随应用分发的便携版）→
//! 数据目录 `tools/`（应用内「一键安装」的下载位置）→ 系统 PATH。
//! 全部未命中时回退裸命令名，让调用方保持与原先一致的 spawn 行为。
//!
//! 预期目录布局（见仓库根 package.ps1 与设置页一键安装）：
//! - MinGit：`tools/git/cmd/git.exe`
//! - Win32-OpenSSH：`tools/ssh/ssh.exe`

use std::path::{Path, PathBuf};

pub mod setup;

/// 环境变量覆盖（值为可执行文件完整路径）。
pub const ENV_GIT: &str = "DEVKIT_GIT";
pub const ENV_SSH: &str = "DEVKIT_SSH";
pub const ENV_SSH_KEYGEN: &str = "DEVKIT_SSH_KEYGEN";

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|p| p.to_path_buf())
}

/// 候选 tools 根：exe 旁（随应用分发）→ 数据目录（应用内安装）。
fn tool_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(d) = exe_dir() {
        roots.push(d.join("tools"));
    }
    if let Some(d) = devkit_core::prefs::data_dir() {
        roots.push(PathBuf::from(d).join("tools"));
    }
    roots
}

/// 在一个 tools 根下定位 git.exe（MinGit / PortableGit 布局，容忍一层版本子目录）。
pub(crate) fn git_in(root: &Path) -> Option<PathBuf> {
    for rel in [
        "git/cmd/git.exe",
        "git/bin/git.exe",
        "git/mingw64/bin/git.exe",
        "PortableGit/cmd/git.exe",
        "cmd/git.exe",
        "mingw64/bin/git.exe",
    ] {
        let p = root.join(rel);
        if p.is_file() {
            return Some(p);
        }
    }
    // tools/git/<任意子目录>/…（手动解压多带出的一层版本目录）
    let base = root.join("git");
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            for rel in ["cmd/git.exe", "bin/git.exe", "mingw64/bin/git.exe"] {
                let p = e.path().join(rel);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// 在一个 tools 根下定位 ssh.exe（Win32-OpenSSH 便携版 / PortableGit 自带）。
pub(crate) fn ssh_in(root: &Path) -> Option<PathBuf> {
    for rel in [
        "ssh/ssh.exe",
        "ssh/bin/ssh.exe",
        "ssh/OpenSSH-Win64/ssh.exe",
        "OpenSSH-Win64/ssh.exe",
        "git/usr/bin/ssh.exe",
    ] {
        let p = root.join(rel);
        if p.is_file() {
            return Some(p);
        }
    }
    let base = root.join("ssh");
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            for rel in ["ssh.exe", "bin/ssh.exe"] {
                let p = e.path().join(rel);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// PATH 搜索（等价 which，含 .exe/.cmd/.bat 扩展）。
fn which(name: &str) -> Option<PathBuf> {
    let name = name.trim_end_matches(".exe");
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(';').filter(|s| !s.is_empty()) {
        for ext in ["", ".exe", ".cmd", ".bat"] {
            let p = Path::new(dir).join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

/// git.exe 完整路径（用于状态展示 / 检测；未解析到返回 None）。
pub fn git_path() -> Option<String> {
    if let Some(p) = env_path(ENV_GIT) {
        return Some(p.to_string_lossy().to_string());
    }
    for root in tool_roots() {
        if let Some(p) = git_in(&root) {
            return Some(p.to_string_lossy().to_string());
        }
    }
    which("git").map(|p| p.to_string_lossy().to_string())
}

/// ssh.exe 完整路径。
pub fn ssh_path() -> Option<String> {
    if let Some(p) = env_path(ENV_SSH) {
        return Some(p.to_string_lossy().to_string());
    }
    for root in tool_roots() {
        if let Some(p) = ssh_in(&root) {
            return Some(p.to_string_lossy().to_string());
        }
    }
    which("ssh").map(|p| p.to_string_lossy().to_string())
}

/// ssh-keygen.exe 完整路径（优先与 ssh.exe 同目录）。
pub fn ssh_keygen_path() -> Option<String> {
    if let Some(p) = env_path(ENV_SSH_KEYGEN) {
        return Some(p.to_string_lossy().to_string());
    }
    if let Some(s) = ssh_path() {
        if let Some(p) = Path::new(&s).parent().map(|d| d.join("ssh-keygen.exe")) {
            if p.is_file() {
                return Some(p.to_string_lossy().to_string());
            }
        }
    }
    which("ssh-keygen").map(|p| p.to_string_lossy().to_string())
}

/// 以下三个供 `Command::new` 使用：未解析到时回退裸命令名（由系统 PATH 兜底）。


pub fn git() -> String {
    git_path().unwrap_or_else(|| "git".into())
}

pub fn ssh() -> String {
    ssh_path().unwrap_or_else(|| "ssh".into())
}

pub fn ssh_keygen() -> String {
    ssh_keygen_path().unwrap_or_else(|| "ssh-keygen".into())
}

/// git 可用性（--version 探测）。
pub fn git_available() -> bool {
    std::process::Command::new(git())
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// ssh 可用性。Windows OpenSSH 的 `-V` 把版本串直接写控制台（重定向拿不到）且退出码 255，
/// 因此以「能成功启动且非缺 DLL 崩溃」为判据：启动失败或 STATUS_DLL_NOT_FOUND 才判不可用。
pub fn ssh_available() -> bool {
    match std::process::Command::new(ssh()).arg("-V").output() {
        Ok(o) => o.status.code().map(|c| c as u32) != Some(0xC0000135),
        Err(_) => false,
    }
}
