//! Git 命令执行器：git 子进程（内置便携版优先，见 toolchain 模块）+ GIT_SSH_COMMAND 密钥注入。

use super::types::{BranchRow, ChangeEntry, CommitRow, DiffLine, DiffLineKind, RepoStatus, StashRow};
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// git 执行器（绑定一个仓库路径）。
pub struct GitRunner {
    pub path: PathBuf,
    /// 密钥注入：私钥绝对路径（Some 时远程操作注入 GIT_SSH_COMMAND）。
    pub ssh_key_path: Option<PathBuf>,
}

impl GitRunner {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), ssh_key_path: None }
    }

    /// 同步执行 git 子命令，返回 stdout。
    pub fn run(&self, args: &[&str], ssh: bool) -> Result<String> {
        let mut cmd = crate::proc::cmd(crate::toolchain::git());
        cmd.current_dir(&self.path).args(args);
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        cmd.env("core.quotepath", "false");
        if ssh {
            if let Some(key) = &self.ssh_key_path {
                let key_str = key.to_string_lossy().to_string();
                cmd.env(
                    "GIT_SSH_COMMAND",
                    format!(
                        "\"{}\" -i \"{key_str}\" -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new",
                        crate::toolchain::ssh()
                    ),
                );
            }
        }
        let out = cmd.output().map_err(|e| {
            anyhow!("git 执行失败：{e}（未检测到 Git？可在 设置→通用 一键安装便携版）")
        })?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("git {}: {}", args.join(" "), err.trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// 执行并容忍失败（返回 Result）。
    pub fn try_run(&self, args: &[&str], ssh: bool) -> Result<String> {
        self.run(args, ssh)
    }

    // ---------- 状态 ----------

    pub fn status(&self) -> Result<RepoStatus> {
        let out = self.run(&["status", "--porcelain=v2", "--branch"], false)?;
        let mut st = RepoStatus {
            branch: String::new(),
            ahead: 0,
            behind: 0,
            rebasing: false,
            conflicts: Vec::new(),
            unstaged: Vec::new(),
            staged: Vec::new(),
            untracked: Vec::new(),
            unpushed: 0,
        };
        for line in out.lines() {
            if let Some(rest) = line.strip_prefix("# branch.head ") {
                st.branch = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
                // +N -M
                let mut it = rest.split_whitespace();
                if let Some(a) = it.next().and_then(|s| s.strip_prefix('+')) {
                    st.ahead = a.parse().unwrap_or(0);
                }
                if let Some(b) = it.next().and_then(|s| s.strip_prefix('-')) {
                    st.behind = b.parse().unwrap_or(0);
                }
            } else if line.starts_with("1 ") || line.starts_with("2 ") {
                // changed entry: XY path
                let fields: Vec<&str> = line.splitn(9, ' ').collect();
                if fields.len() >= 9 {
                    let xy = fields[1];
                    let path = unquote_path(fields[8]);
                    classify(xy, &path, &mut st);
                }
            } else if let Some(rest) = line.strip_prefix("u ") {
                // unmerged: u XY from to path...
                let fields: Vec<&str> = rest.splitn(4, ' ').collect();
                if fields.len() >= 4 {
                    let path = unquote_path(fields[3]);
                    st.conflicts.push(path.clone());
                    st.unstaged.push(ChangeEntry {
                        path,
                        status: "UU".into(),
                        staged: false,
                        untracked: false,
                        conflict: true,
                    });
                }
            } else if let Some(rest) = line.strip_prefix("? ") {
                let path = unquote_path(rest.trim());
                st.untracked.push(ChangeEntry {
                    path,
                    status: "??".into(),
                    staged: false,
                    untracked: true,
                    conflict: false,
                });
            }
        }
        // rebasing 检测
        st.rebasing = self.path.join(".git").join("REBASE_HEAD").exists()
            || self.path.join(".git").join("rebase-merge").exists();
        // 未推送提交数
        st.unpushed = st.ahead;
        Ok(st)
    }

    pub fn remotes(&self) -> Result<Vec<(String, String)>> {
        let out = self.run(&["remote", "-v"], false)?;
        let mut seen = Vec::new();
        for line in out.lines() {
            let mut it = line.split_whitespace();
            if let (Some(name), Some(url)) = (it.next(), it.next()) {
                if !seen.iter().any(|(n, _): &(String, String)| n == name) {
                    seen.push((name.to_string(), url.to_string()));
                }
            }
        }
        Ok(seen)
    }

    pub fn set_remote_url(&self, name: &str, url: &str) -> Result<()> {
        // 已存在则 set-url，否则 add
        if self.remotes()?.iter().any(|(n, _)| n == name) {
            self.run(&["remote", "set-url", name, url], false)?;
        } else {
            self.run(&["remote", "add", name, url], false)?;
        }
        Ok(())
    }

    // ---------- 暂存 / 丢弃 ----------

    pub fn stage(&self, paths: &[String]) -> Result<()> {
        let mut args = vec!["add", "--"];
        args.extend(paths.iter().map(|s| s.as_str()));
        self.run(&args, false)?;
        Ok(())
    }

    pub fn unstage(&self, paths: &[String]) -> Result<()> {
        let mut args = vec!["restore", "--staged", "--"];
        args.extend(paths.iter().map(|s| s.as_str()));
        self.run(&args, false)?;
        Ok(())
    }

    pub fn discard(&self, paths: &[String]) -> Result<()> {
        // 未暂存丢弃（含未跟踪）
        for p in paths {
            let tracked = self
                .run(&["ls-files", "--error-unmatch", "--", p], false)
                .is_ok();
            if tracked {
                self.run(&["checkout", "--", p], false)?;
            } else {
                let _ = std::fs::remove_file(self.path.join(p));
            }
        }
        Ok(())
    }

    pub fn commit(&self, message: &str, amend: bool) -> Result<String> {
        let mut args = vec!["commit", "-m", message];
        if amend {
            args.push("--amend");
        }
        self.run(&args, false)
    }

    // ---------- diff ----------

    pub fn diff_unstaged(&self, path: &str) -> Result<String> {
        self.run(&["diff", "--", path], false)
    }

    pub fn diff_staged(&self, path: &str) -> Result<String> {
        self.run(&["diff", "--cached", "--", path], false)
    }

    pub fn diff_conflict(&self, path: &str) -> Result<String> {
        // 冲突文件直接读 | 合并标记
        let content = std::fs::read_to_string(self.path.join(path)).unwrap_or_default();
        Ok(content)
    }

    // ---------- 冲突 ----------

    pub fn take_theirs(&self, path: &str) -> Result<()> {
        self.run(&["checkout", "--theirs", "--", path], false)?;
        self.run(&["add", "--", path], false).map(|_| ())
    }

    pub fn take_ours(&self, path: &str) -> Result<()> {
        self.run(&["checkout", "--ours", "--", path], false)?;
        self.run(&["add", "--", path], false).map(|_| ())
    }

    pub fn mark_resolved(&self, path: &str) -> Result<()> {
        self.run(&["add", "--", path], false).map(|_| ())
    }

    // ---------- 分支 / 标签 ----------

    pub fn branches(&self) -> Result<Vec<BranchRow>> {
        let out = self.run(&["branch", "-a", "--format=%(HEAD)%00%(refname)"], false)?;
        let mut rows = Vec::new();
        for line in out.lines() {
            let mut it = line.split('\u{0}');
            let head = it.next().unwrap_or("").trim();
            let refname = it.next().unwrap_or("");
            let (remote, name) = if let Some(n) = refname.strip_prefix("refs/remotes/") {
                (true, n.to_string())
            } else if let Some(n) = refname.strip_prefix("refs/heads/") {
                (false, n.to_string())
            } else {
                continue;
            };
            rows.push(BranchRow { name, current: head == "*", remote });
        }
        Ok(rows)
    }

    pub fn create_branch(&self, name: &str, switch: bool) -> Result<()> {
        self.run(&["branch", name], false)?;
        if switch {
            self.run(&["switch", name], false)?;
        }
        Ok(())
    }

    pub fn switch_branch(&self, name: &str) -> Result<()> {
        self.run(&["switch", name], false).map(|_| ())
    }

    pub fn merge_branch(&self, name: &str) -> Result<String> {
        self.run(&["merge", name], false)
    }

    pub fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        if force {
            self.run(&["branch", "-D", name], false)?;
        } else {
            self.run(&["branch", "-d", name], false)?;
        }
        Ok(())
    }

    pub fn create_tag(&self, name: &str) -> Result<()> {
        self.run(&["tag", name], false).map(|_| ())
    }

    pub fn delete_tag(&self, name: &str) -> Result<()> {
        self.run(&["tag", "-d", name], false).map(|_| ())
    }

    // ---------- 历史 ----------

    pub fn log(&self, page: usize, per_page: usize) -> Result<Vec<CommitRow>> {
        let skip = page * per_page;
        let out = self.run(
            &[
                "log",
                &format!("-{}", per_page),
                &format!("--skip={}", skip),
                "--pretty=format:%H%x00%an%x00%ad%x00%s",
                "--date=short",
            ],
            false,
        )?;
        Ok(out
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\u{0}');
                Some(CommitRow {
                    hash: it.next()?.to_string(),
                    author: it.next().unwrap_or("").to_string(),
                    date: it.next().unwrap_or("").to_string(),
                    subject: it.next().unwrap_or("").to_string(),
                })
            })
            .collect())
    }

    // ---------- 远程操作 ----------

    pub fn fetch(&self) -> Result<String> {
        self.run(&["fetch", "--all"], true)
    }

    pub fn pull(&self) -> Result<String> {
        self.run(&["pull"], true)
    }

    pub fn push(&self) -> Result<String> {
        self.run(&["push"], true)
    }

    // ---------- 高级 ----------

    pub fn cherry_pick(&self, hash: &str) -> Result<String> {
        self.run(&["cherry-pick", hash], false)
    }

    pub fn reset(&self, hash: &str, mode: &str) -> Result<String> {
        self.run(&["reset", mode, hash], false)
    }

    pub fn rebase_abort(&self) -> Result<String> {
        self.run(&["rebase", "--abort"], false)
    }

    pub fn rebase_onto(&self, target: &str) -> Result<String> {
        self.run(&["rebase", target], false)
    }

    // ---------- Stash ----------

    pub fn stash_list(&self) -> Result<Vec<StashRow>> {
        let out = self.run(&["stash", "list"], false)?;
        Ok(out
            .lines()
            .enumerate()
            .map(|(i, l)| {
                // stash@{0}: WIP on main: abc message
                let msg = l.splitn(3, ": ").nth(2).unwrap_or(l).to_string();
                StashRow { index: i, message: msg }
            })
            .collect())
    }

    pub fn stash_push(&self, message: Option<&str>) -> Result<String> {
        match message {
            Some(m) => self.run(&["stash", "push", "-m", m], false),
            None => self.run(&["stash", "push"], false),
        }
    }

    pub fn stash_pop(&self, index: usize) -> Result<String> {
        self.run(&["stash", "pop", &format!("stash@{{{index}}}")], false)
    }

    pub fn stash_apply(&self, index: usize) -> Result<String> {
        self.run(&["stash", "apply", &format!("stash@{{{index}}}")], false)
    }

    pub fn stash_drop(&self, index: usize) -> Result<String> {
        self.run(&["stash", "drop", &format!("stash@{{{index}}}")], false)
    }
}

