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

    /// 读取文件的一段（从 offset 起最多 len 字节）。
    ///
    /// 默认实现退化为「整文件读取后切片」，保证新后端不实现也能工作；
    /// 本地 / 远端后端都覆写为真正的分块读取，传输大文件时不会把整份内容读进内存。
    async fn read_chunk(&mut self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        let data = self.read_file(path).await?;
        let start = (offset as usize).min(data.len());
        let end = ((offset.saturating_add(len)) as usize).min(data.len());
        Ok(data[start..end].to_vec())
    }

    /// 在 offset 处写入一段数据。
    ///
    /// 默认实现退化为「读出旧内容后整体覆写」；offset == 0 时等价于 write_file。
    async fn write_chunk(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<()> {
        if offset == 0 {
            return self.write_file(path, data).await;
        }
        let mut all = self.read_file(path).await.unwrap_or_default();
        let start = (offset as usize).min(all.len());
        all.resize(start + data.len(), 0);
        all[start..start + data.len()].copy_from_slice(data);
        self.write_file(path, &all).await
    }

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
