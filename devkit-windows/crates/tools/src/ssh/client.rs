//! SSH 终端引擎：统一走系统 OpenSSH 子进程 + ConPTY；
//! 密码/口令/主机信任提示通过 PTY 自动应答（对应 macOS 双引擎的 Windows 实现）。

use super::model::{SSHAuthKind, SSHProfile, SSHConnectionState};
use super::store::KeyStorage;
use crate::terminal::pty::PtySession;
use anyhow::{anyhow, Result};

/// SSH 会话：PTY + 提示监视器 + 状态。
pub struct SshTerminal {
    pub pty: PtySession,
    pub state: SSHConnectionState,
    watcher: PromptWatcher,
}

/// 自动应答状态机：password / passphrase / known_hosts 确认。
struct PromptWatcher {
    password: Option<String>,
    passphrase: Option<String>,
    auto_trust: bool,
    password_typed: bool,
    passphrase_typed: bool,
    trust_answered: bool,
    buf: Vec<u8>,
}

impl PromptWatcher {
    fn new(password: Option<String>, passphrase: Option<String>, auto_trust: bool) -> Self {
        Self {
            password,
            passphrase,
            auto_trust,
            password_typed: false,
            passphrase_typed: false,
            trust_answered: false,
            buf: Vec::with_capacity(512),
        }
    }

    /// 输入一段输出，返回需要注入 PTY 的应答字节。
    fn on_output(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(chunk);
        if self.buf.len() > 1024 {
            let drop = self.buf.len() - 512;
            self.buf.drain(..drop);
        }
        let text = String::from_utf8_lossy(&self.buf);
        let lower = text.to_lowercase();

        if !self.trust_answered
            && self.auto_trust
            && (lower.contains("continue connecting") || lower.contains("authenticity"))
        {
            self.trust_answered = true;
            self.buf.clear();
            return b"yes\r".to_vec();
        }
        if !self.passphrase_typed {
            if let Some(pp) = &self.passphrase {
                if lower.contains("passphrase") && !lower.contains("password") {
                    self.passphrase_typed = true;
                    self.buf.clear();
                    return format!("{pp}\r").into_bytes();
                }
            }
        }
        if !self.password_typed {
            if let Some(pw) = &self.password {
                // 覆盖 OpenSSH 与多数服务器登录提示（含中文「密码：」）
                if lower.contains("password:") || lower.contains("password：") {
                    self.password_typed = true;
                    self.buf.clear();
                    return format!("{pw}\r").into_bytes();
                }
            }
        }
        Vec::new()
    }
}

impl SshTerminal {
    /// 启动 SSH 会话。TOFU：`auto_trust` 为 true 时自动确认首次连接并落库。
    pub fn connect(
        profile: &SSHProfile,
        keys: &KeyStorage,
        rows: u16,
        cols: u16,
        auto_trust: bool,
    ) -> Result<Self> {
        if profile.is_local() {
            anyhow::bail!("本地终端请走本地 shell");
        }
        let mut args: Vec<String> = vec![
            "-tt".into(),
            "-p".into(),
            profile.port.to_string(),
            "-o".into(),
            "StrictHostKeyChecking=ask".into(),
            "-o".into(),
            "BatchMode=no".into(),
        ];
        match profile.auth_kind {
            SSHAuthKind::Key => {
                let file = profile
                    .private_key_file_name
                    .as_ref()
                    .ok_or_else(|| anyhow!("未配置私钥文件"))?;
                let path = keys.key_path(file);
                if !path.exists() {
                    anyhow::bail!("私钥文件不存在：{}", path.display());
                }
                args.push("-i".into());
                args.push(path.to_string_lossy().to_string());
                args.push("-o".into());
                args.push("IdentitiesOnly=yes".into());
            }
            SSHAuthKind::Password => {
                // 密码经 PTY 自动键入
            }
        }
        args.push(format!("{}@{}", profile.username, profile.host));
        if let Some(rc) = profile.ssh_remote_command() {
            args.push(rc);
        }

        let pty = PtySession::spawn_command("ssh", &args, rows, cols, None)?;
        let watcher = PromptWatcher::new(
            profile.password.clone(),
            profile.key_passphrase.clone(),
            auto_trust,
        );
        Ok(Self {
            pty,
            state: SSHConnectionState::Connecting,
            watcher,
        })
    }

    /// 启动本地 shell。
    pub fn connect_local(rows: u16, cols: u16, cwd: Option<&str>) -> Result<Self> {
        let pty = PtySession::spawn(None, rows, cols, cwd)?;
        Ok(Self {
            pty,
            state: SSHConnectionState::Connected("本地终端".into()),
            watcher: PromptWatcher::new(None, None, false),
        })
    }

    /// 输出喂给监视器，返回应注入的自动应答字节。
    pub fn on_output(&mut self, chunk: &[u8]) -> Vec<u8> {
        let inject = self.watcher.on_output(chunk);
        // 用监视器的滚动缓冲做整段判断：ssh 的报错可能跨 chunk 到达。
        let buf = String::from_utf8_lossy(&self.watcher.buf).to_lowercase();

        if let Some(reason) = classify_failure(&buf) {
            self.state = SSHConnectionState::Failed(reason.to_string());
        } else if let Some(reason) = classify_closed(&buf) {
            self.state = SSHConnectionState::Disconnected;
            let _ = reason;
        } else if self.state == SSHConnectionState::Connecting
            && (self.watcher.password_typed || self.watcher.trust_answered)
            && !buf.trim().is_empty()
        {
            // 已应答认证提示且没有失败输出：视为进入 shell。
            self.state = SSHConnectionState::Connected(self.watcher_summary());
        }
        inject
    }

    fn watcher_summary(&self) -> String {
        "远程会话".into()
    }

    pub fn mark_connected(&mut self) {
        if self.state == SSHConnectionState::Connecting {
            self.state = SSHConnectionState::Connected("远程会话".into());
        }
    }

    pub fn mark_disconnected(&mut self) {
        self.state = SSHConnectionState::Disconnected;
    }
}

/// 把 OpenSSH 的典型报错映射为可读的中文原因。
fn classify_failure(buf: &str) -> Option<&'static str> {
    if buf.contains("permission denied") {
        return Some("认证失败：用户名、密码或私钥不正确");
    }
    if buf.contains("connection timed out") || buf.contains("operation timed out") {
        return Some("连接超时：主机或端口不可达");
    }
    if buf.contains("connection refused") {
        return Some("连接被拒绝：目标端口未开放");
    }
    if buf.contains("could not resolve hostname") {
        return Some("无法解析主机名");
    }
    if buf.contains("no route to host") || buf.contains("network is unreachable") {
        return Some("网络不可达");
    }
    if buf.contains("host key verification failed") {
        return Some("主机密钥校验失败");
    }
    if buf.contains("too many authentication failures") {
        return Some("认证失败次数过多");
    }
    if buf.contains("connection reset by peer") {
        return Some("连接被对端重置");
    }
    None
}

/// 服务器/客户端主动关闭连接：属于「已结束」而不是配置错误。
fn classify_closed(buf: &str) -> Option<&'static str> {
    if buf.contains("connection closed by") || buf.contains("connection to") && buf.contains("closed") {
        return Some("连接已关闭");
    }
    None
}