fn classify(xy: &str, path: &str, st: &mut RepoStatus) {
    let x = xy.as_bytes().first().copied().unwrap_or(b'.');
    let y = xy.as_bytes().get(1).copied().unwrap_or(b'.');
    if x != b'.' && x != b'?' {
        st.staged.push(ChangeEntry {
            path: path.to_string(),
            status: xy.to_string(),
            staged: true,
            untracked: false,
            conflict: false,
        });
    }
    if y != b'.' && y != b'?' {
        st.unstaged.push(ChangeEntry {
            path: path.to_string(),
            status: xy.to_string(),
            staged: false,
            untracked: false,
            conflict: y == b'U',
        });
    }
}

/// `git status` 对含中文/空格路径的引用还原。
fn unquote_path(s: &str) -> String {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        // 八进制转义还原
        let bytes = inner.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 3 < bytes.len() {
                let oct = std::str::from_utf8(&bytes[i + 1..i + 4]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(oct, 8) {
                    out.push(v);
                    i += 4;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).to_string()
    } else {
        s.to_string()
    }
}

/// 解析 unified diff 为逐行结构。
pub fn parse_diff(diff: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let mut left: u32 = 0;
    let mut right: u32 = 0;
    for line in diff.lines() {
        if let Some(h) = line.strip_prefix("@@") {
            // @@ -a,b +c,d @@
            let mut it = h.split_whitespace();
            it.next();
            let l = it.next().unwrap_or("-0,0");
            let r = it.next().unwrap_or("+0,0");
            left = l
                .trim_start_matches('-')
                .split(',')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            right = r
                .trim_start_matches('+')
                .split(',')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            out.push(DiffLine { kind: DiffLineKind::Hunk, left_no: None, right_no: None, text: format!("@@{h}") });
        } else if line.starts_with("diff ") || line.starts_with("index ") || line.starts_with("--- ") || line.starts_with("+++ ") {
            out.push(DiffLine { kind: DiffLineKind::Meta, left_no: None, right_no: None, text: line.to_string() });
        } else if let Some(rest) = line.strip_prefix('+') {
            out.push(DiffLine { kind: DiffLineKind::Add, left_no: None, right_no: Some(right), text: rest.to_string() });
            right += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            out.push(DiffLine { kind: DiffLineKind::Del, left_no: Some(left), right_no: None, text: rest.to_string() });
            left += 1;
        } else if let Some(rest) = line.strip_prefix(' ') {
            out.push(DiffLine { kind: DiffLineKind::Context, left_no: Some(left), right_no: Some(right), text: rest.to_string() });
            left += 1;
            right += 1;
        } else if line.starts_with("\\ No newline") {
            out.push(DiffLine { kind: DiffLineKind::Meta, left_no: None, right_no: None, text: line.to_string() });
        }
    }
    out
}

/// ssh-keygen 生成密钥对。
pub struct GeneratedKey {
    pub public_key: String,
    pub private_path: PathBuf,
    pub fingerprint: String,
}

pub fn ssh_keygen(
    keys_dir: &Path,
    key_id: uuid::Uuid,
    algorithm: &str,
    comment: Option<&str>,
    passphrase: Option<&str>,
) -> Result<GeneratedKey> {
    std::fs::create_dir_all(keys_dir)?;
    let file = keys_dir.join(key_id.to_string());
    // 已存在则先删
    let _ = std::fs::remove_file(&file);
    let mut cmd = crate::proc::cmd(crate::toolchain::ssh_keygen());
    cmd.arg("-t");
    cmd.arg(if algorithm.eq_ignore_ascii_case("rsa") { "rsa" } else { "ed25519" });
    if algorithm.eq_ignore_ascii_case("rsa") {
        cmd.arg("-b").arg("4096");
    }
    cmd.arg("-f").arg(&file);
    cmd.arg("-N").arg(passphrase.unwrap_or(""));
    cmd.arg("-C").arg(comment.unwrap_or("devkit"));
    cmd.arg("-q");
    let out = cmd.output().map_err(|e| anyhow!("ssh-keygen 启动失败：{e}（未检测到 OpenSSH？可在 设置→通用 一键安装便携版）"))?;
    if !out.status.success() {
        anyhow::bail!("ssh-keygen 失败：{}", String::from_utf8_lossy(&out.stderr));
    }
    let public_key = std::fs::read_to_string(keys_dir.join(format!("{}.pub", key_id)))?;
    let _ = std::fs::remove_file(keys_dir.join(format!("{}.pub", key_id)));
    let fp = crate::proc::cmd(crate::toolchain::ssh_keygen())
        .arg("-lf")
        .arg(&file)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string())
        .unwrap_or_default();
    Ok(GeneratedKey { public_key: public_key.trim().to_string(), private_path: file, fingerprint: fp })
}

