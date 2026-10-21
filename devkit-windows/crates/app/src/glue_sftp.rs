//! SFTP 工具粘合层：左右双面板 ↔ 后端 ↔ 传输 HUD。

use crate::shell::{current_window, Ctx};
use crate::{SftpRow, SftpUI, SshTreeNode};
use devkit_tools::http::client::runtime;
use devkit_tools::sftp::backend::SftpBackend;
use devkit_tools::sftp::local::LocalBackend;
use devkit_tools::sftp::remote::RemoteBackend;
use devkit_tools::sftp::transport::{self, TransferItem};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// 一侧面板状态。
pub struct Panel {
    pub backend: Option<Box<dyn SftpBackend>>,
    pub path: String,
    pub rows: Vec<devkit_tools::sftp::backend::DirEntry>,
    pub selected: HashSet<String>,
}

impl Default for Panel {
    fn default() -> Self {
        Self { backend: None, path: String::new(), rows: Vec::new(), selected: HashSet::new() }
    }
}

/// SFTP 标签运行时。
pub struct SftpRun {
    pub left: Panel,
    pub right: Panel,
    pub cancel: Option<Arc<AtomicBool>>,
    pub done_bytes: Option<Arc<AtomicU64>>,
    pub total_bytes: u64,
    pub transferring: bool,
    pub pending_items: Option<Vec<PendingItem>>,
}

/// 待传条目（含来源侧标记与已传字节数）。
#[derive(Debug, Clone)]
pub struct PendingItem {
    pub item: TransferItem,
    pub src_is_left: bool,
    /// 该文件已传输的字节数（分块传输时用于续传）。
    pub done: u64,
}

impl Default for SftpRun {
    fn default() -> Self {
        Self {
            left: Panel::default(),
            right: Panel::default(),
            cancel: None,
            done_bytes: None,
            total_bytes: 0,
            transferring: false,
            pending_items: None,
        }
    }
}

