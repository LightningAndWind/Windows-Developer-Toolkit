//! Git 工具数据模型。对应 macOS 端 Tools/Git/Models + GitTypes。

use serde::{Deserialize, Serialize};

/// 登记的仓库。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitRepo {
    #[serde(default = "uuid::Uuid::new_v4")]
    pub id: uuid::Uuid,
    #[serde(default)]
    pub folder_id: Option<uuid::Uuid>,
    pub alias: String,
    pub path: String,
    /// 绑定的密钥 ID（git_keys.id）。
    #[serde(default)]
    pub key_id: Option<uuid::Uuid>,
    #[serde(default = "chrono::Utc::now")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default = "chrono::Utc::now")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 密钥元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitKey {
    #[serde(default = "uuid::Uuid::new_v4")]
    pub id: uuid::Uuid,
    pub name: String,
    /// Ed25519 / RSA4096
    pub algorithm: String,
    #[serde(default)]
    pub comment: Option<String>,
    pub public_key: String,
    /// git_keys/ 内的相对文件名。
    #[serde(default)]
    pub private_file: Option<String>,
    pub has_passphrase: bool,
    /// true=导入已有私钥，false=ssh-keygen 生成。
    pub imported: bool,
    #[serde(default = "chrono::Utc::now")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default = "chrono::Utc::now")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 工作区变更条目。
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeEntry {
    pub path: String,
    /// 状态码（M/A/D/R/??/UU 等）。
    pub status: String,
    /// true = 已暂存（未提交组），false = 未暂存组。
    pub staged: bool,
    /// 未跟踪文件。
    pub untracked: bool,
    /// 冲突。
    pub conflict: bool,
}

/// 提交行。
#[derive(Debug, Clone)]
pub struct CommitRow {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

/// 分支行。
#[derive(Debug, Clone)]
pub struct BranchRow {
    pub name: String,
    pub current: bool,
    pub remote: bool,
}

/// Stash 行。
#[derive(Debug, Clone)]
pub struct StashRow {
    pub index: usize,
    pub message: String,
}

/// diff 行类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Add,
    Del,
    Hunk,
    Meta,
}

/// diff 行。
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub left_no: Option<u32>,
    pub right_no: Option<u32>,
    pub text: String,
}

/// 冲突块（「保留我方/对方」操作以文件为单位简化实现）。
#[derive(Debug, Clone)]
pub struct RepoStatus {
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub rebasing: bool,
    pub conflicts: Vec<String>,
    pub unstaged: Vec<ChangeEntry>,
    pub staged: Vec<ChangeEntry>,
    pub untracked: Vec<ChangeEntry>,
    pub unpushed: usize,
}
