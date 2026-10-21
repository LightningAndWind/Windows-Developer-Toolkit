//! 远端 SFTP 后端：russh + russh-sftp（进程内，支持密码/私钥认证）。

use super::backend::{sort_entries, DirEntry, SftpBackend};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use russh::client::{self, Handle};
use russh_sftp::client::SftpSession;
use std::sync::Arc;

pub struct ClientHandler {
    accept_host: bool,
}

#[async_trait::async_trait]
impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(self.accept_host)
    }
}

/// 已连接的远端会话（保活 handle + sftp）。
pub struct RemoteBackend {
    pub handle: Handle<ClientHandler>,
    pub sftp: SftpSession,
    pub label: String,
}

impl RemoteBackend {
    /// 连接远端并打开 SFTP 子系统（私钥优先，其次密码）。
    pub async fn connect(
        host: &str,
        port: u16,
        username: &str,
        password: Option<String>,
        key_pem: Option<(String, Option<String>)>,
    ) -> Result<Self> {
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(600)),
            ..Default::default()
        });
        let handler = ClientHandler { accept_host: true };
        let mut handle =
            client::connect(config, (host, port), handler)
                .await
                .map_err(|e| anyhow!("SSH 连接失败：{e}"))?;

        // 认证：私钥优先
        let auth_ok = if let Some((pem, pass)) = key_pem {
            let key = russh::keys::decode_secret_key(&pem, pass.as_deref())
                .map_err(|e| anyhow!("私钥解析失败：{e}"))?;
            handle
                .authenticate_publickey(username, Arc::new(key))
                .await
                .map_err(|e| anyhow!("私钥认证失败：{e}"))?
        } else if let Some(pw) = password {
            handle
                .authenticate_password(username, &pw)
                .await
                .map_err(|e| anyhow!("密码认证失败：{e}"))?
        } else {
            return Err(anyhow!("未提供凭据"));
        };
        if !auth_ok {
            anyhow::bail!("认证失败：用户名或凭据不正确");
        }

        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| anyhow!("打开会话通道失败：{e}"))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| anyhow!("请求 sftp 子系统失败：{e}"))?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| anyhow!("SFTP 初始化失败：{e}"))?;

        Ok(Self { handle, sftp, label: format!("{username}@{host}") })
    }

    pub async fn canonical_home(&self) -> String {
        self.sftp.canonicalize(".").await.unwrap_or_else(|_| ".".to_string())
    }
}

#[async_trait]
impl SftpBackend for RemoteBackend {
    async fn list_dir(&mut self, path: &str) -> Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        let entries = self
            .sftp
            .read_dir(path)
            .await
            .map_err(|e| anyhow!("读取目录失败：{e}"))?;
        for e in entries {
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let meta = e.metadata();
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            out.push(DirEntry {
                is_dir: meta.is_dir(),
                size: meta.len(),
                modified,
                name,
            });
        }
        sort_entries(&mut out);
        Ok(out)
    }

    async fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        let mut file = self
            .sftp
            .open(path)
            .await
            .map_err(|e| anyhow!("打开文件失败：{e}"))?;
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .await
            .map_err(|e| anyhow!("读取文件失败：{e}"))?;
        Ok(buf)
    }

    async fn write_file(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let mut file = self
            .sftp
            .create(path)
            .await
            .map_err(|e| anyhow!("创建文件失败：{e}"))?;
        use tokio::io::AsyncWriteExt;
        file.write_all(data)
            .await
            .map_err(|e| anyhow!("写入文件失败：{e}"))?;
        file.flush().await.ok();
        Ok(())
    }

    async fn read_chunk(&mut self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut file = self
            .sftp
            .open(path)
            .await
            .map_err(|e| anyhow!("打开文件失败：{e}"))?;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| anyhow!("定位文件失败：{e}"))?;
        let mut buf = vec![0u8; len as usize];
        let mut read = 0usize;
        while read < buf.len() {
            let n = file.read(&mut buf[read..]).await.map_err(|e| anyhow!("读取文件失败：{e}"))?;
            if n == 0 {
                break;
            }
            read += n;
        }
        buf.truncate(read);
        Ok(buf)
    }

    async fn write_chunk(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<()> {
        use russh_sftp::protocol::OpenFlags;
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        let mut file = if offset == 0 {
            self.sftp
                .create(path)
                .await
                .map_err(|e| anyhow!("创建文件失败：{e}"))?
        } else {
            self.sftp
                .open_with_flags(path, OpenFlags::WRITE | OpenFlags::CREATE)
                .await
                .map_err(|e| anyhow!("打开文件失败：{e}"))?
        };
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| anyhow!("定位文件失败：{e}"))?;
        }
        file.write_all(data)
            .await
            .map_err(|e| anyhow!("写入文件失败：{e}"))?;
        file.flush().await.ok();
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<()> {
        self.sftp
            .create_dir(path)
            .await
            .map_err(|e| anyhow!("创建目录失败：{e}"))?;
        Ok(())
    }

    async fn remove_file(&mut self, path: &str) -> Result<()> {
        self.sftp
            .remove_file(path)
            .await
            .map_err(|e| anyhow!("删除文件失败：{e}"))?;
        Ok(())
    }

    async fn remove_dir_recursive(&mut self, path: &str) -> Result<()> {
        // 先清空子级
        let entries = self.list_dir(path).await.unwrap_or_default();
        for e in entries {
            let child = join_remote(path, &e.name);
            if e.is_dir {
                Box::pin(self.remove_dir_recursive(&child)).await?;
            } else {
                self.remove_file(&child).await?;
            }
        }
        self.sftp
            .remove_dir(path)
            .await
            .map_err(|e| anyhow!("删除目录失败：{e}"))?;
        Ok(())
    }

    async fn exists(&mut self, path: &str) -> Result<bool> {
        Ok(self.sftp.metadata(path).await.is_ok())
    }

    async fn is_dir(&mut self, path: &str) -> Result<bool> {
        Ok(self
            .sftp
            .metadata(path)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false))
    }

    fn describe(&self) -> String {
        self.label.clone()
    }
}

/// 远端路径拼接（POSIX）。
pub fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}
