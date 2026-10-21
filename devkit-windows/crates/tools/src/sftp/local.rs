//! 本地文件系统后端。

use super::backend::{sort_entries, DirEntry, SftpBackend};
use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

pub struct LocalBackend {
    pub home: PathBuf,
}

impl LocalBackend {
    pub fn new() -> Self {
        Self { home: dirs_home() }
    }
}

pub fn dirs_home() -> PathBuf {
    std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("C:\\"))
}

#[async_trait]
impl SftpBackend for LocalBackend {
    async fn list_dir(&mut self, path: &str) -> Result<Vec<DirEntry>> {
        let p = Path::new(path);
        let mut out = Vec::new();
        for e in std::fs::read_dir(p)? {
            let Ok(e) = e else { continue };
            let meta = e.metadata()?;
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            out.push(DirEntry {
                name: e.file_name().to_string_lossy().to_string(),
                is_dir: meta.is_dir(),
                size: meta.len(),
                modified,
            });
        }
        sort_entries(&mut out);
        Ok(out)
    }

    async fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(path)?)
    }

    async fn write_file(&mut self, path: &str, data: &[u8]) -> Result<()> {
        std::fs::write(path, data)?;
        Ok(())
    }

    async fn read_chunk(&mut self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; len as usize];
        let mut read = 0usize;
        while read < buf.len() {
            let n = f.read(&mut buf[read..])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        buf.truncate(read);
        Ok(buf)
    }

    async fn write_chunk(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(offset == 0)
            .open(path)?;
        f.seek(SeekFrom::Start(offset))?;
        f.write_all(data)?;
        f.flush()?;
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<()> {
        std::fs::create_dir_all(path)?;
        Ok(())
    }

    async fn remove_file(&mut self, path: &str) -> Result<()> {
        std::fs::remove_file(path)?;
        Ok(())
    }

    async fn remove_dir_recursive(&mut self, path: &str) -> Result<()> {
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    async fn exists(&mut self, path: &str) -> Result<bool> {
        Ok(Path::new(path).exists())
    }

    async fn is_dir(&mut self, path: &str) -> Result<bool> {
        Ok(Path::new(path).is_dir())
    }

    fn describe(&self) -> String {
        "本机".to_string()
    }
}
