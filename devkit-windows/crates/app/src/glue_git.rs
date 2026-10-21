//! Git 工具粘合层：仓库登记 ↔ GitRunner ↔ UI。
//!
//! 设计要点：
//!   · 所有 git 子进程都在后台线程执行，UI 线程只负责渲染（否则 pull / push 会卡死界面）。
//!   · 后台结果通过 crossbeam channel 回传，由一个 80ms 轮询定时器取回并写回 UI。
//!   · 每个标签各自持有一份运行时（GitRun），互不干扰。

use crate::shell::{current_window, Ctx};
use crate::{GitBranchRow, GitChangeRow, GitCommitRow, GitDiffRow, GitKeyRow, GitRepoNode, GitStashRow, GitUI};
use anyhow::Result;
use devkit_core::tool::ids;
use devkit_tools::git::runner::{parse_diff, ssh_keygen, GitRunner};
use devkit_tools::git::store as gstore;
use devkit_tools::git::types::{BranchRow, ChangeEntry, CommitRow, DiffLineKind, GitKey, GitRepo, StashRow};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Git 标签运行时。
#[derive(Default)]
pub struct GitRun {
    /// 打开的仓库 ID。
    pub repo_id: Option<String>,
    pub repo_path: String,
    pub expanded: HashSet<String>,
    pub history_page: usize,
    pub selected_section: i32,
    pub selected_index: i32,
}

/// 随会话快照持久化的 Git 状态。
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct GitToolState {
    #[serde(default)]
    pub repo_id: Option<String>,
}

/// 后台采集到的一份仓库快照（一次 git 调用序列的结果）。
pub struct GitSnapshot {
    branch: String,
    ahead: i32,
    behind: i32,
    rebasing: bool,
    conflicts: Vec<ChangeEntry>,
    unstaged: Vec<ChangeEntry>,
    staged: Vec<ChangeEntry>,
    untracked: Vec<ChangeEntry>,
    remote_url: String,
    branches: Vec<BranchRow>,
    stashes: Vec<StashRow>,
    unpushed: Vec<CommitRow>,
}

/// 后台任务：返回 Ok 表示成功，Err 为可展示的错误文本。
type Job = Box<dyn FnOnce(&GitRunner) -> Result<String> + Send>;

/// 后台线程 → UI 线程的消息。
pub enum GitMsg {
    /// 一个 git 操作完成。
    Job { ok: String, res: Result<String, String> },
    /// 仓库快照采集完成。
    Snap(GitSnapshot),
    /// 提交历史分页加载完成。
    Hist { page: usize, rows: Vec<CommitRow> },
    /// 差异行加载完成。
    Diff(Vec<GitDiffRow>),
    /// 克隆完成。
    CloneDone {
        res: Result<String, String>,
        path: String,
        alias: String,
    },
}

/// 取得全局消息通道（发送端可克隆）。
pub fn git_sender(ctx: &Rc<Ctx>) -> Option<crossbeam_channel::Sender<GitMsg>> {
    ctx.git_chan.borrow().clone()
}

/// 轮询后台消息（由 shell 启动的唯一定时器驱动）。
pub fn poll_git(ctx: &Rc<Ctx>) {
    let mut msgs = Vec::new();
    {
        let rxs = ctx.git_rx.borrow();
        if let Some(rx) = rxs.as_ref() {
            while let Ok(m) = rx.try_recv() {
                msgs.push(m);
                if msgs.len() > 32 {
                    break;
                }
            }
        }
    }
    for m in msgs {
        match m {
            GitMsg::Job { ok, res } => handle_job_result(ctx, ok, res),
            GitMsg::Snap(snap) => apply_snapshot(ctx, snap),
            GitMsg::Hist { page, rows } => apply_history(ctx, page, rows),
            GitMsg::Diff(rows) => {
                if let Some(w_g) = current_window() {
                    w_g.global::<GitUI>()
                        .set_diff(ModelRc::from(Rc::new(VecModel::from(rows))));
                }
            }
            GitMsg::CloneDone { res, path, alias } => {
                set_busy(ctx, false);
                match res {
                    Ok(out) => {
                        if let Some(w_g) = current_window() {
                            let s = w_g.global::<GitUI>();
                            s.set_clone_visible(false);
                            s.set_console_text(out.into());
                        }
                        match register_repo_at(ctx, &path, &alias) {
                            Ok(()) => {
                                rebuild_repos(ctx);
                                toast(ctx, 0, "克隆完成");
                            }
                            Err(e) => {
                                toast(ctx, 1, &truncate(&format!("克隆完成但登记失败：{e}"), 90))
                            }
                        }
                    }
                    Err(e) => toast(ctx, 1, &truncate(&format!("克隆失败：{e}"), 120)),
                }
            }
        }
    }
}

/// 处理一个 git 操作的结果。
fn handle_job_result(ctx: &Rc<Ctx>, ok: String, res: Result<String, String>) {
    set_busy(ctx, false);
    match res {
        Ok(out) => {
            if !out.is_empty() {
                show_console(ctx, &out);
            }
            toast(ctx, 0, &ok);
        }
        Err(msg) => {
            let conflict = msg.contains("CONFLICT") || msg.contains("conflict");
            toast(ctx, 1, &truncate(&msg, 110));
            if conflict {
                let first = current_conflict_path(ctx);
                refresh_all(ctx);
                if let Some(p) = first {
                    if let Some(w_g) = current_window() {
                        let s = w_g.global::<GitUI>();
                        s.set_selected_list(0);
                        s.set_diff_path(p.into());
                    }
                }
                return;
            }
        }
    }
    refresh_all(ctx);
}

