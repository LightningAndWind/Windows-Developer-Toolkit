//! 会话快照：持久化到 SQLite kv 表（key = "session.v1"），App 重启后恢复标签集合与分组。
//! 结构对应 macOS 端 `SessionSnapshot.swift`；工具内部状态以 JSON Value 形式内嵌。

use serde::{Deserialize, Serialize};

/// 序列化到磁盘的标签快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabSnapshot {
    pub id: uuid::Uuid,
    pub tool_id: String,
    #[serde(default)]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub group_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub is_pinned: bool,
    #[serde(default = "chrono::Utc::now")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 工具自行贡献的内部状态；旧快照无此字段 → None。
    #[serde(default)]
    pub tool_state: Option<serde_json::Value>,
}

/// 序列化到磁盘的分组快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabGroupSnapshot {
    pub id: uuid::Uuid,
    pub name: String,
    pub color_raw: String,
    #[serde(default)]
    pub is_collapsed: bool,
}

/// 单个窗口的会话快照（当前只有一个主窗口，字段允许多窗口扩展）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSessionSnapshot {
    pub window_id: uuid::Uuid,
    pub tabs: Vec<TabSnapshot>,
    pub groups: Vec<TabGroupSnapshot>,
    #[serde(default)]
    pub selected_tab_id: Option<uuid::Uuid>,
}

/// 整个 App 的会话快照（v1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "chrono::Utc::now")]
    pub saved_at: chrono::DateTime<chrono::Utc>,
    pub windows: Vec<WindowSessionSnapshot>,
}

fn default_version() -> u32 {
    1
}

const STORAGE_KEY: &str = "session.v1";

/// 会话持久化存储。
pub struct SessionStore;

impl SessionStore {
    /// 保存当前会话；db 未打开时静默返回。
    pub fn save(db: &crate::db::Database, snapshot: &SessionSnapshot) {
        if !db.is_open() {
            return;
        }
        if let Ok(data) = serde_json::to_vec(snapshot) {
            let _ = db.put_kv(STORAGE_KEY, &data);
        }
    }

    pub fn load(db: &crate::db::Database) -> Option<SessionSnapshot> {
        if !db.is_open() {
            return None;
        }
        let data = db.get_kv(STORAGE_KEY).ok()??;
        serde_json::from_slice(&data).ok()
    }

    pub fn clear(db: &crate::db::Database) {
        if db.is_open() {
            let _ = db.del_kv(STORAGE_KEY);
        }
    }
}
