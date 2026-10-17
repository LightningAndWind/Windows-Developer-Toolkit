//! SFTP 后端抽象：本地与远端统一协议。
//! 对应 macOS 端 SFTPBackend / LocalSFTPBackend / SFTPProtocol。

use anyhow::Result;
use async_trait::async_trait;

/// 目录条目。
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<u64>,
}

/// 抽象后端：本地文件系统与远端 SFTP 同一接口。
#[async_trait]
pub trait SftpBackend: Send {
    /// 列目录（预排序：目录置顶 + 名称大小写不敏感排序）。
    async fn list_dir(&mut self, path: &str) -> Result<Vec<DirEntry>>;

    async fn read_file(&mut self, path: &str) -> Result<Vec<u8>>;
    async fn write_file(&mut self, path: &str, data: &[u8]) -> Result<()>;

    async fn mkdir(&mut self, path: &str) -> Result<()>;
    async fn remove_file(&mut self, path: &str) -> Result<()>;
    async fn remove_dir_recursive(&mut self, path: &str) -> Result<()>;
    async fn exists(&mut self, path: &str) -> Result<bool>;
    async fn is_dir(&mut self, path: &str) -> Result<bool>;

    fn describe(&self) -> String;
}

/// 目录排序缓存策略（目录置顶 + 本地化大小写不敏感）。
pub fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        })
    });
}