/// git 是否可用（内置便携版 → PATH，见 toolchain）。
pub fn git_available() -> bool {
    crate::toolchain::git_available()
}

/// 克隆仓库（实时输出经回调推送）。
pub fn clone(url: &str, parent_dir: &Path, key: Option<&Path>, on_line: impl Fn(&str)) -> Result<PathBuf> {
    let mut cmd = crate::proc::cmd(crate::toolchain::git());
    cmd.arg("clone").arg(url).current_dir(parent_dir);
    if let Some(k) = key {
        let k_str = k.to_string_lossy().to_string();
        cmd.env("GIT_SSH_COMMAND", format!("\"{}\" -i \"{k_str}\" -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new", crate::toolchain::ssh()));
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| anyhow!("git 启动失败：{e}"))?;
    use std::io::{BufRead, BufReader};
    if let Some(stderr) = child.stderr.take() {
        for line in BufReader::new(stderr).lines() {
            if let Ok(l) = line {
                on_line(&l);
            }
        }
    }
    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("克隆失败");
    }
    // 从 URL 推断目录名
    let name = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .trim_end_matches(".git");
    Ok(parent_dir.join(name))
}

/// 默认超时常量（供 UI 层提示）。
pub const RUN_TIMEOUT: Duration = Duration::from_secs(60);
