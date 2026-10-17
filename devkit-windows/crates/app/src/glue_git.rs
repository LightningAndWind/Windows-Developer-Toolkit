//! Git 工具粘合层：仓库登记 ↔ GitRunner ↔ UI。

use crate::shell::{current_window, Ctx};
use crate::{GitBranchRow, GitChangeRow, GitCommitRow, GitDiffRow, GitKeyRow, GitRepoNode, GitStashRow, GitUI};
use anyhow::Result;
use devkit_core::tool::ids;
use devkit_tools::git::runner::{parse_diff, ssh_keygen, GitRunner};
use devkit_tools::git::store as gstore;
use devkit_tools::git::types::{DiffLineKind, GitKey, GitRepo};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
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

pub fn wire_git_callbacks(ctx: Rc<Ctx>) {
    let Some(ui) = current_window() else { return };
    let git_ui = ui.global::<GitUI>();

    {
        let ctx2 = ctx.clone();
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
        let ctx2 = ctx.clone();
        git_ui.on_open_repo_path(move || {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<GitUI>();
                s.set_repo_path("".into());
                s.set_repo_alias("".into());
                s.set_console_visible(true);
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
    // 变更操作
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
            git_op(&ctx2, |r| r.stage(&[path.clone()]), "已暂存");
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_unstage(move |section, index| {
            let Some(path) = change_path_at(&ctx2, section, index) else { return };
            git_op(&ctx2, |r| r.unstage(&[path.clone()]), "已取消暂存");
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_stage_all(move |_| {
            let paths = all_paths(&ctx2, true);
            if !paths.is_empty() {
                git_op(&ctx2, |r| r.stage(&paths), "已全部暂存");
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_unstage_all(move |_| {
            let paths = all_paths(&ctx2, false);
            if !paths.is_empty() {
                git_op(&ctx2, |r| r.unstage(&paths), "已全部取消暂存");
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_discard(move |section, index| {
            let Some(path) = change_path_at(&ctx2, section, index) else { return };
            git_op(&ctx2, |r| r.discard(&[path.clone()]), "已丢弃");
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
            git_op(&ctx2, |r| r.commit(&msg, amend).map(|_| "提交成功".to_string()), "提交成功");
            s.set_commit_message("".into());
        });
    }
    // 分支
    {
        let ctx2 = ctx.clone();
        git_ui.on_branch_new(move || {
             let Some(w_g) = current_window() else { return };
             let s = w_g.global::<GitUI>();
            let name = s.get_new_branch_name().trim().to_string();
            if name.is_empty() {
                return;
            }
            git_op(&ctx2, |r| r.create_branch(&name, true).map(|_| format!("已创建并切换到 {name}")), "分支已创建");
            s.set_new_branch_name("".into());
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_branch_switch(move |index| {
            let Some(name) = branch_at(&ctx2, index as usize) else { return };
            git_op(&ctx2, |r| r.switch_branch(&name).map(|_| format!("已切换到 {name}")), "已切换");
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_branch_delete(move |index, force| {
            let Some(name) = branch_at(&ctx2, index as usize) else { return };
            git_op(&ctx2, |r| r.delete_branch(&name, force).map(|_| format!("已删除 {name}")), "分支已删除");
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_merge_branch(move |index| {
            let Some(name) = branch_at(&ctx2, index as usize) else { return };
            git_op(&ctx2, |r| r.merge_branch(&name), &format!("合并 {name}"));
        });
    }
    // Stash
    {
        let ctx2 = ctx.clone();
        git_ui.on_stash_push(move || git_op(&ctx2, |r| r.stash_push(None).map(|_| "已暂存改动".to_string()), "Stash 完成"));
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_stash_pop(move |index| git_op(&ctx2, |r| r.stash_pop(index as usize), "Stash Pop"));
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_stash_drop(move |index| git_op(&ctx2, |r| r.stash_drop(index as usize), "Stash Drop"));
    }
    // 历史
    {
        let ctx2 = ctx.clone();
        git_ui.on_history_menu(move |index, action| {
            let Some(hash) = commit_hash_at(&ctx2, index as usize) else { return };
            let r = match action {
                0 => with_runner(&ctx2, |run| run.cherry_pick(&hash)),
                1 => with_runner(&ctx2, |run| run.reset(&hash, "--soft")),
                2 => with_runner(&ctx2, |run| run.reset(&hash, "--mixed")),
                3 => with_runner(&ctx2, |run| run.reset(&hash, "--hard")),
                _ => {
                     if let Some(w_g) = current_window() {
                     let s = w_g.global::<GitUI>();
                        if let Ok(mut c) = arboard::Clipboard::new() {
                            let _ = c.set_text(hash.clone());
                        }
                        s.set_toast("已复制完整哈希".into());
                        s.set_toast_kind(0);
                    }
                    return;
                }
            };
            match r {
                Ok(out) => {
                    show_console(&ctx2, &out);
                    refresh_all(&ctx2);
                }
                Err(e) => toast(&ctx2, 1, &truncate(&format!("{e}"), 90)),
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
    // 远程
    {
        let ctx2 = ctx.clone();
        git_ui.on_fetch(move || git_op_ssh(&ctx2, |r| r.fetch(), "Fetch 完成"));
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
        git_ui.on_rebase_abort(move || git_op(&ctx2, |r| r.rebase_abort(), "变基已中止"));
    }
    // 冲突
    {
        let ctx2 = ctx.clone();
        git_ui.on_conflict_ours(move |index| {
            let Some(path) = conflict_path_at(&ctx2, index as usize) else { return };
            git_op(&ctx2, |r| r.take_ours(&path).map(|_| "已采用我方".to_string()), "已采用我方");
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_conflict_theirs(move |index| {
            let Some(path) = conflict_path_at(&ctx2, index as usize) else { return };
            git_op(&ctx2, |r| r.take_theirs(&path).map(|_| "已采用对方".to_string()), "已采用对方");
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_conflict_resolved(move |index| {
            let Some(path) = conflict_path_at(&ctx2, index as usize) else { return };
            git_op(&ctx2, |r| r.mark_resolved(&path).map(|_| "已标记解决".to_string()), "冲突已解决");
        });
    }
    // 克隆
    {
        let ctx2 = ctx.clone();
        git_ui.on_clone_start(move || {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<GitUI>();
                s.set_console_visible(true);
                s.set_console_text("输入 URL 与目标目录开始克隆…".into());
            }
        });
    }
    // 密钥
    {
        let ctx2 = ctx.clone();
        git_ui.on_keys_open(move || {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<GitUI>();
                s.set_console_visible(true);
            }
            reload_keys(&ctx2);
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
                        let fp = std::process::Command::new("ssh-keygen")
                            .arg("-lf")
                            .arg(&dest)
                            .output()
                            .ok()
                            .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string())
                            .unwrap_or_default();
                        let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
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
            // 删除私钥文件
            let data_dir = devkit_core::prefs::data_dir().unwrap_or_default();
            let _ = std::fs::remove_file(Path::new(&data_dir).join("git_keys").join(&id));
            reload_keys(&ctx2);
        });
    }
    {
        let ctx2 = ctx.clone();
        git_ui.on_key_copy_public(move |index| {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<GitUI>();
                if let Some(k) = s.get_keys().row_data(index as usize) {
                    if let Ok(mut c) = arboard::Clipboard::new() {
                        let _ = c.set_text(k.fingerprint.to_string());
                    }
                    s.set_toast("已复制".into());
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
            let repo_id = ctx2.git_runs.borrow().get(active.as_str()).and_then(|r| r.repo_id.clone());
            let Some(repo_id) = repo_id else { return };
            let repos = gstore::list_repos(&db).unwrap_or_default();
            if let Some(mut row) = repos.into_iter().find(|r| r.id == repo_id) {
                row.repo.key_id = uuid::Uuid::parse_str(&key_id).ok();
                let name = {
                    let keys = gstore::list_keys(&db).unwrap_or_default();
                    keys.iter().find(|k| k.id.to_string() == key_id).map(|k| k.name.clone()).unwrap_or_default()
                };
                if gstore::upsert_repo(&db, &row.repo).is_ok() {
                     if let Some(w_g) = current_window() {
                     let s = w_g.global::<GitUI>();
                        s.set_bound_key_name(name.into());
                    }
                    toast(&ctx2, 0, "密钥已绑定");
                }
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

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn with_runner<T>(ctx: &Rc<Ctx>, f: impl FnOnce(&GitRunner) -> Result<T>) -> Result<T> {
    let (path, key) = current_repo_info(ctx);
    let mut runner = GitRunner::new(path);
    runner.ssh_key_path = key;
    f(&runner)
}

fn current_repo_info(ctx: &Rc<Ctx>) -> (String, Option<std::path::PathBuf>) {
    let Some(db) = ctx.db() else { return (String::new(), None) };
    let active = ctx.active.borrow().clone();
    let repo_id = ctx.git_runs.borrow().get(active.as_str()).and_then(|r| r.repo_id.clone());
    let Some(repo_id) = repo_id else { return (String::new(), None) };
    let repos = gstore::list_repos(&db).unwrap_or_default();
    let Some(row) = repos.into_iter().find(|r| r.id == repo_id) else { return (String::new(), None) };
    let key = row.repo.key_id.and_then(|kid| {
        let keys = gstore::list_keys(&db).unwrap_or_default();
        keys.iter().find(|k| k.id == kid).and_then(|k| k.private_file.clone())
    });
    let data_dir = devkit_core::prefs::data_dir().unwrap_or_default();
    let key_path = key.map(|f| Path::new(&data_dir).join("git_keys").join(f));
    (row.repo.path, key_path)
}

fn git_op<T>(ctx: &Rc<Ctx>, f: impl FnOnce(&GitRunner) -> Result<T>, ok_msg: &str) {
    set_busy(ctx, true);
    let r = with_runner(ctx, f);
    set_busy(ctx, false);
    match r {
        Ok(_) => toast(ctx, 0, ok_msg),
        Err(e) => {
            // pull/merge/rebase 冲突 → 橙色警告并自动刷新（对应 macOS 冲突分流）
            let msg = format!("{e}");
            if msg.contains("CONFLICT") || msg.contains("conflict") {
                toast(ctx, 1, &truncate(&msg, 90));
                let first = with_runner(ctx, |run| run.status()).ok()
                    .and_then(|s| s.conflicts.first().cloned());
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
            toast(ctx, 1, &truncate(&msg, 90));
        }
    }
    refresh_all(ctx);
}

fn git_op_ssh(ctx: &Rc<Ctx>, f: impl FnOnce(&GitRunner) -> Result<String>, ok_msg: &str) {
    git_op(ctx, f, ok_msg);
}

fn set_busy(ctx: &Rc<Ctx>, busy: bool) {
    if let Some(w_g) = current_window() {
        let s = w_g.global::<GitUI>();
        s.set_busy(busy);
    }
    let _ = ctx;
}

fn toast(ctx: &Rc<Ctx>, kind: i32, msg: &str) {
    if let Some(w_g) = current_window() {
        let s = w_g.global::<GitUI>();
        s.set_toast_kind(kind);
        s.set_toast(msg.into());
    }
    LAST_ERROR.with(|c| *c.borrow_mut() = msg.to_string());
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

fn repo_node_at(ctx: &Rc<Ctx>, index: usize) -> Option<(String, bool)> {
     current_window().and_then(|w_g| { let s = w_g.global::<GitUI>(); s.get_repos().row_data(index).map(|n| (n.id.to_string(), n.is_folder)) })
}

fn change_path_at(ctx: &Rc<Ctx>, section: i32, index: i32) -> Option<String> {
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

fn conflict_path_at(ctx: &Rc<Ctx>, index: usize) -> Option<String> {
     current_window().and_then(|w_g| { let s = w_g.global::<GitUI>(); s.get_changes_conflict().row_data(index).map(|r| r.path.to_string()) })
}

fn all_paths(ctx: &Rc<Ctx>, staged: bool) -> Vec<String> {
     let Some(w_g) = current_window() else { return Vec::new() };
     let s = w_g.global::<GitUI>();
    let model = if staged { s.get_changes_unstaged() } else { s.get_changes_staged() };
    (0..model.row_count()).filter_map(|i| model.row_data(i).map(|r| r.path.to_string())).collect()
}

fn branch_at(ctx: &Rc<Ctx>, index: usize) -> Option<String> {
     current_window().and_then(|w_g| { let s = w_g.global::<GitUI>(); s.get_branches().row_data(index).map(|b| b.name.to_string()) })
}

fn commit_hash_at(ctx: &Rc<Ctx>, index: usize) -> Option<String> {
     current_window().and_then(|w_g| { let s = w_g.global::<GitUI>(); s.get_history().row_data(index).map(|c| c.hash.to_string()) })
}

fn key_id_at(ctx: &Rc<Ctx>, index: usize) -> Option<String> {
     current_window().and_then(|w_g| { let s = w_g.global::<GitUI>(); s.get_keys().row_data(index).map(|k| k.id.to_string()) })
}

// ---------- 刷新 ----------

pub fn refresh_all(ctx: &Rc<Ctx>) {
    let active = ctx.active.borrow().clone();
    {
        let runs = ctx.git_runs.borrow();
        let Some(run) = runs.get(active.as_str()) else { return };
        if run.repo_id.is_none() {
            return;
        }
    }
     let Some(w_g) = current_window() else { return };
     let s = w_g.global::<GitUI>();
    let (path, key) = current_repo_info(ctx);
    if path.is_empty() {
        return;
    }
    let mut runner = GitRunner::new(&path);
    runner.ssh_key_path = key;
    // 状态
    if let Ok(st) = runner.status() {
        s.set_banner_branch(st.branch.clone().into());
        s.set_ahead(st.ahead as i32);
        s.set_behind(st.behind as i32);
        s.set_conflict_count(st.conflicts.len() as i32);
        s.set_rebasing(st.rebasing);
        let to_rows = |v: &[devkit_tools::git::types::ChangeEntry]| -> Vec<GitChangeRow> {
            v.iter().map(|c| GitChangeRow {
                path: c.path.clone().into(),
                status: c.status.clone().into(),
                untracked: c.untracked,
            }).collect()
        };
        s.set_changes_conflict(ModelRc::from(Rc::new(VecModel::from(to_rows(&st.conflicts.iter().map(|p| devkit_tools::git::types::ChangeEntry {
            path: p.clone(), status: "UU".into(), staged: false, untracked: false, conflict: true,
        }).collect::<Vec<_>>())))));
        s.set_changes_unstaged(ModelRc::from(Rc::new(VecModel::from(to_rows(&st.unstaged)))));
        s.set_changes_staged(ModelRc::from(Rc::new(VecModel::from(to_rows(&st.staged)))));
        let unpushed = st.ahead;
        let _ = unpushed;
    }
    // 远程
    if let Ok(remotes) = runner.remotes() {
        let url = remotes.iter().find(|(n, _)| n == "origin").or_else(|| remotes.first()).map(|(_, u)| u.clone()).unwrap_or_default();
        s.set_remote_url(url.into());
    }
    // 分支
    if let Ok(branches) = runner.branches() {
        let rows: Vec<GitBranchRow> = branches.into_iter().map(|b| GitBranchRow {
            name: b.name.into(), current: b.current, remote: b.remote,
        }).collect();
        s.set_branches(ModelRc::from(Rc::new(VecModel::from(rows))));
    }
    // stash
    if let Ok(stash) = runner.stash_list() {
        let rows: Vec<GitStashRow> = stash.into_iter().map(|st| GitStashRow {
            index: st.index as i32, message: st.message.into(),
        }).collect();
        s.set_stashes(ModelRc::from(Rc::new(VecModel::from(rows))));
    }
    // 未推送提交行
    if st_ahead(ctx) > 0 {
        if let Ok(log) = runner.log(0, st_ahead(ctx).max(1)) {
            let rows: Vec<GitCommitRow> = log.into_iter().map(|c| GitCommitRow {
                hash: c.hash.clone().into(),
                short: c.hash.chars().take(7).collect::<String>().into(),
                author: c.author.into(),
                date: c.date.into(),
                subject: c.subject.into(),
            }).collect();
            s.set_changes_unpushed(ModelRc::from(Rc::new(VecModel::from(rows))));
        }
    } else {
        s.set_changes_unpushed(ModelRc::from(Rc::new(VecModel::from(Vec::<GitCommitRow>::new()))));
    }
}

fn st_ahead(ctx: &Rc<Ctx>) -> usize {
     current_window().and_then(|w_g| { let s = w_g.global::<GitUI>(); Some(s.get_ahead().max(0) as usize) }).unwrap_or(0)
}

pub fn reload_history(ctx: &Rc<Ctx>, append: bool) {
      if let Some(w_g) = current_window() {
     let s = w_g.global::<GitUI>();
    let (path, key) = current_repo_info(ctx);
    if path.is_empty() { return; }
    let mut runner = GitRunner::new(&path);
    runner.ssh_key_path = key;
    let page = ctx.git_runs.borrow().get(ctx.active.borrow().as_str()).map(|r| r.history_page).unwrap_or(0);
    if let Ok(log) = runner.log(page, 20) {
        let rows: Vec<GitCommitRow> = log.into_iter().map(|c| GitCommitRow {
            hash: c.hash.clone().into(),
            short: c.hash.chars().take(7).collect::<String>().into(),
            author: c.author.into(),
            date: c.date.into(),
            subject: c.subject.into(),
        }).collect();
        if append {
            let existing = s.get_history();
            let mut all: Vec<GitCommitRow> = Vec::new();
            for i in 0..existing.row_count() {
                if let Some(r) = existing.row_data(i) {
                    all.push(r);
                }
            }
            all.extend(rows);
            s.set_history(ModelRc::from(Rc::new(VecModel::from(all))));
            s.set_has_more_history(true);
        } else {
            s.set_history(ModelRc::from(Rc::new(VecModel::from(rows))));
            s.set_has_more_history(true);
        }
    }
    }
}

fn show_diff(ctx: &Rc<Ctx>, section: i32, index: i32) {
      if let Some(w_g) = current_window() {
     let s = w_g.global::<GitUI>();
    let Some(path) = change_path_at(ctx, section, index) else { return };
    s.set_diff_path(path.clone().into());
    let (p, key) = current_repo_info(ctx);
    if p.is_empty() { return; }
    let mut runner = GitRunner::new(&p);
    runner.ssh_key_path = key;
    let diff = match section {
        0 => runner.diff_conflict(&path).unwrap_or_default(),
        1 => runner.diff_unstaged(&path).unwrap_or_default(),
        2 => runner.diff_staged(&path).unwrap_or_default(),
        _ => String::new(),
    };
    let rows: Vec<GitDiffRow> = parse_diff(&diff).into_iter().map(|l| GitDiffRow {
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
    }).collect();
    s.set_diff(ModelRc::from(Rc::new(VecModel::from(rows))));
    }
}

// ---------- 仓库 ----------

fn register_repo(ctx: &Rc<Ctx>) -> Result<()> {
    let Some(db) = ctx.db() else { anyhow::bail!("数据库未打开") };
    let Some(w_g) = current_window() else { anyhow::bail!("窗口未就绪") };
    let s = w_g.global::<GitUI>();
    let path = s.get_repo_path().trim().to_string();
    if path.is_empty() || !Path::new(&path).join(".git").exists() {
        s.set_console_text("请先选择包含 .git 的仓库目录".into());
        anyhow::bail!("无效仓库目录");
    }
    let alias = s.get_repo_alias().trim().to_string();
    let alias = if alias.is_empty() {
        Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    } else {
        alias
    };
    let repo = GitRepo { alias, path: path.clone(), ..Default::default() };
    gstore::upsert_repo(&db, &repo)?;
    rebuild_repos(ctx);
    s.set_console_text(format!("已登记：{path}").into());
    Ok(())
}

pub fn rebuild_repos(ctx: &Rc<Ctx>) {
    let Some(ui) = current_window() else { return };
    let Some(db) = ctx.db() else { return };
    let active = ctx.active.borrow().clone();
    let expanded = ctx.git_runs.borrow().get(active.as_str()).map(|r| r.expanded.clone()).unwrap_or_default();
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
            let has_children = folders.iter().any(|c| c.parent_id.as_deref() == Some(f.id.as_str()))
                || repos.iter().any(|r| r.folder_id.as_deref() == Some(f.id.as_str()));
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
        row.repo.key_id.and_then(|kid| keys.iter().find(|k| k.id == kid).map(|k| k.name.clone())).unwrap_or_default()
    };
    {
        let mut runs = ctx.git_runs.borrow_mut();
        let Some(run) = runs.get_mut(active.as_str()) else { return };
        run.repo_id = Some(row.id.clone());
        run.repo_path = row.repo.path.clone();
        run.history_page = 0;
    }
     if let Some(w_g) = current_window() {
     let s = w_g.global::<GitUI>();
        s.set_bound_key_name(key_name.into());
    }
    sync_repo_open(ctx);
    refresh_all(ctx);
    reload_history(ctx, false);
}

pub fn sync_repo_open(ctx: &Rc<Ctx>) {
    let active = ctx.active.borrow().clone();
    let open = ctx.git_runs.borrow().get(active.as_str()).map(|r| r.repo_id.is_some()).unwrap_or(false);
     if let Some(w_g) = current_window() {
     let s = w_g.global::<GitUI>();
        s.set_repo_open(open);
    }
}

fn reload_keys(ctx: &Rc<Ctx>) {
    let Some(db) = ctx.db() else { return };
    let keys = gstore::list_keys(&db).unwrap_or_default();
    let active = ctx.active.borrow().clone();
    let bound = ctx.git_runs.borrow().get(active.as_str()).and_then(|r| r.repo_id.clone()).and_then(|rid| {
        let repos = gstore::list_repos(&db).unwrap_or_default();
        repos.into_iter().find(|r| r.id == rid).and_then(|r| r.repo.key_id).map(|k| k.to_string())
    });
    let rows: Vec<GitKeyRow> = keys.into_iter().map(|k| GitKeyRow {
        id: k.id.to_string().into(),
        name: k.name.into(),
        algorithm: k.algorithm.into(),
        fingerprint: k.public_key.into(),
        bound: bound.as_deref() == Some(k.id.to_string().as_str()),
    }).collect();
     if let Some(w_g) = current_window() {
     let s = w_g.global::<GitUI>();
        s.set_keys(ModelRc::from(Rc::new(VecModel::from(rows))));
    }
}

// ---------- 远程操作 ----------

fn pull_impl(ctx: &Rc<Ctx>, then_push: bool) {
    set_busy(ctx, true);
    let r = with_runner(ctx, |run| run.pull());
    set_busy(ctx, false);
    match r {
        Ok(out) => {
            toast(ctx, 0, "拉取成功");
            if then_push {
                let pr = with_runner(ctx, |run| run.push());
                match pr {
                    Ok(_) => toast(ctx, 0, "推送成功"),
                    Err(e) => toast(ctx, 1, &truncate(&format!("{e}"), 90)),
                }
            }
            let _ = out;
        }
        Err(e) => git_op(ctx, |_| Ok::<String, anyhow::Error>(String::new()), ""),
    }
    refresh_all(ctx);
}

fn push_impl(ctx: &Rc<Ctx>) {
    set_busy(ctx, true);
    let r = with_runner(ctx, |run| run.push());
    set_busy(ctx, false);
    match r {
        Ok(_) => toast(ctx, 0, "推送成功"),
        Err(e) => toast(ctx, 1, &truncate(&format!("{e}"), 90)),
    }
    refresh_all(ctx);
}
