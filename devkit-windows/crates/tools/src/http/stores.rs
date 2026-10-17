//! HTTP 历史与集合（多级文件夹）存储。表结构对应 macOS 端 DatabaseManager。

use super::model::{HTTPRequestModel, HistorySummary};
use anyhow::Result;
use devkit_core::db::Database;
use rusqlite::params;
use uuid::Uuid;

pub fn now() -> f64 {
    devkit_core::db::unix_now()
}

// ---------- 历史 ----------

pub fn insert_history(
    db: &Database,
    request: &HTTPRequestModel,
    response: Option<&super::model::HTTPResponseModel>,
    saved_request_id: Option<&str>,
) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let request_json = serde_json::to_vec(request)?;
    let (status, dur, size) = match response {
        Some(r) => (Some(r.status_code as i64), Some(r.duration_ms as i64), Some(r.size_bytes as i64)),
        None => (None, None, None),
    };
    db.run(
        "INSERT INTO http_history(id, created_at, method, url, status_code, duration_ms, size_bytes, saved_request_id, request_json) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9);",
        &params![
            id,
            now(),
            request.method,
            request.effective_url(),
            status,
            dur,
            size,
            saved_request_id,
            request_json
        ],
    )?;
    Ok(id)
}

/// 最近 N 条摘要（轻量，不读请求体）。
pub fn list_history(db: &Database, limit: usize) -> Result<Vec<HistorySummary>> {
    db.query(
        "SELECT id, created_at, method, url, status_code, duration_ms, size_bytes, saved_request_id \
         FROM http_history ORDER BY created_at DESC LIMIT ?1;",
        &params![limit as i64],
        |row| {
            Ok(HistorySummary {
                id: row.get(0)?,
                created_at: row.get(1)?,
                method: row.get(2)?,
                url: row.get(3)?,
                status_code: row.get(4)?,
                duration_ms: row.get(5)?,
                size_bytes: row.get(6)?,
                saved_request_id: row.get(7)?,
            })
        },
    )
}

pub fn count_history(db: &Database) -> Result<i64> {
    let rows = db.query("SELECT COUNT(*) FROM http_history;", &[], |r| Ok(r.get::<_, i64>(0)?))?;
    Ok(rows.first().copied().unwrap_or(0))
}

/// 关键字搜索摘要。
pub fn search_history(db: &Database, keyword: &str, limit: usize) -> Result<Vec<HistorySummary>> {
    let like = format!("%{}%", keyword.replace('%', ""));
    db.query(
        "SELECT id, created_at, method, url, status_code, duration_ms, size_bytes, saved_request_id \
         FROM http_history WHERE url LIKE ?1 OR method LIKE ?1 ORDER BY created_at DESC LIMIT ?2;",
        &params![like, limit as i64],
        |row| {
            Ok(HistorySummary {
                id: row.get(0)?,
                created_at: row.get(1)?,
                method: row.get(2)?,
                url: row.get(3)?,
                status_code: row.get(4)?,
                duration_ms: row.get(5)?,
                size_bytes: row.get(6)?,
                saved_request_id: row.get(7)?,
            })
        },
    )
}

/// 读完整历史（请求 + 响应）。
pub fn get_history(db: &Database, id: &str) -> Result<Option<(HTTPRequestModel, Option<super::model::HTTPResponseModel>)>> {
    let rows: Vec<(Vec<u8>,)> = db.query(
        "SELECT request_json FROM http_history WHERE id = ?1;",
        &params![id],
        |r| Ok((r.get::<_, Vec<u8>>(0)?,)),
    )?;
    let Some((blob,)) = rows.into_iter().next() else { return Ok(None) };
    #[derive(serde::Deserialize)]
    struct Full {
        request: HTTPRequestModel,
        #[serde(default)]
        response: Option<super::model::HTTPResponseModel>,
    }
    let full: Full = serde_json::from_slice(&blob)?;
    Ok(Some((full.request, full.response)))
}

pub fn delete_history(db: &Database, id: &str) -> Result<()> {
    db.run("DELETE FROM http_history WHERE id = ?1;", &params![id])?;
    Ok(())
}

pub fn clear_history(db: &Database) -> Result<()> {
    db.run("DELETE FROM http_history;", &[])?;
    Ok(())
}

// ---------- 集合（多级文件夹 + 请求） ----------

#[derive(Debug, Clone)]
pub struct FolderRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct RequestRow {
    pub id: String,
    pub folder_id: Option<String>,
    pub name: String,
    pub updated_at: f64,
    pub request: HTTPRequestModel,
}

pub fn list_folders(db: &Database) -> Result<Vec<FolderRow>> {
    db.query(
        "SELECT id, parent_id, name FROM http_folders ORDER BY name;",
        &[],
        |r| Ok(FolderRow { id: r.get(0)?, parent_id: r.get(1)?, name: r.get(2)? }),
    )
}

pub fn create_folder(db: &Database, parent_id: Option<&str>, name: &str) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let t = now();
    db.run(
        "INSERT INTO http_folders(id, parent_id, name, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?4);",
        &params![id, parent_id, name, t],
    )?;
    Ok(id)
}