pub fn wire_git_callbacks(ctx: Rc<Ctx>) {
    let Some(ui) = current_window() else { return };
    let git_ui = ui.global::<GitUI>();

    // ---- 仓库登记 ----
    {
        git_ui.on_browse_repo_path(move || {
            if let Some(dir) = rfd::FileDialog::new().set_title("选择仓库目录").pick_folder() {
                if let Some(w_g) = current_window() {
                    let s = w_g.global::<GitUI>();
                    s.set_repo_path(dir.to_string_lossy().to_string().into());
                }
            }
        });
    }
    {
        // 「登记仓库…」：直接弹出目录选择器 → 校验 → 登记 → 打开。
        let ctx2 = ctx.clone();
        git_ui.on_open_repo_path(move || {
            let Some(dir) = rfd::FileDialog::new()
                .set_title("选择 Git 仓库目录")
                .pick_folder()
            else {
                return;
            };
            let path = dir.to_string_lossy().to_string();
            if !Path::new(&path).join(".git").exists() {
                toast(&ctx2, 1, "该目录不是 Git 仓库（缺少 .git）");
                return;
            }
            if let Err(e) = register_repo_at(&ctx2, &path, "") {
                toast(&ctx2, 1, &truncate(&format!("{e}"), 90));
                return;
            }
            // 打开刚登记的仓库
            let Some(db) = ctx2.db() else { return };
            let repos = gstore::list_repos(&db).unwrap_or_default();
            if let Some(row) = repos.iter().rev().find(|r| r.repo.path == path) {
                open_repo(&ctx2, &row.id);
            }
            rebuild_repos(&ctx2);
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_register_repo(move || {
            let _ = register_repo(&ctx2);
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_repo_selected(move |index| {
            let Some((id, is_folder)) = repo_node_at(&ctx2, index as usize) else { return };
            if is_folder {
                let active = ctx2.active.borrow().clone();
                if let Some(run) = ctx2.git_runs.borrow_mut().get_mut(active.as_str()) {
                    if run.expanded.contains(&id) {
                        run.expanded.remove(&id);
                    } else {
                        run.expanded.insert(id);
                    }
                }
                rebuild_repos(&ctx2);
                return;
            }
            open_repo(&ctx2, &id);
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_repo_deleted(move |index| {
            let Some((id, is_folder)) = repo_node_at(&ctx2, index as usize) else { return };
            let Some(db) = ctx2.db() else { return };
            let r = if is_folder {
                gstore::delete_folder_cascade(&db, &id)
            } else {
                gstore::delete_repo(&db, &id)
            };
            if r.is_ok() {
                let active = ctx2.active.borrow().clone();
                if let Some(run) = ctx2.git_runs.borrow_mut().get_mut(active.as_str()) {
                    if run.repo_id.as_deref() == Some(id.as_str()) {
                        run.repo_id = None;
                    }
                }
                sync_repo_open(&ctx2);
                rebuild_repos(&ctx2);
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_close_repo(move || {
            let active = ctx2.active.borrow().clone();
            if let Some(run) = ctx2.git_runs.borrow_mut().get_mut(active.as_str()) {
                run.repo_id = None;
                run.repo_path.clear();
            }
            sync_repo_open(&ctx2);
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_refresh_all(move || refresh_all(&ctx2));
    }

    // ---- 变更操作 ----
    {
        let ctx2 = ctx.clone();
        git_ui.on_select_change(move |section, index| {
            if let Some(w_g) = current_window() {
                let s = w_g.global::<GitUI>();
                s.set_selected_list(section);
                s.set_selected_index(index);
            }
            let active = ctx2.active.borrow().clone();
            if let Some(run) = ctx2.git_runs.borrow_mut().get_mut(active.as_str()) {
                run.selected_section = section;
                run.selected_index = index;
            }
            show_diff(&ctx2, section, index);
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_stage(move |section, index| {
            let Some(path) = change_path_at(&ctx2, section, index) else { return };
            git_op(
                &ctx2,
                Box::new(move |r| r.stage(&[path]).map(|_| String::new())),
                "已暂存",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_unstage(move |section, index| {
            let Some(path) = change_path_at(&ctx2, section, index) else { return };
            git_op(
                &ctx2,
                Box::new(move |r| r.unstage(&[path]).map(|_| String::new())),
                "已取消暂存",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_stage_all(move |_| {
            let paths = all_paths(&ctx2, true);
            if !paths.is_empty() {
                git_op(
                    &ctx2,
                    Box::new(move |r| r.stage(&paths).map(|_| String::new())),
                    "已全部暂存",
                );
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_unstage_all(move |_| {
            let paths = all_paths(&ctx2, false);
            if !paths.is_empty() {
                git_op(
                    &ctx2,
                    Box::new(move |r| r.unstage(&paths).map(|_| String::new())),
                    "已全部取消暂存",
                );
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_discard(move |section, index| {
            let Some(path) = change_path_at(&ctx2, section, index) else { return };
            git_op(
                &ctx2,
                Box::new(move |r| r.discard(&[path]).map(|_| String::new())),
                "已丢弃",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_commit(move || {
            let Some(w_g) = current_window() else { return };
            let s = w_g.global::<GitUI>();
            let msg = s.get_commit_message().trim().to_string();
            let amend = s.get_amend();
            if msg.is_empty() && !amend {
                toast(&ctx2, 1, "提交信息不能为空");
                return;
            }
            s.set_commit_message("".into());
            git_op(
                &ctx2,
                Box::new(move |r| r.commit(&msg, amend).map(|_| String::new())),
                "提交成功",
            );
        });
    }

    // ---- 分支 ----
    {
        let ctx2 = ctx.clone();
        git_ui.on_branch_new(move || {
            let Some(w_g) = current_window() else { return };
            let s = w_g.global::<GitUI>();
            let name = s.get_new_branch_name().trim().to_string();
            if name.is_empty() {
                return;
            }
            s.set_new_branch_name("".into());
            git_op(
                &ctx2,
                Box::new(move |r| {
                    r.create_branch(&name, true)?;
                    Ok(format!("已创建并切换到 {name}"))
                }),
                "分支已创建",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_branch_switch(move |index| {
            let Some(name) = branch_at(&ctx2, index as usize) else { return };
            git_op(
                &ctx2,
                Box::new(move |r| {
                    r.switch_branch(&name)?;
                    Ok(format!("已切换到 {name}"))
                }),
                "已切换",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_branch_delete(move |index, force| {
            let Some(name) = branch_at(&ctx2, index as usize) else { return };
            git_op(
                &ctx2,
                Box::new(move |r| {
                    r.delete_branch(&name, force)?;
                    Ok(format!("已删除 {name}"))
                }),
                "分支已删除",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_merge_branch(move |index| {
            let Some(name) = branch_at(&ctx2, index as usize) else { return };
            let msg = format!("合并 {name}");
            git_op(
                &ctx2,
                Box::new(move |r| r.merge_branch(&name).map(|_| String::new())),
                &msg,
            );
        });
    }

    // ---- Stash ----
    {
        let ctx2 = ctx.clone();
        git_ui.on_stash_push(move || {
            git_op(
                &ctx2,
                Box::new(|r| r.stash_push(None).map(|_| String::new())),
                "Stash 完成",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_stash_pop(move |index| {
            git_op(
                &ctx2,
                Box::new(move |r| r.stash_pop(index as usize).map(|_| String::new())),
                "Stash Pop",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_stash_drop(move |index| {
            git_op(
                &ctx2,
                Box::new(move |r| r.stash_drop(index as usize).map(|_| String::new())),
                "Stash Drop",
            );
        });
    }

    // ---- 历史 ----
    {
        let ctx2 = ctx.clone();
        git_ui.on_history_menu(move |index, action| {
            let Some(hash) = commit_hash_at(&ctx2, index as usize) else { return };
            match action {
                0 => git_op(
                    &ctx2,
                    Box::new(move |r| r.cherry_pick(&hash).map(|_| String::new())),
                    "已 cherry-pick",
                ),
                1 => git_op(
                    &ctx2,
                    Box::new(move |r| r.reset(&hash, "--soft").map(|_| String::new())),
                    "已重置（保留改动）",
                ),
                2 => git_op(
                    &ctx2,
                    Box::new(move |r| r.reset(&hash, "--mixed").map(|_| String::new())),
                    "已重置（不暂存）",
                ),
                3 => git_op(
                    &ctx2,
                    Box::new(move |r| r.reset(&hash, "--hard").map(|_| String::new())),
                    "已重置（丢弃改动）",
                ),
                _ => {
                    if let Some(w_g) = current_window() {
                        let s = w_g.global::<GitUI>();
                        if let Ok(mut c) = arboard::Clipboard::new() {
                            let _ = c.set_text(hash.clone());
                        }
                        s.set_toast("已复制完整哈希".into());
                        s.set_toast_kind(0);
                    }
                }
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_history_more(move || {
            let active = ctx2.active.borrow().clone();
            if let Some(run) = ctx2.git_runs.borrow_mut().get_mut(active.as_str()) {
                run.history_page += 1;
            }
            reload_history(&ctx2, true);
        });
    }

    // ---- 远程 ----
    {
        let ctx2 = ctx.clone();
        git_ui.on_fetch(move || {
            git_op(
                &ctx2,
                Box::new(|r| r.fetch().map(|_| String::new())),
                "Fetch 完成",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_pull(move || pull_impl(&ctx2, false));
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_push(move || push_impl(&ctx2));
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_pull_push(move || pull_impl(&ctx2, true));
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_rebase_abort(move || {
            git_op(
                &ctx2,
                Box::new(|r| r.rebase_abort().map(|_| String::new())),
                "变基已中止",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_edit_remote_url(move |url| {
            let url = url.trim().to_string();
            if url.is_empty() {
                return;
            }
            git_op(
                &ctx2,
                Box::new(move |r| {
                    r.set_remote_url("origin", &url).map(|_| String::new())
                }),
                "远程地址已更新",
            );
        });
    }

    // ---- 冲突 ----
    {
        let ctx2 = ctx.clone();
        git_ui.on_conflict_ours(move |index| {
            let Some(path) = conflict_path_at(&ctx2, index as usize) else { return };
            git_op(
                &ctx2,
                Box::new(move |r| r.take_ours(&path).map(|_| String::new())),
                "已采用我方",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_conflict_theirs(move |index| {
            let Some(path) = conflict_path_at(&ctx2, index as usize) else { return };
            git_op(
                &ctx2,
                Box::new(move |r| r.take_theirs(&path).map(|_| String::new())),
                "已采用对方",
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_conflict_resolved(move |index| {
            let Some(path) = conflict_path_at(&ctx2, index as usize) else { return };
            git_op(
                &ctx2,
                Box::new(move |r| r.mark_resolved(&path).map(|_| String::new())),
                "已标记解决",
            );
        });
    }

    // ---- 克隆 ----
    {
        git_ui.on_clone_start(move || {
            if let Some(w_g) = current_window() {
                let s = w_g.global::<GitUI>();
                s.set_clone_url("".into());
                s.set_clone_alias("".into());
                s.set_clone_parent(default_clone_parent().into());
                s.set_console_text("".into());
                s.set_clone_visible(true);
            }
        });
    }
    {
        git_ui.on_clone_cancel(move || {
            if let Some(w_g) = current_window() {
                w_g.global::<GitUI>().set_clone_visible(false);
            }
        });
    }
    {
        git_ui.on_clone_browse(move || {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title("选择克隆目标目录")
                .pick_folder()
            {
                if let Some(w_g) = current_window() {
                    w_g.global::<GitUI>()
                        .set_clone_parent(dir.to_string_lossy().to_string().into());
                }
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_clone_confirm(move || {
            let Some(w_g) = current_window() else { return };
            let s = w_g.global::<GitUI>();
            let url = s.get_clone_url().trim().to_string();
            let parent = s.get_clone_parent().trim().to_string();
            let alias = s.get_clone_alias().trim().to_string();
            if url.is_empty() {
                toast(&ctx2, 1, "请填写仓库 URL");
                return;
            }
            if parent.is_empty() {
                toast(&ctx2, 1, "请选择目标目录");
                return;
            }
            let name = repo_name_from_url(&url);
            let dest = Path::new(&parent).join(&name);
            set_busy(&ctx2, true);
            if let Some(w_g) = current_window() {
                w_g.global::<GitUI>().set_console_text(format!("正在克隆 {url} …\n").into());
            }
            let _ctx3 = ctx2.clone();
            let url2 = url.clone();
            let dest_str = dest.to_string_lossy().to_string();
            let Some(tx) = git_sender(&ctx2) else { return };
            std::thread::spawn(move || {
                let runner = GitRunner::new(&parent);
                let args = ["clone", url2.as_str(), name.as_str()];
                let res = runner.run(&args, true);
                let _ = tx.send(GitMsg::CloneDone {
                    res: res.map_err(|e| format!("{e}")),
                    path: dest_str,
                    alias: alias.clone(),
                });
            });
        });
    }

    // ---- 密钥 ----
    {
        let ctx2 = ctx.clone();
        git_ui.on_keys_open(move || {
            if let Some(w_g) = current_window() {
                w_g.global::<GitUI>().set_keys_visible(true);
            }
            reload_keys(&ctx2);
        });
    }
    {
        git_ui.on_keys_close(move || {
            if let Some(w_g) = current_window() {
                w_g.global::<GitUI>().set_keys_visible(false);
            }
        });
    }
    {
        git_ui.on_console_close(move || {
            if let Some(w_g) = current_window() {
                w_g.global::<GitUI>().set_console_visible(false);
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_key_generate(move || {
            let Some(db) = ctx2.db() else { return };
            let data_dir = devkit_core::prefs::data_dir().unwrap_or_default();
            let keys_dir = Path::new(&data_dir).join("git_keys");
            let id = uuid::Uuid::new_v4();
            match ssh_keygen(&keys_dir, id, "ed25519", Some("devkit"), None) {
                Ok(g) => {
                    let key = GitKey {
                        id,
                        name: format!("ed25519-{}", chrono::Local::now().format("%m%d-%H%M")),
                        algorithm: "Ed25519".into(),
                        comment: Some("devkit".into()),
                        public_key: g.public_key,
                        private_file: Some(id.to_string()),
                        has_passphrase: false,
                        imported: false,
                        created_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                    };
                    let _ = gstore::upsert_key(&db, &key);
                    reload_keys(&ctx2);
                    toast(&ctx2, 0, "密钥已生成");
                }
                Err(e) => toast(&ctx2, 1, &truncate(&format!("{e}"), 90)),
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_key_import(move || {
            if let Some(file) = rfd::FileDialog::new().set_title("导入私钥").pick_file() {
                let Some(db) = ctx2.db() else { return };
                let data_dir = devkit_core::prefs::data_dir().unwrap_or_default();
                let keys_dir = Path::new(&data_dir).join("git_keys");
                let _ = std::fs::create_dir_all(&keys_dir);
                let id = uuid::Uuid::new_v4();
                let dest = keys_dir.join(id.to_string());
                match std::fs::copy(&file, &dest) {
                    Ok(_) => {
                        let fp = devkit_tools::proc::cmd(devkit_tools::toolchain::ssh_keygen())
                            .arg("-lf")
                            .arg(&dest)
                            .output()
                            .ok()
                            .map(|o| {
                                String::from_utf8_lossy(&o.stdout)
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .to_string()
                            })
                            .unwrap_or_default();
                        let name = file
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let key = GitKey {
                            id,
                            name,
                            algorithm: "Imported".into(),
                            comment: None,
                            public_key: fp,
                            private_file: Some(id.to_string()),
                            has_passphrase: false,
                            imported: true,
                            created_at: chrono::Utc::now(),
                            updated_at: chrono::Utc::now(),
                        };
                        let _ = gstore::upsert_key(&db, &key);
                        reload_keys(&ctx2);
                        toast(&ctx2, 0, "私钥已导入");
                    }
                    Err(e) => toast(&ctx2, 1, &format!("导入失败：{e}")),
                }
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_key_delete(move |index| {
            let Some(id) = key_id_at(&ctx2, index as usize) else { return };
            let Some(db) = ctx2.db() else { return };
            let _ = gstore::delete_key(&db, &id);
            let data_dir = devkit_core::prefs::data_dir().unwrap_or_default();
            let _ = std::fs::remove_file(Path::new(&data_dir).join("git_keys").join(&id));
            reload_keys(&ctx2);
        });
    }
    {
        git_ui.on_key_copy_public(move |index| {
            if let Some(w_g) = current_window() {
                let s = w_g.global::<GitUI>();
                if let Some(k) = s.get_keys().row_data(index as usize) {
                    if let Ok(mut c) = arboard::Clipboard::new() {
                        let _ = c.set_text(k.fingerprint.to_string());
                    }
                    s.set_toast("已复制公钥".into());
                    s.set_toast_kind(0);
                }
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_bind_key(move |index| {
            let Some(key_id) = key_id_at(&ctx2, index as usize) else { return };
            let Some(db) = ctx2.db() else { return };
            let active = ctx2.active.borrow().clone();
            let repo_id = ctx2
                .git_runs
                .borrow()
                .get(active.as_str())
                .and_then(|r| r.repo_id.clone());
            let Some(repo_id) = repo_id else {
                toast(&ctx2, 1, "请先打开一个仓库");
                return;
            };
            let repos = gstore::list_repos(&db).unwrap_or_default();
            if let Some(mut row) = repos.into_iter().find(|r| r.id == repo_id) {
                row.repo.key_id = uuid::Uuid::parse_str(&key_id).ok();
                let name = {
                    let keys = gstore::list_keys(&db).unwrap_or_default();
                    keys.iter()
                        .find(|k| k.id.to_string() == key_id)
                        .map(|k| k.name.clone())
                        .unwrap_or_default()
                };
                if gstore::upsert_repo(&db, &row.repo).is_ok() {
                    if let Some(w_g) = current_window() {
                        w_g.global::<GitUI>().set_bound_key_name(name.into());
                    }
                    reload_keys(&ctx2);
                    toast(&ctx2, 0, "密钥已绑定");
                }
            }
        });
    }
    {
        // 取消绑定：清空仓库的 key_id
        let ctx2 = ctx.clone();
        git_ui.on_unbind_key(move || {
            let Some(db) = ctx2.db() else { return };
            let active = ctx2.active.borrow().clone();
            let repo_id = ctx2
                .git_runs
                .borrow()
                .get(active.as_str())
                .and_then(|r| r.repo_id.clone());
            let Some(repo_id) = repo_id else { return };
            let repos = gstore::list_repos(&db).unwrap_or_default();
            if let Some(mut row) = repos.into_iter().find(|r| r.id == repo_id) {
                row.repo.key_id = None;
                let _ = gstore::upsert_repo(&db, &row.repo);
                if let Some(w_g) = current_window() {
                    w_g.global::<GitUI>().set_bound_key_name("".into());
                }
                reload_keys(&ctx2);
                toast(&ctx2, 0, "已取消绑定密钥");
            }
        });
    }

    sync_repo_open(&ctx);
    rebuild_repos(&ctx);
}

// ---------- 工具函数 ----------

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn default_clone_parent() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string())
}

/// 从仓库 URL 推导目录名（对齐 macOS 端 repoName(from:)）。
fn repo_name_from_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let last = last.rsplit(':').next().unwrap_or(last);
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty() {
        "repo".to_string()
    } else {
        name.to_string()
    }
}

fn current_repo_info(ctx: &Rc<Ctx>) -> (String, Option<PathBuf>) {
    let Some(db) = ctx.db() else { return (String::new(), None) };
    let active = ctx.active.borrow().clone();
    let repo_id = ctx
        .git_runs
        .borrow()
        .get(active.as_str())
        .and_then(|r| r.repo_id.clone());
    let Some(repo_id) = repo_id else { return (String::new(), None) };
    let repos = gstore::list_repos(&db).unwrap_or_default();
    let Some(row) = repos.into_iter().find(|r| r.id == repo_id) else {
        return (String::new(), None);
    };
    let key = row.repo.key_id.and_then(|kid| {
        let keys = gstore::list_keys(&db).unwrap_or_default();
        keys.iter()
            .find(|k| k.id == kid)
            .and_then(|k| k.private_file.clone())
    });
    let data_dir = devkit_core::prefs::data_dir().unwrap_or_default();
    let key_path = key.map(|f| Path::new(&data_dir).join("git_keys").join(f));
    (row.repo.path, key_path)
}

/// 在后台线程执行一个 git 操作；完成后回到 UI 线程提示并刷新。
fn git_op(ctx: &Rc<Ctx>, job: Job, ok_msg: &str) {
    let (path, key) = current_repo_info(ctx);
    if path.is_empty() {
        return;
    }
    let Some(tx) = git_sender(ctx) else { return };
    set_busy(ctx, true);
    let ok = ok_msg.to_string();
    std::thread::spawn(move || {
        let mut runner = GitRunner::new(path);
        runner.ssh_key_path = key;
        let res = job(&runner).map_err(|e| format!("{e}"));
        let _ = tx.send(GitMsg::Job { ok, res });
    });
}

fn current_conflict_path(_ctx: &Rc<Ctx>) -> Option<String> {
    let Some(ui) = current_window() else { return None };
    let s = ui.global::<GitUI>();
    let model = s.get_changes_conflict();
    model.row_data(0).map(|r| r.path.to_string())
}

fn set_busy(ctx: &Rc<Ctx>, busy: bool) {
    if let Some(w_g) = current_window() {
        w_g.global::<GitUI>().set_busy(busy);
    }
    let _ = ctx;
}

fn toast(ctx: &Rc<Ctx>, kind: i32, msg: &str) {
    if let Some(w_g) = current_window() {
        let s = w_g.global::<GitUI>();
        s.set_toast_kind(kind);
        s.set_toast(msg.into());
    }
    let _ = ctx;
}

fn show_console(ctx: &Rc<Ctx>, text: &str) {
    if let Some(w_g) = current_window() {
        let s = w_g.global::<GitUI>();
        s.set_console_visible(true);
        s.set_console_text(text.into());
    }
    let _ = ctx;
}

fn repo_node_at(_ctx: &Rc<Ctx>, index: usize) -> Option<(String, bool)> {
    current_window().and_then(|w_g| {
        let s = w_g.global::<GitUI>();
        s.get_repos()
            .row_data(index)
            .map(|n| (n.id.to_string(), n.is_folder))
    })
}

fn change_path_at(_ctx: &Rc<Ctx>, section: i32, index: i32) -> Option<String> {
    let w_g = current_window()?;
    let s = w_g.global::<GitUI>();
    let model = match section {
        0 => s.get_changes_conflict(),
        1 => s.get_changes_unstaged(),
        2 => s.get_changes_staged(),
        _ => return None,
    };
    model.row_data(index as usize).map(|r| r.path.to_string())
}

fn conflict_path_at(_ctx: &Rc<Ctx>, index: usize) -> Option<String> {
    current_window().and_then(|w_g| {
        let s = w_g.global::<GitUI>();
        s.get_changes_conflict()
            .row_data(index)
            .map(|r| r.path.to_string())
    })
}

fn all_paths(_ctx: &Rc<Ctx>, staged: bool) -> Vec<String> {
    let Some(w_g) = current_window() else { return Vec::new() };
    let s = w_g.global::<GitUI>();
    let model = if staged {
        s.get_changes_unstaged()
    } else {
        s.get_changes_staged()
    };
    (0..model.row_count())
        .filter_map(|i| model.row_data(i).map(|r| r.path.to_string()))
        .collect()
}

fn branch_at(_ctx: &Rc<Ctx>, index: usize) -> Option<String> {
    current_window().and_then(|w_g| {
        let s = w_g.global::<GitUI>();
        s.get_branches()
            .row_data(index)
            .map(|b| b.name.to_string())
    })
}

fn commit_hash_at(_ctx: &Rc<Ctx>, index: usize) -> Option<String> {
    current_window().and_then(|w_g| {
        let s = w_g.global::<GitUI>();
        s.get_history()
            .row_data(index)
            .map(|c| c.hash.to_string())
    })
}

fn key_id_at(_ctx: &Rc<Ctx>, index: usize) -> Option<String> {
    current_window().and_then(|w_g| {
        let s = w_g.global::<GitUI>();
        s.get_keys().row_data(index).map(|k| k.id.to_string())
    })
}

// ---------- 刷新 ----------

/// 刷新工作区：所有 git 调用放到后台线程，结果回来后再写 UI。
pub fn refresh_all(ctx: &Rc<Ctx>) {
    let active = ctx.active.borrow().clone();
    {
        let runs = ctx.git_runs.borrow();
        let Some(run) = runs.get(active.as_str()) else { return };
        if run.repo_id.is_none() {
            return;
        }
    }
    let (path, key) = current_repo_info(ctx);
    if path.is_empty() {
        return;
    }
    let Some(tx) = git_sender(ctx) else { return };
    std::thread::spawn(move || {
        let snap = collect_snapshot(&path, key, 0);
        let _ = tx.send(GitMsg::Snap(snap));
    });
}

/// 在后台线程里跑完一串 git 命令，收集成快照。
fn collect_snapshot(path: &str, key: Option<PathBuf>, _page: usize) -> GitSnapshot {
    let mut runner = GitRunner::new(path);
    runner.ssh_key_path = key;
    let mut snap = GitSnapshot {
        branch: String::new(),
        ahead: 0,
        behind: 0,
        rebasing: false,
        conflicts: Vec::new(),
        unstaged: Vec::new(),
        staged: Vec::new(),
        untracked: Vec::new(),
        remote_url: String::new(),
        branches: Vec::new(),
        stashes: Vec::new(),
        unpushed: Vec::new(),
    };
    if let Ok(st) = runner.status() {
        snap.branch = st.branch.clone();
        snap.ahead = st.ahead as i32;
        snap.behind = st.behind as i32;
        snap.rebasing = st.rebasing;
        snap.conflicts = st
            .conflicts
            .iter()
            .map(|p| ChangeEntry {
                path: p.clone(),
                status: "UU".into(),
                staged: false,
                untracked: false,
                conflict: true,
            })
            .collect();
        snap.unstaged = st.unstaged.clone();
        snap.staged = st.staged.clone();
        snap.untracked = st.untracked.clone();
        if st.ahead > 0 {
            if let Ok(log) = runner.log(0, st.ahead.max(1)) {
                snap.unpushed = log;
            }
        }
    }
    if let Ok(remotes) = runner.remotes() {
        snap.remote_url = remotes
            .iter()
            .find(|(n, _)| n == "origin")
            .or_else(|| remotes.first())
            .map(|(_, u)| u.clone())
            .unwrap_or_default();
    }
    if let Ok(branches) = runner.branches() {
        snap.branches = branches;
    }
    if let Ok(stash) = runner.stash_list() {
        snap.stashes = stash;
    }
    snap
}

/// 把快照写回 UI。
fn apply_snapshot(_ctx: &Rc<Ctx>, snap: GitSnapshot) {
    let Some(w_g) = current_window() else { return };
    let s = w_g.global::<GitUI>();
    s.set_banner_branch(snap.branch.clone().into());
    s.set_ahead(snap.ahead);
    s.set_behind(snap.behind);
    s.set_conflict_count(snap.conflicts.len() as i32);
    s.set_rebasing(snap.rebasing);
    s.set_remote_url(snap.remote_url.clone().into());

    let to_rows = |v: &[ChangeEntry]| -> Vec<GitChangeRow> {
        v.iter()
            .map(|c| GitChangeRow {
                path: c.path.clone().into(),
                status: c.status.clone().into(),
                untracked: c.untracked,
            })
            .collect()
    };
    s.set_changes_conflict(ModelRc::from(Rc::new(VecModel::from(to_rows(&snap.conflicts)))));
    // 未跟踪文件（新增）也要能看见并暂存 —— 归到「未暂存」分组
    let mut unstaged_rows = to_rows(&snap.unstaged);
    unstaged_rows.extend(to_rows(&snap.untracked));
    s.set_changes_unstaged(ModelRc::from(Rc::new(VecModel::from(unstaged_rows))));
    s.set_changes_staged(ModelRc::from(Rc::new(VecModel::from(to_rows(&snap.staged)))));

    let branch_rows: Vec<GitBranchRow> = snap
        .branches
        .iter()
        .map(|b| GitBranchRow {
            name: b.name.clone().into(),
            current: b.current,
            remote: b.remote,
        })
        .collect();
    s.set_branches(ModelRc::from(Rc::new(VecModel::from(branch_rows))));

    let stash_rows: Vec<GitStashRow> = snap
        .stashes
        .iter()
        .map(|st| GitStashRow {
            index: st.index as i32,
            message: st.message.clone().into(),
        })
        .collect();
    s.set_stashes(ModelRc::from(Rc::new(VecModel::from(stash_rows))));

    let unpushed: Vec<GitCommitRow> = snap
        .unpushed
        .iter()
        .map(|c| GitCommitRow {
            hash: c.hash.clone().into(),
            short: c.hash.chars().take(7).collect::<String>().into(),
            author: c.author.clone().into(),
            date: c.date.clone().into(),
            subject: c.subject.clone().into(),
        })
        .collect();
    s.set_changes_unpushed(ModelRc::from(Rc::new(VecModel::from(unpushed))));
}

/// 加载历史（分页追加）。
pub fn reload_history(ctx: &Rc<Ctx>, append: bool) {
    let _ = append;
    let (path, key) = current_repo_info(ctx);
    if path.is_empty() {
        return;
    }
    let Some(tx) = git_sender(ctx) else { return };
    let page = ctx
        .git_runs
        .borrow()
        .get(ctx.active.borrow().as_str())
        .map(|r| r.history_page)
        .unwrap_or(0);
    std::thread::spawn(move || {
        let mut runner = GitRunner::new(path);
        runner.ssh_key_path = key;
        let rows = runner.log(page, 20).unwrap_or_default();
        let _ = tx.send(GitMsg::Hist { page, rows });
    });
}

/// 把历史页写回 UI。
fn apply_history(ctx: &Rc<Ctx>, page: usize, log: Vec<CommitRow>) {
    let Some(w_g) = current_window() else { return };
    let s = w_g.global::<GitUI>();
    let rows: Vec<GitCommitRow> = log
        .iter()
        .map(|c| GitCommitRow {
            hash: c.hash.clone().into(),
            short: c.hash.chars().take(7).collect::<String>().into(),
            author: c.author.clone().into(),
            date: c.date.clone().into(),
            subject: c.subject.clone().into(),
        })
        .collect();
    if page == 0 {
        s.set_history(ModelRc::from(Rc::new(VecModel::from(rows))));
    } else {
        let existing = s.get_history();
        let mut all: Vec<GitCommitRow> = Vec::new();
        for i in 0..existing.row_count() {
            if let Some(r) = existing.row_data(i) {
                all.push(r);
            }
        }
        all.extend(rows);
        s.set_history(ModelRc::from(Rc::new(VecModel::from(all))));
    }
    s.set_has_more_history(true);
    let _ = ctx;
}

/// 显示选中文件的差异。
fn show_diff(ctx: &Rc<Ctx>, section: i32, index: i32) {
    let Some(w_g) = current_window() else { return };
    let s = w_g.global::<GitUI>();
    let Some(path) = change_path_at(ctx, section, index) else { return };
    s.set_diff_path(path.clone().into());
    let (p, key) = current_repo_info(ctx);
    if p.is_empty() {
        return;
    }
    let Some(tx) = git_sender(ctx) else { return };
    std::thread::spawn(move || {
        let mut runner = GitRunner::new(p);
        runner.ssh_key_path = key;
        let diff = match section {
            0 => runner.diff_conflict(&path).unwrap_or_default(),
            1 => runner.diff_unstaged(&path).unwrap_or_default(),
            2 => runner.diff_staged(&path).unwrap_or_default(),
            _ => String::new(),
        };
        let rows: Vec<GitDiffRow> = if section == 0 {
            // 冲突文件：工作区里就是带冲突标记的原文，按行展示（标记行高亮）
            diff.lines()
                .enumerate()
                .map(|(i, l)| GitDiffRow {
                    kind: if l.starts_with("<<<<<<<")
                        || l.starts_with("=======")
                        || l.starts_with(">>>>>>>")
                    {
                        3
                    } else {
                        4
                    },
                    left_no: 0,
                    right_no: (i + 1) as i32,
                    text: l.into(),
                })
                .collect()
        } else {
            parse_diff(&diff)
                .into_iter()
                .map(|l| GitDiffRow {
                    kind: match l.kind {
                        DiffLineKind::Context => 0,
                        DiffLineKind::Add => 1,
                        DiffLineKind::Del => 2,
                        DiffLineKind::Hunk => 3,
                        DiffLineKind::Meta => 4,
                    },
                    left_no: l.left_no.unwrap_or(0) as i32,
                    right_no: l.right_no.unwrap_or(0) as i32,
                    text: l.text.into(),
                })
                .collect()
        };
        let _ = tx.send(GitMsg::Diff(rows));
    });
}

// ---------- 仓库 ----------

/// 登记一个本地仓库（alias 为空时用目录名）。
fn register_repo_at(ctx: &Rc<Ctx>, path: &str, alias: &str) -> Result<()> {
    let Some(db) = ctx.db() else { anyhow::bail!("数据库未打开") };
    if !Path::new(path).join(".git").exists() {
        anyhow::bail!("该目录不是 Git 仓库（缺少 .git）");
    }
    let alias = if alias.trim().is_empty() {
        Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    } else {
        alias.trim().to_string()
    };
    let repo = GitRepo {
        alias,
        path: path.to_string(),
        ..Default::default()
    };
    gstore::upsert_repo(&db, &repo)?;
    Ok(())
}

/// 使用 UI 中已填写的路径 / 别名登记仓库（兼容旧回调）。
fn register_repo(ctx: &Rc<Ctx>) -> Result<()> {
    let Some(w_g) = current_window() else { anyhow::bail!("窗口未就绪") };
    let s = w_g.global::<GitUI>();
    let path = s.get_repo_path().trim().to_string();
    let alias = s.get_repo_alias().trim().to_string();
    register_repo_at(ctx, &path, &alias)?;
    rebuild_repos(ctx);
    Ok(())
}

/// 登记并打开一个本地仓库（供无头截图 / 排障使用）。
pub fn register_and_open(ctx: &Rc<Ctx>, path: &str) {
    if let Err(e) = register_repo_at(ctx, path, "") {
        toast(ctx, 1, &truncate(&format!("登记失败：{e}"), 90));
        return;
    }
    let Some(db) = ctx.db() else { return };
    let repos = gstore::list_repos(&db).unwrap_or_default();
    if let Some(row) = repos.iter().rev().find(|r| r.repo.path == path) {
        let id = row.id.clone();
        open_repo(ctx, &id);
    }
    rebuild_repos(ctx);
}

pub fn rebuild_repos(ctx: &Rc<Ctx>) {
    let Some(ui) = current_window() else { return };
    let Some(db) = ctx.db() else { return };
    let active = ctx.active.borrow().clone();
    let expanded = ctx
        .git_runs
        .borrow()
        .get(active.as_str())
        .map(|r| r.expanded.clone())
        .unwrap_or_default();
    let folders = gstore::list_folders(&db).unwrap_or_default();
    let repos = gstore::list_repos(&db).unwrap_or_default();
    let mut nodes: Vec<GitRepoNode> = Vec::new();
    fn walk(
        folders: &[gstore::GitFolderRow],
        repos: &[gstore::RepoRow],
        folder: Option<&str>,
        depth: i32,
        expanded: &HashSet<String>,
        nodes: &mut Vec<GitRepoNode>,
    ) {
        for f in folders.iter().filter(|f| f.parent_id.as_deref() == folder) {
            let has_children = folders
                .iter()
                .any(|c| c.parent_id.as_deref() == Some(f.id.as_str()))
                || repos
                    .iter()
                    .any(|r| r.folder_id.as_deref() == Some(f.id.as_str()));
            let is_open = expanded.contains(&f.id);
            nodes.push(GitRepoNode {
                id: f.id.clone().into(),
                name: f.name.clone().into(),
                is_folder: true,
                depth,
                expanded: is_open && has_children,
                has_children,
                bound: false,
            });
            if is_open {
                walk(folders, repos, Some(&f.id), depth + 1, expanded, nodes);
            }
        }
        for r in repos.iter().filter(|r| r.folder_id.as_deref() == folder) {
            nodes.push(GitRepoNode {
                id: r.id.clone().into(),
                name: r.repo.alias.clone().into(),
                is_folder: false,
                depth,
                expanded: false,
                has_children: false,
                bound: false,
            });
        }
    }
    walk(&folders, &repos, None, 0, &expanded, &mut nodes);
    let git_ui2 = ui.global::<GitUI>();
    git_ui2.set_repos(ModelRc::from(Rc::new(VecModel::from(nodes))));
}

pub fn open_repo(ctx: &Rc<Ctx>, repo_id: &str) {
    let Some(db) = ctx.db() else { return };
    let repos = gstore::list_repos(&db).unwrap_or_default();
    let Some(row) = repos.into_iter().find(|r| r.id == repo_id) else { return };
    let active = ctx.active.borrow().clone();
    let key_name = {
        let keys = gstore::list_keys(&db).unwrap_or_default();
        row.repo
            .key_id
            .and_then(|kid| keys.iter().find(|k| k.id == kid).map(|k| k.name.clone()))
            .unwrap_or_default()
    };
    {
        let mut runs = ctx.git_runs.borrow_mut();
        let Some(run) = runs.get_mut(active.as_str()) else { return };
        run.repo_id = Some(row.id.clone());
        run.repo_path = row.repo.path.clone();
        run.history_page = 0;
    }
    if let Some(w_g) = current_window() {
        w_g.global::<GitUI>().set_bound_key_name(key_name.into());
    }
    sync_repo_open(ctx);
    refresh_all(ctx);
    reload_history(ctx, false);
}

pub fn sync_repo_open(ctx: &Rc<Ctx>) {
    let active = ctx.active.borrow().clone();
    let open = ctx
        .git_runs
        .borrow()
        .get(active.as_str())
        .map(|r| r.repo_id.is_some())
        .unwrap_or(false);
    if let Some(w_g) = current_window() {
        w_g.global::<GitUI>().set_repo_open(open);
    }
}

fn reload_keys(ctx: &Rc<Ctx>) {
    let Some(db) = ctx.db() else { return };
    let keys = gstore::list_keys(&db).unwrap_or_default();
    let active = ctx.active.borrow().clone();
    let bound = ctx
        .git_runs
        .borrow()
        .get(active.as_str())
        .and_then(|r| r.repo_id.clone())
        .and_then(|rid| {
            let repos = gstore::list_repos(&db).unwrap_or_default();
            repos
                .into_iter()
                .find(|r| r.id == rid)
                .and_then(|r| r.repo.key_id)
                .map(|k| k.to_string())
        });
    let rows: Vec<GitKeyRow> = keys
        .into_iter()
        .map(|k| GitKeyRow {
            id: k.id.to_string().into(),
            name: k.name.into(),
            algorithm: k.algorithm.into(),
            fingerprint: k.public_key.into(),
            bound: bound.as_deref() == Some(k.id.to_string().as_str()),
        })
        .collect();
    if let Some(w_g) = current_window() {
        w_g.global::<GitUI>().set_keys(ModelRc::from(Rc::new(VecModel::from(rows))));
    }
}

// ---------- 远程操作 ----------

fn pull_impl(ctx: &Rc<Ctx>, then_push: bool) {
    let (path, key) = current_repo_info(ctx);
    if path.is_empty() {
        return;
    }
    let Some(tx) = git_sender(ctx) else { return };
    set_busy(ctx, true);
    let ok = if then_push { "拉取并推送成功" } else { "拉取成功" }.to_string();
    std::thread::spawn(move || {
        let mut runner = GitRunner::new(path);
        runner.ssh_key_path = key.clone();
        let pull = runner.pull();
        let res = match pull {
            Ok(out) => {
                if then_push {
                    if let Err(e) = runner.push() {
                        Err(format!("{e}"))
                    } else {
                        Ok(out)
                    }
                } else {
                    Ok(out)
                }
            }
            Err(e) => Err(format!("{e}")),
        };
        let _ = tx.send(GitMsg::Job { ok, res });
    });
}

fn push_impl(ctx: &Rc<Ctx>) {
    let (path, key) = current_repo_info(ctx);
    if path.is_empty() {
        return;
    }
    let Some(tx) = git_sender(ctx) else { return };
    set_busy(ctx, true);
    std::thread::spawn(move || {
        let mut runner = GitRunner::new(path);
        runner.ssh_key_path = key;
        let res = runner.push().map_err(|e| format!("{e}"));
        let _ = tx.send(GitMsg::Job { ok: "推送成功".to_string(), res });
    });
}

/// 兼容旧调用：未使用的 ids 引用保持编译通过。
#[allow(dead_code)]
fn _keep_ids(_: &str) -> bool {
    ids::GIT == "git"
}