pub fn wire_sftp_callbacks(ctx: Rc<Ctx>) {
    let Some(ui) = current_window() else { return };
    let sftp_ui = ui.global::<SftpUI>();

    // 宏在调用处展开为语句，把 RefMut 守卫留在调用方作用域，panel 借用才合法。
    macro_rules! side_panel {
        ($side:expr, $ctx:expr, $panel:ident) => {
            let active = $ctx.active.borrow().clone();
            let mut runs = $ctx.sftp_runs.borrow_mut();
            let Some(run) = runs.get_mut(active.as_str()) else { return };
            let $panel = match $side {
                2 => &mut run.right,
                _ => &mut run.left,
            };
        };
    }

    {
        let ctx2 = ctx.clone();
        sftp_ui.on_refresh(move |side| {
            side_panel!(side as u8, &ctx2, panel);
            refresh_panel(&ctx2, panel, side, None);
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_go_up(move |side| {
            let parent = {
                side_panel!(side as u8, &ctx2, panel);
                parent_of(&panel.path)
            };
            side_panel!(side as u8, &ctx2, panel);
            refresh_panel(&ctx2, panel, side, Some(parent));
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_path_edited(move |side, path| {
            side_panel!(side as u8, &ctx2, panel);
            refresh_panel(&ctx2, panel, side, Some(path.to_string()));
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_row_toggled(move |side, index| {
            let name = {
                side_panel!(side as u8, &ctx2, panel);
                panel.rows.get(index as usize).map(|r| r.name.clone())
            };
            let Some(name) = name else { return };
            {
                side_panel!(side as u8, &ctx2, panel);
                if panel.selected.contains(&name) {
                    panel.selected.remove(&name);
                } else {
                    panel.selected.insert(name);
                }
            }
            sync_panel_ui(&ctx2, side);
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_row_activated(move |side, index| {
            // -2 = 工具栏「进入目录」：进入唯一选中项
            let name = if index >= 0 {
                side_panel!(side as u8, &ctx2, panel);
                panel.rows.get(index as usize).map(|r| r.name.clone())
            } else {
                side_panel!(side as u8, &ctx2, panel);
                panel.selected.iter().next().cloned()
            };
            let Some(name) = name else { return };
            let (is_dir, new_path) = {
                side_panel!(side as u8, &ctx2, panel);
                let is_dir = panel.rows.iter().find(|r| r.name == name).map(|r| r.is_dir).unwrap_or(false);
                (is_dir, transport::join_path(&panel.path, &name))
            };
            if is_dir {
                side_panel!(side as u8, &ctx2, panel);
                refresh_panel(&ctx2, panel, side, Some(new_path));
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_connect_local(move |side| {
            side_panel!(side as u8, &ctx2, panel);
            panel.backend = Some(Box::new(LocalBackend::new()));
            let home = devkit_tools::sftp::local::dirs_home().to_string_lossy().to_string();
            refresh_panel(&ctx2, panel, side, Some(home));
            close_picker(&ctx2);
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_connect_profile(move |side, profile_id| {
            connect_profile(&ctx2, side as u8, &profile_id);
            close_picker(&ctx2);
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_disconnect(move |side| {
            {
                side_panel!(side as u8, &ctx2, panel);
                panel.backend = None;
                panel.path.clear();
                panel.rows.clear();
                panel.selected.clear();
            }
            sync_panel_ui(&ctx2, side);
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_open_picker(move |side| {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<SftpUI>();
                s.set_picker_side(side);
            }
            rebuild_picker_profiles(&ctx2);
        });
    }
    {
        sftp_ui.on_close_picker(move || {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<SftpUI>();
                s.set_picker_side(0);
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_transfer(move |from| {
            start_transfer(&ctx2, from as u8);
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_cancel_transfer(move || {
            if let Some(c) = &ctx2.sftp_runs.borrow().get(ctx2.active.borrow().as_str()).and_then(|r| r.cancel.clone()) {
                c.store(true, Ordering::Relaxed);
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_new_folder(move |side| {
            let Some(ui) = current_window() else { return };
            let sftp_ui2 = ui.global::<SftpUI>();
            let name = format!("新建文件夹{}", chrono::Local::now().format("%H%M%S"));
            {
                side_panel!(side as u8, &ctx2, panel);
                if panel.backend.is_none() {
                    return;
                }
                let p = transport::join_path(&panel.path, &name);
                let rt = runtime();
                let backend = panel.backend.as_mut().unwrap();
                if let Err(e) = rt.block_on(backend.mkdir(&p)) {
                    set_hud(&sftp_ui2, true, "新建文件夹失败", &format!("{e}"), 0.0);
                    return;
                }
            }
            side_panel!(side as u8, &ctx2, panel);
            refresh_panel(&ctx2, panel, side, None);
        });
    }
    {
        let ctx2 = ctx.clone();
        sftp_ui.on_delete_selected(move |side| {
            let (path, names) = {
                side_panel!(side as u8, &ctx2, panel);
                (panel.path.clone(), panel.selected.clone())
            };
            if names.is_empty() {
                return;
            }
            let rt = runtime();
            {
                side_panel!(side as u8, &ctx2, panel);
                if panel.backend.is_none() {
                    return;
                }
                for name in &names {
                    let p = transport::join_path(&path, name);
                    let Some(backend) = panel.backend.as_mut() else { break };
                    let is_dir = rt.block_on(backend.is_dir(&p)).unwrap_or(false);
                    let r = if is_dir {
                        rt.block_on(backend.remove_dir_recursive(&p))
                    } else {
                        rt.block_on(backend.remove_file(&p))
                    };
                    if let Err(e) = r {
                        if let Some(w_g) = current_window() {
                            let s = w_g.global::<SftpUI>();
                            set_hud(&s, true, "删除失败", "", 0.0);
                            s.set_hud_error(format!("{e}").into());
                        }
                    }
                }
                panel.selected.clear();
            }
            side_panel!(side as u8, &ctx2, panel);
            refresh_panel(&ctx2, panel, side, None);
        });
    }
}

fn close_picker(ctx: &Rc<Ctx>) {
    let _ = ctx;
     if let Some(w_g) = current_window() {
     let s = w_g.global::<SftpUI>();
        s.set_picker_side(0);
    }
}

fn set_hud(ui: &SftpUI, visible: bool, line1: &str, line2: &str, progress: f32) {
    ui.set_hud_visible(visible);
    ui.set_hud_line1(line1.into());
    ui.set_hud_line2(line2.into());
    ui.set_hud_progress(progress);
    if visible {
        ui.set_hud_error("".into());
    }
}

fn parent_of(path: &str) -> String {
    match path.rfind(['/', '\\']) {
        Some(i) if i > 0 => path[..i].to_string(),
        Some(_) => "/".to_string(),
        None => "/".to_string(),
    }
}

/// 连接远端（复用 SSH 会话记录库；本地记录不参与）。
fn connect_profile(ctx: &Rc<Ctx>, side: u8, profile_id: &str) {
    let Some(db) = ctx.db() else { return };
    let Ok(rows) = devkit_tools::ssh::store::list_profiles(&db) else { return };
    let Some(row) = rows.into_iter().find(|r| r.id == profile_id) else { return };
    let p = row.profile;
    if p.is_local() {
        return;
    }
    let key_pem = p.private_key_file_name.as_ref().and_then(|f| {
        let data_dir = devkit_core::prefs::data_dir()?;
        let path = std::path::Path::new(&data_dir).join("ssh_keys").join(f);
        std::fs::read_to_string(path).ok().map(|c| (c, p.key_passphrase.clone()))
    });
    let active = ctx.active.borrow().clone();
    let result = runtime().block_on(async {
        RemoteBackend::connect(
            &p.host,
            p.port,
            &p.username,
            p.password.clone(),
            key_pem,
        )
        .await
    });
    match result {
        Ok(remote) => {
            let home = runtime().block_on(remote.canonical_home());
            let mut runs = ctx.sftp_runs.borrow_mut();
            let Some(run) = runs.get_mut(active.as_str()) else { return };
            let panel = match side {
                2 => &mut run.right,
                _ => &mut run.left,
            };
            panel.backend = Some(Box::new(remote));
            refresh_panel(ctx, panel, side as i32, Some(home));
        }
        Err(e) => {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<SftpUI>();
                set_hud(&s, true, "连接失败", &format!("{e}"), 0.0);
            }
        }
    }
}

/// 刷新面板列表。
fn refresh_panel(_ctx: &Rc<Ctx>, panel: &mut Panel, side: i32, new_path: Option<String>) {
    if panel.backend.is_none() {
        return;
    }
    let path = new_path.unwrap_or_else(|| panel.path.clone());
    let rt = runtime();
    let backend = panel.backend.as_mut().unwrap();
    match rt.block_on(backend.list_dir(&path)) {
        Ok(entries) => {
            panel.path = path;
            panel.rows = entries;
            panel.selected.clear();
        }
        Err(e) => {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<SftpUI>();
                set_hud(&s, true, "读取目录失败", &format!("{e}"), 0.0);
            }
        }
    }
    if let Some(w_g) = current_window() {
        let sftp_ui = w_g.global::<SftpUI>();
        sync_panel_ui_at(&sftp_ui, panel, side);
    }
}

/// 面板状态 → UI。
fn sync_panel_ui(ctx: &Rc<Ctx>, side: i32) {
    let Some(ui) = current_window() else { return };
    let sftp_ui = ui.global::<SftpUI>();
    let active = ctx.active.borrow().clone();
    let runs = ctx.sftp_runs.borrow();
    let Some(run) = runs.get(active.as_str()) else { return };
    let panel = match side {
        2 => &run.right,
        _ => &run.left,
    };
    sync_panel_ui_at(&sftp_ui, panel, side);
}

/// 已持有 panel 引用时直接同步 UI（避免重复借 sftp_runs）。
fn sync_panel_ui_at(sftp_ui: &SftpUI, panel: &Panel, side: i32) {
    let (label, path, rows, selected, connected) = (
        String::new(),
        panel.path.clone(),
        panel.rows.clone(),
        panel.selected.clone(),
        panel.backend.is_some(),
    );
    let label_text = panel.backend.as_ref().map(|b| b.describe()).unwrap_or_default();
    let _ = label;
    let to_rows = |rows: &Vec<devkit_tools::sftp::backend::DirEntry>,
                   selected: &HashSet<String>|
     -> Vec<SftpRow> {
        rows.iter()
            .map(|r| SftpRow {
                name: r.name.clone().into(),
                is_dir: r.is_dir,
                size_text: if r.is_dir {
                    "目录".into()
                } else if r.size >= 1024 * 1024 {
                    format!("{:.1} MB", r.size as f64 / 1048576.0).into()
                } else if r.size >= 1024 {
                    format!("{:.1} KB", r.size as f64 / 1024.0).into()
                } else {
                    format!("{} B", r.size).into()
                },
                selected: selected.contains(&r.name),
            })
            .collect()
    };
    if side == 2 {
        sftp_ui.set_right_label(label_text.into());
        sftp_ui.set_right_path(path.as_str().into());
        sftp_ui.set_right_connected(connected);
        sftp_ui.set_right_rows(ModelRc::from(Rc::new(VecModel::from(to_rows(&rows, &selected)))));
    } else {
        sftp_ui.set_left_label(label_text.into());
        sftp_ui.set_left_path(path.as_str().into());
        sftp_ui.set_left_connected(connected);
        sftp_ui.set_left_rows(ModelRc::from(Rc::new(VecModel::from(to_rows(&rows, &selected)))));
    }
}

impl SftpRun {}

/// 激活 SFTP 标签时刷新两侧面板。
pub fn sync_all(ctx: &Rc<Ctx>, _ui: &crate::MainWindow) {
    sync_panel_ui(ctx, 1);
    sync_panel_ui(ctx, 2);
}

fn rebuild_picker_profiles(ctx: &Rc<Ctx>) {
    let Some(ui) = current_window() else { return };
    let Some(db) = ctx.db() else { return };
    let folders = devkit_tools::ssh::store::list_folders(&db).unwrap_or_default();
    let profiles = devkit_tools::ssh::store::list_profiles(&db).unwrap_or_default();
    let mut nodes: Vec<SshTreeNode> = Vec::new();
    fn walk(
        folders: &[devkit_tools::ssh::store::FolderRow],
        profiles: &[devkit_tools::ssh::store::ProfileRow],
        folder: Option<&str>,
        depth: i32,
        nodes: &mut Vec<SshTreeNode>,
    ) {
        for f in folders.iter().filter(|f| f.parent_id.as_deref() == folder) {
            nodes.push(SshTreeNode {
                id: f.id.clone().into(),
                name: f.name.clone().into(),
                summary: String::new().into(),
                is_folder: true,
                is_local: false,
                depth,
                expanded: true,
                has_children: true,
            });
            walk(folders, profiles, Some(&f.id), depth + 1, nodes);
        }
        for p in profiles.iter().filter(|p| p.folder_id.as_deref() == folder) {
            if p.profile.is_local() {
                continue;
            }
            nodes.push(SshTreeNode {
                id: p.id.clone().into(),
                name: p.profile.name.clone().into(),
                summary: p.profile.connect_summary().into(),
                is_folder: false,
                is_local: false,
                depth,
                expanded: false,
                has_children: false,
            });
        }
    }
    walk(&folders, &profiles, None, 0, &mut nodes);
    let sftp_ui = ui.global::<SftpUI>();
    sftp_ui.set_picker_profiles(ModelRc::from(Rc::new(VecModel::from(nodes))));
}

/// 启动传输（from → 另一侧）。
fn start_transfer(ctx: &Rc<Ctx>, from: u8) {
    let Some(ui) = current_window() else { return };
    let sftp_ui = ui.global::<SftpUI>();
    let active = ctx.active.borrow().clone();
    let (src_path, names, src_ok, dst_ok, dst_path) = {
        let mut runs = ctx.sftp_runs.borrow_mut();
        let Some(run) = runs.get_mut(active.as_str()) else { return };
        if run.transferring {
            return;
        }
        let (src_panel, dst_panel) = match from {
            2 => (&mut run.right, &mut run.left),
            _ => (&mut run.left, &mut run.right),
        };
        (
            src_panel.path.clone(),
            src_panel.selected.clone(),
            src_panel.backend.is_some(),
            dst_panel.backend.is_some(),
            dst_panel.path.clone(),
        )
    };
    if !src_ok || !dst_ok {
        set_hud(&sftp_ui, true, "无法传输", "请先连接两侧", 0.0);
        return;
    }
    if names.is_empty() {
        set_hud(&sftp_ui, true, "无法传输", "请先选中文件或文件夹", 0.0);
        return;
    }
    // 计划
    let mut items: Vec<TransferItem> = Vec::new();
    {
        let mut runs = ctx.sftp_runs.borrow_mut();
        let Some(run) = runs.get_mut(active.as_str()) else { return };
        let (a, b) = match from {
            2 => (&mut run.right, &mut run.left),
            _ => (&mut run.left, &mut run.right),
        };
        let rt = runtime();
        for name in &names {
            let src = transport::join_path(&src_path, name);
            if let Err(e) = rt.block_on(transport::plan_transfer(a.backend.as_mut().unwrap().as_mut(), &src, &dst_path, &mut items)) {
                set_hud(&sftp_ui, true, "计划失败", &format!("{e}"), 0.0);
                return;
            }
        }
        // 建目标目录
        for dir in items.iter().filter_map(|i| Some(parent_of(&i.dst_path)).filter(|p| !p.is_empty())) {
            let _ = rt.block_on(b.backend.as_mut().unwrap().mkdir(&dir));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let _ = done;
        let done_bytes = Arc::new(AtomicU64::new(0));
        run.cancel = Some(cancel.clone());
        run.done_bytes = Some(done_bytes.clone());
        run.transferring = true;
        run.total_bytes = items.iter().map(|i| i.size).sum();
        run.pending_items = Some(
            items
                .iter()
                .map(|i| PendingItem { item: i.clone(), src_is_left: from == 1, done: 0 })
                .collect(),
        );
    }
    // 轮询定时器驱动传输（在事件循环上 block_on 执行）
    let ctx2 = ctx.clone();
    let tab_id = active.clone();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(50), move || {
        drive_transfer(&ctx2, &tab_id);
    });
    TRANSFER_TIMER.with(|c| *c.borrow_mut() = Some(timer));
    set_hud(&sftp_ui, true, "传输中…", &format!("共 {} 个文件", items_len(ctx, &active)), 0.0);
}

thread_local! {
    static TRANSFER_TIMER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

fn items_len(ctx: &Rc<Ctx>, active: &str) -> usize {
    ctx.sftp_runs
        .borrow()
        .get(active)
        .and_then(|r| r.pending_items.as_ref())
        .map(|v| v.len())
        .unwrap_or(0)
}

/// 分步驱动传输（避免阻塞 UI）。
fn drive_transfer(ctx: &Rc<Ctx>, tab_id: &str) {
    let Some(ui) = current_window() else { return };
    let sftp_ui = ui.global::<SftpUI>();
    let step = {
        let mut runs = ctx.sftp_runs.borrow_mut();
        let Some(run) = runs.get_mut(tab_id) else { return };
        let Some(items) = run.pending_items.as_mut() else {
            run.transferring = false;
            return;
        };
        if let Some(c) = &run.cancel {
            if c.load(Ordering::Relaxed) {
                run.pending_items = None;
                run.transferring = false;
                set_hud(&sftp_ui, false, "", "", 0.0);
                return;
            }
        }
        // 每次搬一个文件
        let _ = run.done_bytes.as_ref();
        let next = items.first().cloned();
        match next {
            None => {
                run.pending_items = None;
                run.transferring = false;
                "done".to_string()
            }
            Some(item) => {
                let (a, b) = if item.src_is_left {
                    (&mut run.left, &mut run.right)
                } else {
                    (&mut run.right, &mut run.left)
                };
                if a.backend.is_none() || b.backend.is_none() {
                    items.remove(0);
                    return;
                }
                let rt = runtime();
                // 分块传输：每次搬运 CHUNK 字节，大文件不会整份读进内存。
                const CHUNK: u64 = 1024 * 1024;
                let size = item.item.size;
                let offset = item.done;
                let r = rt.block_on(async {
                    let remaining = size.saturating_sub(offset);
                    if remaining == 0 {
                        return Ok::<(), anyhow::Error>(());
                    }
                    let want = CHUNK.min(remaining);
                    let data = a.backend.as_mut().unwrap().read_chunk(&item.item.src_path, offset, want).await?;
                    if data.is_empty() {
                        return Ok(());
                    }
                    b.backend.as_mut().unwrap().write_chunk(&item.item.dst_path, offset, &data).await?;
                    Ok::<(), anyhow::Error>(())
                });
                match r {
                    Ok(()) => {
                        let moved = size.saturating_sub(offset).min(CHUNK);
                        items[0].done = offset + moved;
                        if let Some(done) = run.done_bytes.as_ref() {
                            done.fetch_add(moved, Ordering::Relaxed);
                        }
                        if items[0].done >= size {
                            items.remove(0);
                            format!("done-file:{}", item.item.src_path)
                        } else {
                            format!("copy:{}", item.item.src_path)
                        }
                    }
                    Err(e) => {
                        items.remove(0);
                        format!("err:{}:{}", item.item.src_path, e)
                    }
                }
            }
        }
    };
    // 更新进度
    let active_is = ctx.active.borrow().clone() == tab_id;
    if active_is {
        let runs = ctx.sftp_runs.borrow();
        if let Some(run) = runs.get(tab_id) {
            let done = run.done_bytes.as_ref().map(|d| d.load(Ordering::Relaxed)).unwrap_or(0);
            let total = run.total_bytes.max(1);
            if step == "done" {
                // 传输结束：隐藏进度条并给出结果提示
                set_hud(&sftp_ui, false, "", "", 0.0);
            } else if let Some(err) = step.strip_prefix("err:") {
                let msg = err.split_once(':').map(|(_, e)| e).unwrap_or(err);
                set_hud(&sftp_ui, true, "传输失败", "", 0.0);
                sftp_ui.set_hud_error(msg.to_string().into());
            } else {
                set_hud(&sftp_ui, true, "传输中…", &step, done as f32 / total as f32);
            }
        }
    }
}
