//! SSH 会话记录库与已知主机信任存储（SQLite）。
//! 对应 macOS 端 SSHProfileStore / SSHHostKeyStore / SSHKeyStorage。

use super::model::{SSHAuthKind, SSHFolder, SSHProfile, SSHSessionKind};
use anyhow::Result;
use devkit_core::db::Database;
use rusqlite::params;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub fn now() -> f64 {
    devkit_core::db::unix_now()
}

// ---------- Profile ----------

#[derive(Debug, Clone)]
pub struct ProfileRow {
    pub id: String,
    pub folder_id: Option<String>,
    pub profile: SSHProfile,
}

pub fn list_profiles(db: &Database) -> Result<Vec<ProfileRow>> {
    db.query(
        "SELECT id, folder_id, profile_json FROM ssh_profiles ORDER BY name;",
        &[],
        |r| {
            let id: String = r.get(0)?;
            let folder_id: Option<String> = r.get(1)?;
            let blob: Vec<u8> = r.get(2)?;
            let profile: SSHProfile = serde_json::from_slice(&blob)?;
            Ok(ProfileRow { id, folder_id, profile })
        },
    )
}

pub fn upsert_profile(db: &Database, profile: &SSHProfile) -> Result<()> {
    let blob = serde_json::to_vec(profile)?;
    db.run(
        "INSERT INTO ssh_profiles(id, folder_id, name, host, port, username, auth_kind, password, key_file, key_passphrase, created_at, updated_at, profile_json) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12) \
         ON CONFLICT(id) DO UPDATE SET folder_id=excluded.folder_id, name=excluded.name, host=excluded.host, \
           port=excluded.port, username=excluded.username, auth_kind=excluded.auth_kind, password=excluded.password, \
           key_file=excluded.key_file, key_passphrase=excluded.key_passphrase, updated_at=excluded.updated_at, profile_json=excluded.profile_json;",
        &params![
            profile.id.to_string(),
            profile.folder_id.map(|f| f.to_string()),
            profile.name,
            profile.host,
            profile.port as i64,
            profile.username,
            if profile.auth_kind == SSHAuthKind::Key { "key" } else { "password" },
            profile.password,
            profile.private_key_file_name,
            profile.key_passphrase,
            now(),
            blob
        ],
    )?;
    Ok(())
}

pub fn delete_profile(db: &Database, id: &str) -> Result<()> {
    db.run("DELETE FROM ssh_profiles WHERE id = ?1;", &params![id])?;
    Ok(())
}

// ---------- 文件夹 ----------

#[derive(Debug, Clone)]
pub struct FolderRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
}

pub fn list_folders(db: &Database) -> Result<Vec<FolderRow>> {
    db.query(
        "SELECT id, parent_id, name FROM ssh_folders ORDER BY name;",
        &[],
        |r| Ok(FolderRow { id: r.get(0)?, parent_id: r.get(1)?, name: r.get(2)? }),
    )
}

pub fn create_folder(db: &Database, parent_id: Option<&str>, name: &str) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    db.run(
        "INSERT INTO ssh_folders(id, parent_id, name, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?4);",
        &params![id, parent_id, name, now()],
    )?;
    Ok(id)
}

pub fn rename_folder(db: &Database, id: &str, name: &str) -> Result<()> {
    db.run(
        "UPDATE ssh_folders SET name = ?2, updated_at = ?3 WHERE id = ?1;",
        &params![id, name, now()],
    )?;
    Ok(())
}

pub fn move_folder(db: &Database, id: &str, parent_id: Option<&str>) -> Result<()> {
    db.run(
        "UPDATE ssh_folders SET parent_id = ?2, updated_at = ?3 WHERE id = ?1;",
        &params![id, parent_id, now()],
    )?;
    Ok(())
}

/// 级联删除文件夹（含子孙文件夹与其中的记录）。
pub fn delete_folder_cascade(db: &Database, id: &str) -> Result<()> {
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(id.to_string());
    let mut all = Vec::new();
    while let Some(cur) = queue.pop_front() {
        all.push(cur.clone());
        let children: Vec<(String,)> = db.query(
            "SELECT id FROM ssh_folders WHERE parent_id = ?1;",
            &params![cur],
            |r| Ok((r.get(0)?,)),
        )?;
        for (c,) in children {
            queue.push_back(c);
        }
    }
    for fid in &all {
        db.run("DELETE FROM ssh_profiles WHERE folder_id = ?1;", &params![fid])?;
        db.run("DELETE FROM ssh_folders WHERE id = ?1;", &params![fid])?;
    }
    Ok(())
}

// ---------- known_hosts（TOFU 信任） ----------

/// 查询是否已信任。key_blob 为 None 时只看 host:port 是否有任意记录。
pub fn is_trusted(db: &Database, host: &str, port: u16, key_blob: Option<&[u8]>) -> Result<bool> {
    match key_blob {
        Some(kb) => {
            let rows: Vec<(i64,)> = db.query(
                "SELECT COUNT(*) FROM ssh_known_hosts WHERE host = ?1 AND port = ?2 AND key_blob = ?3;",
                &params![host, port as i64, kb],
                |r| Ok((r.get(0)?,)),
            )?;
            Ok(rows.first().map(|(c,)| *c > 0).unwrap_or(false))
        }
        None => {
            let rows: Vec<(i64,)> = db.query(
                "SELECT COUNT(*) FROM ssh_known_hosts WHERE host = ?1 AND port = ?2;",
                &params![host, port as i64],
                |r| Ok((r.get(0)?,)),
            )?;
            Ok(rows.first().map(|(c,)| *c > 0).unwrap_or(false))
        }
    }
}

pub fn trust_host(db: &Database, host: &str, port: u16, key_type: &str, key_blob: &[u8]) -> Result<()> {
    db.run(
        "INSERT OR REPLACE INTO ssh_known_hosts(host, port, key_type, key_blob, added_at) VALUES(?1, ?2, ?3, ?4, ?5);",
        &params![host, port as i64, key_type, key_blob, now()],
    )?;
    Ok(())
}

// ---------- 密钥文件存储（数据目录 ssh_keys/） ----------

pub struct KeyStorage {
    pub dir: PathBuf,
}

impl KeyStorage {
    pub fn new(data_dir: &Path) -> Self {
        Self { dir: data_dir.join("ssh_keys") }
    }

    /// 把选中的 PEM 复制到数据目录，返回相对文件名。
    pub fn import_key(&self, source: &Path, profile_id: Uuid) -> Result<String> {
        std::fs::create_dir_all(&self.dir)?;
        let file_name = format!("{}.pem", profile_id);
        let dest = self.dir.join(&file_name);
        std::fs::copy(source, &dest)?;
        Ok(file_name)
    }

    pub fn key_path(&self, file_name: &str) -> PathBuf {
        self.dir.join(file_name)
    }

    pub fn delete_key(&self, file_name: &str) {
        let _ = std::fs::remove_file(self.dir.join(file_name));
    }

    pub fn list_keys(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if let Some(name) = e.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
        out.sort();
        out
    }
}

/// 由行数据还原会话类型（兼容旧数据缺 kind 字段）。
pub fn kind_from_profile_json_or(p: &SSHProfile, host: &str) -> SSHSessionKind {
    if host.is_empty() && p.host.is_empty() {
        p.kind
    } else {
        p.kind
    }
}
