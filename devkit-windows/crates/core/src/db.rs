//! SQLite 数据库封装。表结构原样对应 macOS 端 `DatabaseManager.swift` 的 10 张表。

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value BLOB NOT NULL,
    updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS http_history (
    id TEXT PRIMARY KEY,
    created_at REAL NOT NULL,
    method TEXT NOT NULL,
    url TEXT NOT NULL,
    status_code INTEGER,
    duration_ms INTEGER,
    size_bytes INTEGER,
    saved_request_id TEXT,
    request_json BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_history_created ON http_history(created_at);
CREATE INDEX IF NOT EXISTS idx_history_url ON http_history(url);
CREATE INDEX IF NOT EXISTS idx_history_saved ON http_history(saved_request_id);
CREATE TABLE IF NOT EXISTS http_folders(
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    name TEXT NOT NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_folder_parent ON http_folders(parent_id);
CREATE TABLE IF NOT EXISTS http_requests(
    id TEXT PRIMARY KEY,
    folder_id TEXT,
    name TEXT NOT NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    request_json BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_request_folder ON http_requests(folder_id);
CREATE TABLE IF NOT EXISTS ssh_folders(
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    name TEXT NOT NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sshfolder_parent ON ssh_folders(parent_id);
CREATE TABLE IF NOT EXISTS ssh_profiles(
    id TEXT PRIMARY KEY,
    folder_id TEXT,
    name TEXT NOT NULL,
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    username TEXT NOT NULL,
    auth_kind TEXT NOT NULL,
    password TEXT,
    key_file TEXT,
    key_passphrase TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    profile_json BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sshprofile_folder ON ssh_profiles(folder_id);
CREATE TABLE IF NOT EXISTS ssh_known_hosts(
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    key_type TEXT NOT NULL,
    key_blob BLOB NOT NULL,
    added_at REAL NOT NULL,
    PRIMARY KEY(host, port, key_type)
);
CREATE TABLE IF NOT EXISTS git_folders(
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    name TEXT NOT NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_gitfolder_parent ON git_folders(parent_id);
CREATE TABLE IF NOT EXISTS git_repos(
    id TEXT PRIMARY KEY,
    folder_id TEXT,
    alias TEXT NOT NULL,
    path TEXT NOT NULL,
    key_id TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    repo_json BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_gitrepo_folder ON git_repos(folder_id);
CREATE TABLE IF NOT EXISTS git_keys(
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    algorithm TEXT NOT NULL,
    comment TEXT,
    public_key TEXT NOT NULL,
    private_file TEXT,
    has_passphrase INTEGER NOT NULL,
    imported INTEGER NOT NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    key_json BLOB NOT NULL
);
"#;

/// SQLite 数据库（线程安全：内部互斥串行化，语义对应 Swift 端 NSLock 串行队列）。
#[derive(Clone)]
pub struct Database {
    conn: std::sync::Arc<Mutex<Option<Connection>>>,
    pub path: Option<std::path::PathBuf>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| anyhow!("数据库打开失败：{e}"))?;
        conn.busy_timeout(std::time::Duration::from_millis(3000)).ok();
        conn.execute_batch(SCHEMA)
            .map_err(|e| anyhow!("建表失败：{e}"))?;
        Ok(Self { conn: std::sync::Arc::new(Mutex::new(Some(conn))), path: Some(path.to_path_buf()) })
    }

    /// 打开数据库（已打开则先关闭）。
    pub fn reopen(&self, path: &Path) -> Result<()> {
        let conn = Connection::open(path).map_err(|e| anyhow!("数据库打开失败：{e}"))?;
        conn.busy_timeout(std::time::Duration::from_millis(3000)).ok();
        conn.execute_batch(SCHEMA).map_err(|e| anyhow!("建表失败：{e}"))?;
        let mut guard = self.conn.lock().unwrap();
        *guard = Some(conn);
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.conn.lock().unwrap().is_some()
    }

    pub fn close(&self) {
        *self.conn.lock().unwrap() = None;
    }

    /// 执行写语句（INSERT/UPDATE/DELETE），返回受影响行数。
    pub fn run(&self, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Result<usize> {
        let guard = self.conn.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| anyhow!("数据库未打开"))?;
        Ok(conn.execute(sql, p)?)
    }

    /// 执行只读查询，逐行回调；回调返回 false 提前中止。
    pub fn query<T>(
        &self,
        sql: &str,
        p: &[&dyn rusqlite::ToSql],
        mut row: impl FnMut(&rusqlite::Row<'_>) -> Result<T>,
    ) -> Result<Vec<T>> {
        let guard = self.conn.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| anyhow!("数据库未打开"))?;
        let mut stmt = conn.prepare(sql)?;
        let mut out = Vec::new();
        let mut rows = stmt.query(p)?;
        while let Some(r) = rows.next()? {
            out.push(row(r)?);
        }
        Ok(out)
    }

    /// 写 kv（UPSERT）。
    pub fn put_kv(&self, key: &str, value: &[u8]) -> Result<()> {
        self.run(
            "INSERT INTO kv(key, value, updated_at) VALUES(?1, ?2, ?3) \
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at;",
            &params![key, value, unix_now()],
        )?;
        Ok(())
    }

    /// 读 kv。
    pub fn get_kv(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let rows = self.query("SELECT value FROM kv WHERE key = ?1;", &params![key], |r| {
            Ok(r.get::<_, Vec<u8>>(0)?)
        })?;
        Ok(rows.into_iter().next())
    }

    /// 删 kv。
    pub fn del_kv(&self, key: &str) -> Result<()> {
        self.run("DELETE FROM kv WHERE key = ?1;", &params![key])?;
        Ok(())
    }
}

pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
