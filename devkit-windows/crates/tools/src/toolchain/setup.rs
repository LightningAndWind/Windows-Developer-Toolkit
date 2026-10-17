//! 一键安装：从 GitHub 下载便携版 MinGit / Win32-OpenSSH，解压到数据目录 `tools/` 下。
//! 不写系统目录、不改 PATH —— 解析顺序见 [`super`]。
//!
//! 下载资产：
//! - Git：<https://github.com/git-for-windows/git/releases> 的 `MinGit-*-64-bit.zip`
//! - OpenSSH：<https://github.com/PowerShell/Win32-OpenSSH/releases> 的 `OpenSSH-Win64.zip`

use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const GIT_RELEASES: &str = "https://api.github.com/repos/git-for-windows/git/releases";
const SSH_RELEASES: &str = "https://api.github.com/repos/PowerShell/Win32-OpenSSH/releases";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Git,
    Ssh,
}

/// 安装到数据目录 `tools/{git,ssh}`，返回安装目录。`on_progress` 回调收 0..=99 的下载百分比。
pub fn install(tool: Tool, on_progress: &dyn Fn(u8)) -> Result<PathBuf> {
    let data = devkit_core::prefs::data_dir()
        .ok_or_else(|| anyhow!("尚未选择数据目录，请先完成首次启动引导"))?;
    let tools_dir = PathBuf::from(data).join("tools");
    std::fs::create_dir_all(&tools_dir).context("创建 tools 目录失败")?;

    let client = reqwest::blocking::Client::builder()
        .user_agent("devkit-toolchain-setup")
        .connect_timeout(Duration::from_secs(30))
        .build()?;
    let url = latest_asset_url(&client, tool)?;

    let staging = tools_dir.join(".staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let zip = staging.join(match tool {
        Tool::Git => "mingit.zip",
        Tool::Ssh => "openssh.zip",
    });

    download(&client, &url, &zip, on_progress)?;
    let extracted = staging.join("extracted");
    expand_archive(&zip, &extracted).with_context(|| format!("解压失败（{url}）"))?;

    let root = locate_root(&extracted, tool)?;
    let final_dir = tools_dir.join(match tool {
        Tool::Git => "git",
        Tool::Ssh => "ssh",
    });
    let _ = std::fs::remove_dir_all(&final_dir);
    std::fs::rename(&root, &final_dir).context("移动安装目录失败")?;
    let _ = std::fs::remove_dir_all(&staging);

    // 安装后校验：能被解析层找到才算成功
    let ok = match tool {
        Tool::Git => super::git_in(&tools_dir).is_some(),
        Tool::Ssh => super::ssh_in(&tools_dir).is_some(),
    };
    if !ok {
        return Err(anyhow!("安装后校验失败：未在 {} 解析到可执行文件", final_dir.display()));
    }
    Ok(final_dir)
}

/// 查询最新发布资产下载地址。`/latest` 无命中时回退翻 `/releases` 列表
/// （兼容常年只发 prerelease 的仓库，如 Win32-OpenSSH）。
fn latest_asset_url(client: &reqwest::blocking::Client, tool: Tool) -> Result<String> {
    let (api, matcher): (&str, fn(&str) -> bool) = match tool {
        Tool::Git => (GIT_RELEASES, |n| n.starts_with("MinGit-") && n.ends_with("-64-bit.zip")),
        Tool::Ssh => (SSH_RELEASES, |n| n == "OpenSSH-Win64.zip"),
    };
    for endpoint in [format!("{api}/latest"), format!("{api}?per_page=20")] {
        let resp = match client.get(&endpoint).send() {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !resp.status().is_success() {
            continue;
        }
        let json: serde_json::Value = match resp.json() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let releases: Vec<serde_json::Value> = match json {
            serde_json::Value::Array(v) => v,
            obj => vec![obj],
        };
        for rel in releases {
            let Some(assets) = rel.get("assets").and_then(|a| a.as_array()) else {
                continue;
            };
            for a in assets {
                let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if matcher(name) {
                    if let Some(u) = a.get("browser_download_url").and_then(|u| u.as_str()) {
                        return Ok(u.to_string());
                    }
                }
            }
        }
    }
    Err(anyhow!(
        "未找到可下载的发布资产（检查网络或代理）；也可手动下载 MinGit / OpenSSH-Win64 解压到数据目录 tools/ 下"
    ))
}

fn download(client: &reqwest::blocking::Client, url: &str, dest: &Path, on_progress: &dyn Fn(u8)) -> Result<()> {
    let mut resp = client.get(url).send().context("下载请求失败")?;
    if !resp.status().is_success() {
        anyhow::bail!("下载失败：HTTP {}", resp.status());
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(dest)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut got: u64 = 0;
    let mut last: u8 = 0;
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        got += n as u64;
        if total > 0 {
            let pct = ((got.min(total) * 100) / total) as u8;
            if pct != last {
                last = pct;
                on_progress(pct.min(99));
            }
        }
    }
    if total > 0 && got < total {
        anyhow::bail!("下载不完整（{got}/{total} 字节）");
    }
    Ok(())
}

/// 用 PowerShell Expand-Archive 解压（Win10+ 自带，避免引入 zip 依赖）。
fn expand_archive(zip: &Path, dest: &Path) -> Result<()> {
    let script = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        zip.display(),
        dest.display()
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// 在解压产物中定位工具根目录（容忍 zip 内多出的一层版本目录）。
fn locate_root(dir: &Path, tool: Tool) -> Result<PathBuf> {
    match tool {
        Tool::Git => {
            let Some(git_exe) = find_file(dir, "git.exe", 4) else {
                anyhow::bail!("压缩包内未找到 git.exe");
            };
            // cmd/git.exe 或 mingw64/bin/git.exe → 向上取 MinGit 根
            let mut p = git_exe.parent().unwrap_or(dir).to_path_buf();
            for _ in 0..2 {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if matches!(name.as_str(), "cmd" | "bin" | "mingw64" | "usr") {
                    p = p.parent().unwrap_or(dir).to_path_buf();
                } else {
                    break;
                }
            }
            Ok(p)
        }
        Tool::Ssh => {
            let Some(ssh_exe) = find_file(dir, "ssh.exe", 4) else {
                anyhow::bail!("压缩包内未找到 ssh.exe");
            };
            Ok(ssh_exe.parent().unwrap_or(dir).to_path_buf())
        }
    }
}

/// 深度受限的文件查找（先当前层文件，再子目录）。
fn find_file(dir: &Path, name: &str, depth: u8) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let rd = std::fs::read_dir(dir).ok()?;
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else {
            files.push(p);
        }
    }
    for f in files {
        if f.file_name().map(|n| n.eq_ignore_ascii_case(name)).unwrap_or(false) {
            return Some(f);
        }
    }
    for d in dirs {
        if let Some(f) = find_file(&d, name, depth - 1) {
            return Some(f);
        }
    }
    None
}
