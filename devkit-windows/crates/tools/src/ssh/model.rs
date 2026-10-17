//! SSH 会话记录模型。对应 macOS 端 Tools/SSH/Models/SSHProfile.swift。

use serde::{Deserialize, Serialize};

/// 认证方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SSHAuthKind {
    #[default]
    Password,
    Key,
}

impl SSHAuthKind {
    pub fn label(&self) -> &'static str {
        match self {
            SSHAuthKind::Password => "密码",
            SSHAuthKind::Key => "私钥",
        }
    }
}

/// 会话类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SSHSessionKind {
    #[default]
    Remote,
    Local,
}

/// 一条终端会话记录（远程主机或本地终端）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SSHProfile {
    #[serde(default = "uuid::Uuid::new_v4")]
    pub id: uuid::Uuid,
    #[serde(default)]
    pub folder_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub kind: SSHSessionKind,
    pub name: String,
    /// 默认进入的目录：本地=本机路径；远程=登录后自动 cd 的远端路径。
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub auth_kind: SSHAuthKind,
    /// 密码认证：明文密码（按用户要求离线明文存储）。
    #[serde(default)]
    pub password: Option<String>,
    /// 私钥认证：数据目录 ssh_keys/ 内的相对文件名。
    #[serde(default)]
    pub private_key_file_name: Option<String>,
    /// 私钥口令（明文，可选）。
    #[serde(default)]
    pub key_passphrase: Option<String>,
    #[serde(default = "chrono::Utc::now")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default = "chrono::Utc::now")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

fn default_port() -> u16 {
    22
}

impl Default for SSHProfile {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            folder_id: None,
            kind: SSHSessionKind::Remote,
            name: String::new(),
            working_directory: None,
            host: String::new(),
            port: 22,
            username: String::new(),
            auth_kind: SSHAuthKind::Password,
            password: None,
            private_key_file_name: None,
            key_passphrase: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }
}

impl SSHProfile {
    pub fn is_local(&self) -> bool {
        self.kind == SSHSessionKind::Local
    }

    /// 归一化的默认目录（空白视为未设置）。
    pub fn normalized_working_directory(&self) -> Option<String> {
        self.working_directory
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 展示用 user@host:port（默认端口省略）。
    pub fn connect_summary(&self) -> String {
        let port_part = if self.port == 22 { String::new() } else { format!(":{}", self.port) };
        format!("{}@{}{}", self.username, self.host, port_part)
    }

    /// POSIX 单引号转义。
    pub fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    /// 登录后要发送的 cd 命令（进程内引擎）。
    pub fn remote_cd_command(&self) -> Option<String> {
        if self.is_local() {
            return None;
        }
        self.normalized_working_directory()
            .map(|dir| format!("cd {}", Self::shell_quote(&dir)))
    }

    /// 外部 ssh 子进程的远程命令。
    pub fn ssh_remote_command(&self) -> Option<String> {
        if self.is_local() {
            return None;
        }
        self.normalized_working_directory()
            .map(|dir| format!("cd {}; exec \"$SHELL\" -l", Self::shell_quote(&dir)))
    }
}

/// SSH 集合文件夹。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SSHFolder {
    #[serde(default = "uuid::Uuid::new_v4")]
    pub id: uuid::Uuid,
    #[serde(default)]
    pub parent_id: Option<uuid::Uuid>,
    pub name: String,
    #[serde(default = "chrono::Utc::now")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default = "chrono::Utc::now")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 连接状态机。
#[derive(Debug, Clone, PartialEq)]
pub enum SSHConnectionState {
    Idle,
    Connecting,
    Connected(String),
    Failed(String),
    Disconnected,
}

impl SSHConnectionState {
    pub fn is_live(&self) -> bool {
        matches!(self, SSHConnectionState::Connecting | SSHConnectionState::Connected(_))
    }

    pub fn label(&self) -> String {
        match self {
            SSHConnectionState::Idle => "未连接".into(),
            SSHConnectionState::Connecting => "连接中…".into(),
            SSHConnectionState::Connected(h) => format!("已连接 {h}"),
            SSHConnectionState::Failed(m) => format!("连接失败：{m}"),
            SSHConnectionState::Disconnected => "已断开".into(),
        }
    }
}