pub fn rename_folder(db: &Database, id: &str, name: &str) -> Result<()> {
    db.run(
        "UPDATE http_folders SET name = ?2, updated_at = ?3 WHERE id = ?1;",
        &params![id, name, now()],
    )?;
    Ok(())
}

pub fn move_folder(db: &Database, id: &str, parent_id: Option<&str>) -> Result<()> {
    // 防环：parent 不得是自身后代
    if let Some(pid) = parent_id {
        let mut cur = Some(pid.to_string());
        let mut hops = 0;
        while let Some(c) = cur {
            if c == id {
                anyhow::bail!("不能把文件夹移动到它自己的子级");
            }
            if hops > 64 {
                break;
            }
            let rows: Vec<(Option<String>,)> = db.query(
                "SELECT parent_id FROM http_folders WHERE id = ?1;",
                &params![c],
                |r| Ok((r.get(0)?,)),
            )?;
            cur = rows.into_iter().next().and_then(|(p,)| p);
            hops += 1;
        }
    }
    db.run(
        "UPDATE http_folders SET parent_id = ?2, updated_at = ?3 WHERE id = ?1;",
        &params![id, parent_id, now()],
    )?;
    Ok(())
}

/// 删除文件夹：级联删除全部子孙文件夹、其中的请求、这些请求的历史。
pub fn delete_folder_cascade(db: &Database, id: &str) -> Result<()> {
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(id.to_string());
    let mut all = Vec::new();
    while let Some(cur) = queue.pop_front() {
        all.push(cur.clone());
        let children: Vec<(String,)> = db.query(
            "SELECT id FROM http_folders WHERE parent_id = ?1;",
            &params![cur],
            |r| Ok((r.get(0)?,)),
        )?;
        for (c,) in children {
            queue.push_back(c);
        }
    }
    for fid in &all {
        let reqs: Vec<(String,)> = db.query(
            "SELECT id FROM http_requests WHERE folder_id = ?1;",
            &params![fid],
            |r| Ok((r.get(0)?,)),
        )?;
        for (rid,) in reqs {
            db.run("DELETE FROM http_history WHERE saved_request_id = ?1;", &params![rid])?;
            db.run("DELETE FROM http_requests WHERE id = ?1;", &params![rid])?;
        }
        db.run("DELETE FROM http_folders WHERE id = ?1;", &params![fid])?;
    }
    Ok(())
}

pub fn list_requests(db: &Database, folder_id: Option<&str>) -> Result<Vec<RequestRow>> {
    let (sql, key): (&str, Option<&str>) = match folder_id {
        Some(_) => (
            "SELECT id, folder_id, name, updated_at, request_json FROM http_requests WHERE folder_id = ?1 ORDER BY name;",
            folder_id,
        ),
        None => (
            "SELECT id, folder_id, name, updated_at, request_json FROM http_requests WHERE folder_id IS NULL ORDER BY name;",
            None,
        ),
    };
    db.query(sql, &[&key], |r| {
        let blob: Vec<u8> = r.get(4)?;
        let request: HTTPRequestModel = serde_json::from_slice(&blob)?;
        Ok(RequestRow {
            id: r.get(0)?,
            folder_id: r.get(1)?,
            name: r.get(2)?,
            updated_at: r.get(3)?,
            request,
        })
    })
}

pub fn upsert_request(db: &Database, id: Option<&str>, folder_id: Option<&str>, name: &str, request: &HTTPRequestModel) -> Result<String> {
    let blob = serde_json::to_vec(request)?;
    match id {
        Some(id) => {
            db.run(
                "UPDATE http_requests SET folder_id = ?2, name = ?3, updated_at = ?4, request_json = ?5 WHERE id = ?1;",
                &params![id, folder_id, name, now(), blob],
            )?;
            Ok(id.to_string())
        }
        None => {
            let nid = Uuid::new_v4().to_string();
            db.run(
                "INSERT INTO http_requests(id, folder_id, name, created_at, updated_at, request_json) VALUES(?1, ?2, ?3, ?4, ?4, ?5);",
                &params![nid, folder_id, name, now(), blob],
            )?;
            Ok(nid)
        }
    }
}

pub fn get_request(db: &Database, id: &str) -> Result<Option<RequestRow>> {
    let rows: Vec<(Option<String>, String, f64, Vec<u8>)> = db.query(
        "SELECT folder_id, name, updated_at, request_json FROM http_requests WHERE id = ?1;",
        &params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let Some((folder_id, name, updated_at, blob)) = rows.into_iter().next() else {
        return Ok(None);
    };
    let request: HTTPRequestModel = serde_json::from_slice(&blob)?;
    Ok(Some(RequestRow { id: id.to_string(), folder_id, name, updated_at, request }))
}

/// 删除请求（级联其历史）。
pub fn delete_request(db: &Database, id: &str) -> Result<()> {
    db.run("DELETE FROM http_history WHERE saved_request_id = ?1;", &params![id])?;
    db.run("DELETE FROM http_requests WHERE id = ?1;", &params![id])?;
    Ok(())
}
