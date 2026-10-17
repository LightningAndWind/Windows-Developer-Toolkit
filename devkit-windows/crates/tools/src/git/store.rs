//! Git 仓库登记与密钥元信息存储（SQLite）。对应 macOS 端 GitRepoStore / GitKeyStore。

use super::types::{GitKey, GitRepo};
use anyhow::Result;
use devkit_core::db::Database;
use rusqlite::params;
use uuid::Uuid;

pub fn now() -> f64 {
    devkit_core::db::unix_now()
}

// ---------- 仓库 ----------

#[derive(Debug, Clone)]
pub struct RepoRow {
    pub id: String,
    pub folder_id: Option<String>,
    pub repo: GitRepo,
}

pub fn list_repos(db: &Database) -> Result<Vec<RepoRow>> {
    db.query(
        "SELECT id, folder_id, repo_json FROM git_repos ORDER BY alias;",
        &[],
        |r| {
            let id: String = r.get(0)?;
            let folder_id: Option<String> = r.get(1)?;
            let blob: Vec<u8> = r.get(2)?;
            let repo: GitRepo = serde_json::from_slice(&blob)?;
            Ok(RepoRow { id, folder_id, repo })
        },
    )
}

pub fn upsert_repo(db: &Database, repo: &GitRepo) -> Result<()> {
    let blob = serde_json::to_vec(repo)?;
    db.run(
        "INSERT INTO git_repos(id, folder_id, alias, path, key_id, created_at, updated_at, repo_json) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7) \
         ON CONFLICT(id) DO UPDATE SET folder_id=excluded.folder_id, alias=excluded.alias, path=excluded.path, \
           key_id=excluded.key_id, updated_at=excluded.updated_at, repo_json=excluded.repo_json;",
        &params![
            repo.id.to_string(),
            repo.folder_id.map(|f| f.to_string()),
            repo.alias,
            repo.path,
            repo.key_id.map(|k| k.to_string()),
            now(),
            blob
        ],
    )?;
    Ok(())
}

pub fn delete_repo(db: &Database, id: &str) -> Result<()> {
    db.run("DELETE FROM git_repos WHERE id = ?1;", &params![id])?;
    Ok(())
}

/// 级联删除 git 文件夹（含子孙与其中的仓库登记）。
pub fn delete_folder_cascade(db: &Database, id: &str) -> Result<()> {
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(id.to_string());
    let mut all = Vec::new();
    while let Some(cur) = queue.pop_front() {
        all.push(cur.clone());
        let children: Vec<(String,)> = db.query(
            "SELECT id FROM git_folders WHERE parent_id = ?1;",
            &params![cur],
            |r| Ok((r.get(0)?,)),
        )?;
        for (c,) in children {
            queue.push_back(c);
        }
    }
    for fid in &all {
        db.run("DELETE FROM git_repos WHERE folder_id = ?1;", &params![fid])?;
        db.run("DELETE FROM git_folders WHERE id = ?1;", &params![fid])?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct GitFolderRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
}

pub fn list_folders(db: &Database) -> Result<Vec<GitFolderRow>> {
    db.query(
        "SELECT id, parent_id, name FROM git_folders ORDER BY name;",
        &[],
        |r| Ok(GitFolderRow { id: r.get(0)?, parent_id: r.get(1)?, name: r.get(2)? }),
    )
}

pub fn create_folder(db: &Database, parent_id: Option<&str>, name: &str) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    db.run(
        "INSERT INTO git_folders(id, parent_id, name, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?4);",
        &params![id, parent_id, name, now()],
    )?;
    Ok(id)
}

// ---------- 密钥 ----------

pub fn list_keys(db: &Database) -> Result<Vec<GitKey>> {
    db.query("SELECT key_json FROM git_keys ORDER BY created_at;", &[], |r| {
        let blob: Vec<u8> = r.get(0)?;
        Ok(serde_json::from_slice(&blob)?)
    })
}

pub fn upsert_key(db: &Database, key: &GitKey) -> Result<()> {
    let blob = serde_json::to_vec(key)?;
    db.run(
        "INSERT INTO git_keys(id, name, algorithm, comment, public_key, private_file, has_passphrase, imported, created_at, updated_at, key_json) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10) \
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, algorithm=excluded.algorithm, comment=excluded.comment, \
           public_key=excluded.public_key, private_file=excluded.private_file, has_passphrase=excluded.has_passphrase, \
           imported=excluded.imported, updated_at=excluded.updated_at, key_json=excluded.key_json;",
        &params![
            key.id.to_string(),
            key.name,
            key.algorithm,
            key.comment,
            key.public_key,
            key.private_file,
            key.has_passphrase as i64,
            key.imported as i64,
            now(),
            blob
        ],
    )?;
    Ok(())
}

pub fn delete_key(db: &Database, id: &str) -> Result<()> {
    db.run("DELETE FROM git_keys WHERE id = ?1;", &params![id])?;
    Ok(())
}
