//! SFTP 传输管线：经本地临时文件中转（源下载 → 目标上传）。
//! 小文件（<500KB）并发上限 4，大文件顺序；实时进度 + 取消。
//! 对应 macOS 端 SFTPTransport。

use super::backend::SftpBackend;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// 小文件阈值（字节）。
pub const SMALL_FILE_THRESHOLD: u64 = 500 * 1024;
/// 小文件并发上限。
pub const SMALL_CONCURRENCY: usize = 4;

/// 传输进度事件。
#[derive(Debug, Clone)]
pub enum TransferEvent {
    /// 计划阶段完成：总文件数与总字节。
    Planned { files: usize, bytes: u64 },
    /// 文件开始。
    FileStart { path: String },
    /// 字节进度（累计）。
    Progress { done_bytes: u64 },
    /// 单文件完成。
    FileDone { path: String },
    /// 失败。
    Failed { path: String, message: String },
    /// 全部完成。
    Finished { cancelled: bool },
}

/// 待传文件清单。
#[derive(Debug, Clone)]
pub struct TransferItem {
    pub src_path: String,
    pub dst_path: String,
    pub size: u64,
}

/// 递归收集（源后端）。
pub async fn plan_transfer(
    src: &mut dyn SftpBackend,
    src_path: &str,
    dst_root: &str,
    items: &mut Vec<TransferItem>,
) -> Result<()> {
    if src.is_dir(src_path).await? {
        // 目录：递归
        let name = src_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(src_path)
            .to_string();
        let dst_dir = join_path(dst_root, &name);
        let entries = src.list_dir(src_path).await?;
        for e in entries {
            let child_src = join_path(src_path, &e.name);
            if e.is_dir {
                Box::pin(plan_transfer(src, &child_src, &dst_dir, items)).await?;
            } else {
                items.push(TransferItem {
                    src_path: child_src,
                    dst_path: join_path(&dst_dir, &e.name),
                    size: e.size,
                });
            }
        }
    } else {
        let name = src_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(src_path)
            .to_string();
        let size = src.list_dir(parent_of(src_path)).await.map(|entries| {
            entries
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.size)
                .unwrap_or(0)
        })?;
        items.push(TransferItem {
            src_path: src_path.to_string(),
            dst_path: join_path(dst_root, &name),
            size,
        });
    }
    Ok(())
}

/// 执行传输：顺序执行（进度 + 取消）。小文件并发流水线依赖后端克隆能力，
/// 当前实现为顺序化等价语义（吞吐优化留待后端支持多通道时启用）。
pub async fn run_transfer(
    src: &mut dyn SftpBackend,
    dst: &mut dyn SftpBackend,
    items: Vec<TransferItem>,
    cancel: Arc<AtomicBool>,
    done_bytes: Arc<AtomicU64>,
    on_event: &dyn Fn(TransferEvent),
) {
    let total: u64 = items.iter().map(|i| i.size).sum();
    on_event(TransferEvent::Planned { files: items.len(), bytes: total });

    let (mut small, large): (Vec<&TransferItem>, Vec<&TransferItem>) =
        items.iter().partition(|i| i.size < SMALL_FILE_THRESHOLD);

    let mut failed: Vec<(String, String)> = Vec::new();

    // 大文件优先（用户感知最强），再小文件
    let mut ordered: Vec<&TransferItem> = large;
    ordered.append(&mut small);

    for item in ordered {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        on_event(TransferEvent::FileStart { path: item.src_path.clone() });
        match transfer_one(src, dst, item, &cancel, &done_bytes).await {
            Ok(()) => on_event(TransferEvent::FileDone { path: item.src_path.clone() }),
            Err(e) => {
                failed.push((item.src_path.clone(), format!("{e}")));
                on_event(TransferEvent::Failed { path: item.src_path.clone(), message: format!("{e}") });
            }
        }
    }

    let cancelled = cancel.load(Ordering::Relaxed);
    on_event(TransferEvent::Finished { cancelled });
    let _ = failed;
}

/// 单文件：源读 → 目标写。
async fn transfer_one(
    src: &mut dyn SftpBackend,
    dst: &mut dyn SftpBackend,
    item: &TransferItem,
    cancel: &Arc<AtomicBool>,
    done_bytes: &Arc<AtomicU64>,
) -> Result<()> {
    let data = src.read_file(&item.src_path).await?;
    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("已取消");
    }
    dst.write_file(&item.dst_path, &data).await?;
    done_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
    Ok(())
}

pub fn join_path(dir: &str, name: &str) -> String {
    let sep = if dir.contains('\\') && !dir.contains('/') { '\\' } else { '/' };
    if dir.ends_with('/') || dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}{sep}{name}")
    }
}

fn parent_of(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(i) => &path[..i.max(1)],
        None => path,
    }
}
