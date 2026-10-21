//! SSH 工具粘合层：会话记录库 ↔ 选择面板 ↔ 终端宿主。
//! 终端行模型按标签持有；激活标签时换挂到共享 TerminalView。

use crate::shell::{current_window, Ctx};
use crate::{SshTreeNode, SshUI, TermHostUI, TermLine};
use devkit_tools::ssh::client::SshTerminal;
use devkit_tools::ssh::model::{SSHAuthKind, SSHConnectionState, SSHProfile, SSHSessionKind};
use devkit_tools::ssh::store::{self as sstore, KeyStorage};
use devkit_tools::terminal::TerminalSession;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

/// SSH 标签运行时。
pub struct SshRun {
    pub terminal: Option<SshTerminal>,
    pub session: TerminalSession,
    pub model: Rc<VecModel<TermLine>>,
    pub timer: Option<slint::Timer>,
    pub profile: Option<SSHProfile>,
    pub profile_id: Option<String>,
    pub expanded: HashSet<String>,
    /// 当前横幅状态（避免每帧重复写全局状态）。
    pub banner_kind: i32,
}

impl Default for SshRun {
    fn default() -> Self {
        Self {
            terminal: None,
            session: TerminalSession::new(100, 30),
            model: Rc::new(VecModel::default()),
            timer: None,
            profile: None,
            profile_id: None,
            expanded: HashSet::new(),
            banner_kind: -1,
        }
    }
}

/// 随会话快照持久化的 SSH 状态（仅记录绑定的连接）。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SshToolState {
    #[serde(default)]
    pub profile_id: Option<String>,
}

pub fn wire_ssh_callbacks(ctx: Rc<Ctx>) {
    let Some(ui) = current_window() else { return };
    let ssh_ui = ui.global::<SshUI>();
    let tv = ui.global::<TermHostUI>();

    // 终端键盘输入 → 激活标签 PTY
    {
        let ctx2 = ctx.clone();
        tv.on_send_key(move |text, ctrl, alt, _shift| {
            let bytes = crate::term_host::encode_key(&text, ctrl, alt);
            let active = ctx2.active.borrow().clone();
            let mut runs = ctx2.ssh_runs.borrow_mut();
            if let Some(run) = runs.get_mut(active.as_str()) {
                if let Some(t) = run.terminal.as_mut() {
                    let _ = t.pty.write(&bytes);
                }
            }
        });
    }
    // 终端几何变化 → 激活标签 resize
    {
        let ctx2 = ctx.clone();
        tv.on_geometry_changed(move || {
            let Some(ui) = current_window() else { return };
            let tv = ui.global::<TermHostUI>();
            let cols = tv.get_cols_f().round().max(2.0) as usize;
            let rows = tv.get_rows_f().round().max(2.0) as usize;
            let active = ctx2.active.borrow().clone();
            let mut runs = ctx2.ssh_runs.borrow_mut();
            if let Some(run) = runs.get_mut(active.as_str()) {
                if cols != run.session.grid.cols || rows != run.session.grid.rows {
                    run.session.resize(cols, rows);
                    if let Some(t) = run.terminal.as_ref() {
                        let _ = t.pty.resize(cols as u16, rows as u16);
                    }
                }
            }
        });
    }

    // 树操作
    {
        let ctx2 = ctx.clone();
        ssh_ui.on_tree_toggled(move |index| {
            let active = ctx2.active.borrow().clone();
            let id = node_id_at(ctx2.clone(), index as usize);
            let Some(id) = id else { return };
            if let Some(run) = ctx2.ssh_runs.borrow_mut().get_mut(active.as_str()) {
                if run.expanded.contains(&id) {
                    run.expanded.remove(&id);
                } else {
                    run.expanded.insert(id);
                }
            }
            rebuild_profiles(&ctx2);
        });
    }
    {
        let ctx2 = ctx.clone();
        ssh_ui.on_tree_selected(move |index| {
            connect_by_index(&ctx2, index as usize);
        });
    }
    {
        let ctx2 = ctx.clone();
        ssh_ui.on_tree_deleted(move |index| {
            let Some((id, is_folder)) = node_id_and_kind_at(ctx2.clone(), index as usize) else { return };
            let Some(db) = ctx2.db() else { return };
            let result = if is_folder {
                sstore::delete_folder_cascade(&db, &id)
            } else {
                sstore::delete_profile(&db, &id)
            };
            if result.is_ok() {
                rebuild_profiles(&ctx2);
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        ssh_ui.on_edit_record(move |index| {
            let Some(db) = ctx2.db() else { return };
            let Some((id, is_folder)) = node_id_and_kind_at(ctx2.clone(), index as usize) else { return };
            if is_folder {
                return;
            }
            let Ok(rows) = sstore::list_profiles(&db) else { return };
            let Some(row) = rows.into_iter().find(|r| r.id == id) else { return };
            let p = row.profile;
             if let Some(w_g) = current_window() {
             let s = w_g.global::<SshUI>();
                s.set_editor_visible(true);
                s.set_editing_local(p.is_local());
                s.set_ed_name(p.name.as_str().into());
                s.set_ed_host(p.host.as_str().into());
                s.set_ed_port(p.port.to_string().into());
                s.set_ed_user(p.username.as_str().into());
                s.set_ed_auth(if p.auth_kind == SSHAuthKind::Key { 1 } else { 0 });
                s.set_ed_password(p.password.clone().unwrap_or_default().into());
                s.set_ed_key_file(p.private_key_file_name.clone().unwrap_or_default().into());
                s.set_ed_passphrase(p.key_passphrase.clone().unwrap_or_default().into());
                s.set_ed_working_dir(p.working_directory.clone().unwrap_or_default().into());
                s.set_editor_error("".into());
            }
            EDITING.with(|c| *c.borrow_mut() = Some(id));
        });
    }
    {
        ssh_ui.on_new_remote_record(move || {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<SshUI>();
                s.set_editor_visible(true);
                s.set_editing_local(false);
                s.set_ed_name("".into());
                s.set_ed_host("".into());
                s.set_ed_port("22".into());
                s.set_ed_user("".into());
                s.set_ed_auth(0);
                s.set_ed_password("".into());
                s.set_ed_key_file("".into());
                s.set_ed_passphrase("".into());
                s.set_ed_working_dir("".into());
                s.set_editor_error("".into());
            }
            EDITING.with(|c| *c.borrow_mut() = None);
        });
    }
    {
        ssh_ui.on_new_local_record(move || {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<SshUI>();
                s.set_editor_visible(true);
                s.set_editing_local(true);
                s.set_ed_name("".into());
                s.set_ed_working_dir("".into());
                s.set_editor_error("".into());
            }
            EDITING.with(|c| *c.borrow_mut() = None);
        });
    }
    {
        ssh_ui.on_cancel_editor(move || {
             if let Some(w_g) = current_window() {
             let s = w_g.global::<SshUI>();
                s.set_editor_visible(false);
            }
            EDITING.with(|c| *c.borrow_mut() = None);
        });
    }
    {
        let ctx2 = ctx.clone();
        ssh_ui.on_save_record(move || {
            let Some(ui) = current_window() else { return };
            let Some(db) = ctx2.db() else { return };
            let ssh_ui2 = ui.global::<SshUI>();
            let name = ssh_ui2.get_ed_name().trim().to_string();
            if name.is_empty() {
                ssh_ui2.set_editor_error("名称不能为空".into());
                return;
            }
            let editing = EDITING.with(|c| c.borrow().clone());
            let mut p = match &editing {
                Some(id) => {
                    let rows = sstore::list_profiles(&db).unwrap_or_default();
                    rows.into_iter()
                        .find(|r| r.id == *id)
                        .map(|r| r.profile)
                        .unwrap_or_default()
                }
                None => SSHProfile::default(),
            };
            p.name = name;
            if ssh_ui2.get_editing_local() {
                p.kind = SSHSessionKind::Local;
                p.host = String::new();
                p.username = String::new();
                let dir = ssh_ui2.get_ed_working_dir().trim().to_string();
                p.working_directory = if dir.is_empty() { None } else { Some(dir) };
            } else {
                p.kind = SSHSessionKind::Remote;
                p.host = ssh_ui2.get_ed_host().trim().to_string();
                p.port = ssh_ui2.get_ed_port().trim().parse().unwrap_or(22);
                p.username = ssh_ui2.get_ed_user().trim().to_string();
                p.auth_kind = if ssh_ui2.get_ed_auth() == 1 { SSHAuthKind::Key } else { SSHAuthKind::Password };
                p.password = Some(ssh_ui2.get_ed_password().to_string());
                p.key_passphrase = Some(ssh_ui2.get_ed_passphrase().to_string());
                let dir = ssh_ui2.get_ed_working_dir().trim().to_string();
                p.working_directory = if dir.is_empty() { None } else { Some(dir) };
                if p.auth_kind == SSHAuthKind::Key {
                    let key_file = ssh_ui2.get_ed_key_file().to_string();
                    if key_file.is_empty() {
                        ssh_ui2.set_editor_error("请选择私钥文件".into());
                        return;
                    }
                    p.private_key_file_name = Some(key_file);
                }
            }
            if sstore::upsert_profile(&db, &p).is_ok() {
                ssh_ui2.set_editor_visible(false);
                EDITING.with(|c| *c.borrow_mut() = None);
                rebuild_profiles(&ctx2);
                // 保存后直接连接
                let pid = p.id.to_string();
                connect_profile_id(&ctx2, &pid);
            }
        });
    }
    {
        ssh_ui.on_pick_key_file(move || {
            if let Some(file) = rfd::FileDialog::new()
                .set_title("选择私钥文件")
                .add_filter("密钥文件", &["pem", "key", "pub", ""])
                .pick_file()
            {
                 if let Some(w_g) = current_window() {
                 let s = w_g.global::<SshUI>();
                    s.set_ed_key_file(file.to_string_lossy().to_string().into());
                }
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        ssh_ui.on_connect_local(move || {
            connect_local(&ctx2);
        });
    }
    {
        let ctx2 = ctx.clone();
        ssh_ui.on_disconnect(move || {
            let active = ctx2.active.borrow().clone();
            if let Some(run) = ctx2.ssh_runs.borrow_mut().get_mut(active.as_str()) {
                if let Some(t) = run.terminal.as_mut() {
                    t.mark_disconnected();
                    t.pty.kill();
                }
                run.terminal = None;
                run.timer = None;
            }
            set_banner(ctx2.clone(), 4, "已断开");
        });
    }
    {
        let ctx2 = ctx.clone();
        ssh_ui.on_show_chooser(move || {
            let active = ctx2.active.borrow().clone();
            {
                let mut runs = ctx2.ssh_runs.borrow_mut();
                if let Some(run) = runs.get_mut(active.as_str()) {
                    if let Some(mut t) = run.terminal.take() {
                        t.pty.kill();
                    }
                    run.timer = None;
                    run.profile = None;
                    run.profile_id = None;
                    run.banner_kind = -1;
                }
            }
            if let Some(ui) = current_window() {
                let tv = ui.global::<TermHostUI>();
                tv.set_lines(ModelRc::from(Rc::new(VecModel::<TermLine>::default())));
            }
            ACTIVE_FRAME.with(|c| *c.borrow_mut() = None);
            set_banner(ctx2.clone(), 0, "");
            rebuild_profiles(&ctx2);
        });
    }
    {
        let ctx2 = ctx.clone();
        ssh_ui.on_reconnect(move || {
            let active = ctx2.active.borrow().clone();
            let pid = ctx2.ssh_runs.borrow().get(active.as_str()).and_then(|r| r.profile_id.clone());
            if let Some(pid) = pid {
                connect_profile_id(&ctx2, &pid);
            }
        });
    }
    {
        ssh_ui.on_editor_kind_changed(move |kind| {
            if let Some(w_g) = current_window() {
                let s = w_g.global::<SshUI>();
                s.set_editing_local(kind == 1);
                if kind == 0 && s.get_ed_port().trim().is_empty() {
                    s.set_ed_port("22".into());
                }
                s.set_editor_error("".into());
            }
        });
    }
    {
        ssh_ui.on_editor_auth_cycled(move || {});
    }
    {
        ssh_ui.on_copy_summary(move || {});
    }
    ssh_ui.set_state_kind(0);
    let _ = tv;
}

thread_local! {
    static EDITING: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn node_id_at(ctx: Rc<Ctx>, index: usize) -> Option<String> {
    node_id_and_kind_at(ctx, index).map(|(id, _)| id)
}

fn node_id_and_kind_at(_ctx: Rc<Ctx>, index: usize) -> Option<(String, bool)> {
    let Some(ui) = current_window() else { return None };
    let ssh_ui = ui.global::<SshUI>();
    ssh_ui.get_profiles().row_data(index).map(|n| (n.id.to_string(), n.is_folder))
}

fn set_banner(ctx: Rc<Ctx>, kind: i32, text: &str) {
    let Some(ui) = current_window() else { return };
    let ssh_ui = ui.global::<SshUI>();
    ssh_ui.set_state_kind(kind);
    ssh_ui.set_state_text(text.into());
    let active = ctx.active.borrow().clone();
    let connected = kind == 2;
    ssh_ui.set_connected(connected);
    if let Some(run) = ctx.ssh_runs.borrow_mut().get_mut(active.as_str()) {
        run.banner_kind = kind;
    }
    if kind == 2 {
        let summary = ctx
            .ssh_runs
            .borrow()
            .get(active.as_str())
            .and_then(|r| r.profile.clone())
            .map(|p| if p.is_local() { "本地终端".to_string() } else { p.connect_summary() })
            .unwrap_or_default();
        ssh_ui.set_connect_summary(summary.into());
    } else {
        ssh_ui.set_connect_summary("".into());
    }
}

/// 重建会话记录树。
pub fn rebuild_profiles(ctx: &Rc<Ctx>) {
    let Some(ui) = current_window() else { return };
    let Some(db) = ctx.db() else { return };
    let active = ctx.active.borrow().clone();
    let expanded = ctx
        .ssh_runs
        .borrow()
        .get(active.as_str())
        .map(|r| r.expanded.clone())
        .unwrap_or_default();
    let folders = sstore::list_folders(&db).unwrap_or_default();
    let profiles = sstore::list_profiles(&db).unwrap_or_default();

    let mut nodes: Vec<SshTreeNode> = Vec::new();
    fn walk(
        folders: &[sstore::FolderRow],
        profiles: &[sstore::ProfileRow],
        folder: Option<&str>,
        depth: i32,
        expanded: &HashSet<String>,
        nodes: &mut Vec<SshTreeNode>,
    ) {
        for f in folders.iter().filter(|f| f.parent_id.as_deref() == folder) {
            let has_children = folders.iter().any(|c| c.parent_id.as_deref() == Some(f.id.as_str()))
                || profiles.iter().any(|p| p.folder_id.as_deref() == Some(f.id.as_str()));
            let is_open = expanded.contains(&f.id);
            nodes.push(SshTreeNode {
                id: f.id.clone().into(),
                name: f.name.clone().into(),
                summary: String::new().into(),
                is_folder: true,
                is_local: false,
                depth,
                expanded: is_open && has_children,
                has_children,
            });
            if is_open {
                walk(folders, profiles, Some(&f.id), depth + 1, expanded, nodes);
            }
        }
        for p in profiles.iter().filter(|p| p.folder_id.as_deref() == folder) {
            let summary = if p.profile.is_local() {
                match p.profile.normalized_working_directory() {
                    Some(d) => format!("本地 · {d}"),
                    None => "本地终端 · 个人目录".to_string(),
                }
            } else {
                p.profile.connect_summary()
            };
            nodes.push(SshTreeNode {
                id: p.id.clone().into(),
                name: p.profile.name.clone().into(),
                summary: summary.into(),
                is_folder: false,
                is_local: p.profile.is_local(),
                depth,
                expanded: false,
                has_children: false,
            });
        }
    }
    walk(&folders, &profiles, None, 0, &expanded, &mut nodes);
    let ssh_ui = ui.global::<SshUI>();
    ssh_ui.set_profiles(ModelRc::from(Rc::new(VecModel::from(nodes))));
}

fn connect_by_index(ctx: &Rc<Ctx>, index: usize) {
    let Some((id, is_folder)) = node_id_and_kind_at(ctx.clone(), index) else { return };
    if is_folder {
        return;
    }
    connect_profile_id(ctx, &id);
}

/// 按记录 ID 连接（当前激活标签）。
pub fn connect_profile_id(ctx: &Rc<Ctx>, profile_id: &str) {
    let Some(ui) = current_window() else { return };
    let Some(db) = ctx.db() else { return };
    let Ok(rows) = sstore::list_profiles(&db) else { return };
    let Some(row) = rows.into_iter().find(|r| r.id == profile_id) else { return };
    let profile = row.profile;
    if profile.is_local() {
        let cwd = profile.normalized_working_directory();
        start_terminal(ctx, ui, &profile, profile_id, true, cwd.as_deref());
        return;
    }
    // TOFU：已信任的 host:port 自动应答；否则也自动接受并落库（accept-new 语义）
    let trusted = sstore::is_trusted(&db, &profile.host, profile.port, None).unwrap_or(false);
    if !trusted {
        let _ = sstore::trust_host(&db, &profile.host, profile.port, "unknown", &[]);
    }
    start_terminal(ctx, ui, &profile, profile_id, false, None);
}

/// 打开本地终端（供 UI 按钮与无头回归测试调用）。
pub fn connect_local(ctx: &Rc<Ctx>) {
    let Some(ui) = current_window() else { return };
    let profile = SSHProfile { kind: SSHSessionKind::Local, name: "本地终端".into(), ..Default::default() };
    start_terminal(ctx, ui, &profile, "", true, None);
}

fn start_terminal(ctx: &Rc<Ctx>, ui: crate::MainWindow, profile: &SSHProfile, profile_id: &str, local: bool, cwd: Option<&str>) {
    let active = ctx.active.borrow().clone();
    let tv = ui.global::<TermHostUI>();
    // 终端视图可能尚未完成布局（不可见时宽度为 0），给一个可用下限，
    // 真正尺寸由 feed_tab 的几何轮询在首帧后校正。
    let cols = (tv.get_cols_f().round() as usize).max(20);
    let rows = (tv.get_rows_f().round() as usize).max(6);

    let data_dir = devkit_core::prefs::data_dir().map(std::path::PathBuf::from);
    let keys = KeyStorage::new(data_dir.as_deref().unwrap_or(std::path::Path::new(".")));

    set_banner(ctx.clone(), 1, "连接中…");
    let term = if local {
        SshTerminal::connect_local(rows as u16, cols as u16, cwd)
    } else {
        SshTerminal::connect(profile, &keys, rows as u16, cols as u16, true)
    };
    match term {
        Ok(terminal) => {
            let mut runs = ctx.ssh_runs.borrow_mut();
            let Some(run) = runs.get_mut(active.as_str()) else { return };
            run.session = TerminalSession::new(cols, rows);
            run.model = Rc::new(VecModel::default());
            run.profile = Some(profile.clone());
            run.profile_id = if profile_id.is_empty() { None } else { Some(profile_id.to_string()) };
            run.terminal = Some(terminal);
            // 喂数据定时器
            let ctx2 = ctx.clone();
            let tab_id = active.clone();
            let timer = slint::Timer::default();
            timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(33), move || {
                feed_tab(&ctx2, &tab_id);
            });
            run.timer = Some(timer);
            run.banner_kind = 1;
            drop(runs);
            if local {
                set_banner(ctx.clone(), 2, "已连接");
            } else {
                set_banner(ctx.clone(), 1, "连接中…");
            }
            attach_active_terminal(ctx, &ui);
        }
        Err(e) => {
            set_banner(ctx.clone(), 3, &format!("{e}"));
        }
    }
}

/// 喂某个标签的终端数据（激活标签额外刷新共享 UI 模型）。
pub fn feed_tab(ctx: &Rc<Ctx>, tab_id: &str) {
    let is_active = ctx.active.borrow().clone() == tab_id;
    let cols_rows = if is_active {
        current_window().map(|ui| {
            let tv = ui.global::<TermHostUI>();
            (
                (tv.get_cols_f().round() as usize).max(2),
                (tv.get_rows_f().round() as usize).max(2),
            )
        })
    } else {
        None
    };

    let should_pulse = {
        let mut runs = ctx.ssh_runs.borrow_mut();
        let Some(run) = runs.get_mut(tab_id) else { return };

        // 1) 几何校正：视图尺寸变化（含首次可见）后同步网格与 PTY。
        if let Some((cols, rows)) = cols_rows {
            if cols != run.session.grid.cols || rows != run.session.grid.rows {
                run.session.resize(cols, rows);
                if let Some(t) = run.terminal.as_ref() {
                    let _ = t.pty.resize(cols as u16, rows as u16);
                }
                run.session.grid.dirty = true;
            }
        }

        let Some(t) = run.terminal.as_mut() else { return };
        let chunks = t.pty.drain_output();
        let mut inject = Vec::new();
        for c in &chunks {
            inject.extend(t.on_output(c));
        }
        if !inject.is_empty() {
            let _ = t.pty.write(&inject);
        }
        for c in chunks {
            run.session.feed(&c);
        }

        // 2) 进程退出但状态还停在「连接中」：明确反馈，避免黑屏干等。
        if !t.pty.is_alive() {
            if t.state == SSHConnectionState::Connecting {
                t.state = SSHConnectionState::Failed("连接失败：会话进程已退出".into());
            } else if let SSHConnectionState::Connected(_) = t.state {
                t.state = SSHConnectionState::Disconnected;
            }
        }

        if matches!(t.state, SSHConnectionState::Failed(_)) || t.state == SSHConnectionState::Disconnected {
            if run.session.grid.cursor_visible {
                run.session.grid.cursor_visible = false;
                run.session.grid.dirty = true;
            }
        }

        if run.session.grid.dirty {
            run.session.grid.dirty = false;
            let mut ok = false;
            if is_active {
                ACTIVE_FRAME.with(|c| {
                    if let Some(frame) = c.borrow_mut().as_mut() {
                        frame.sync(&run.session.grid);
                        ok = true;
                    }
                });
            }
            ok
        } else {
            false
        }
    };

    if is_active {
        sync_state_to_ui(ctx, tab_id);
    }

    if should_pulse {
        if let Some(ui) = current_window() {
            let tv = ui.global::<TermHostUI>();
            let pulse = tv.get_pulse();
            tv.set_pulse(pulse + 1);
        }
    }
}

/// 把终端引擎的真实状态映射到连接横幅（仅在激活标签、状态变化时写 UI）。
fn sync_state_to_ui(ctx: &Rc<Ctx>, tab_id: &str) {
    let Some(_ui) = current_window() else { return };
    let (kind, text) = {
        let runs = ctx.ssh_runs.borrow();
        let Some(run) = runs.get(tab_id) else { return };
        let Some(t) = run.terminal.as_ref() else { return };
        let local = run.profile.as_ref().map(|p| p.is_local()).unwrap_or(false);
        let (kind, text) = match &t.state {
            SSHConnectionState::Connecting => (1, "连接中…".to_string()),
            SSHConnectionState::Connected(_) => (2, if local { "本地终端".to_string() } else { "远程会话".to_string() }),
            SSHConnectionState::Failed(msg) => (3, msg.clone()),
            SSHConnectionState::Disconnected => (4, "会话已结束".to_string()),
            SSHConnectionState::Idle => (0, String::new()),
        };
        (kind, text)
    };
    let changed = {
        let runs = ctx.ssh_runs.borrow();
        runs.get(tab_id).map(|r| r.banner_kind != kind).unwrap_or(false)
    };
    if !changed {
        return;
    }
    if let Some(run) = ctx.ssh_runs.borrow_mut().get_mut(tab_id) {
        run.banner_kind = kind;
    }
    set_banner(ctx.clone(), kind, &text);
}

thread_local! {
    /// 激活标签的帧构建器（单实例，因为 UI 只有一个 TerminalView）。
    static ACTIVE_FRAME: RefCell<Option<crate::term_frame::TermFrame>> = const { RefCell::new(None) };
}

/// 把激活标签的模型挂到共享 TerminalView。
pub fn attach_active_terminal(ctx: &Rc<Ctx>, ui: &crate::MainWindow) {
    let active = ctx.active.borrow().clone();
    let tv = ui.global::<TermHostUI>();
    let model = {
        let mut runs = ctx.ssh_runs.borrow_mut();
        let Some(run) = runs.get_mut(active.as_str()) else { return };
        run.model = Rc::new(VecModel::default());
        run.model.clone()
    };
    tv.set_lines(ModelRc::from(model.clone()));
    // 终端配色跟随应用深浅色（与 Theme.terminal-bg 保持一致）。
    let dark = ui.global::<crate::Theme>().get_dark();
    let palette = if dark {
        devkit_tools::terminal::dark_palette()
    } else {
        devkit_tools::terminal::light_palette()
    };
    tv.set_term_bg(if dark {
        slint::Color::from_rgb_u8(0x18, 0x18, 0x18)
    } else {
        slint::Color::from_rgb_u8(0xfc, 0xfc, 0xfd)
    });
    let mut frame = crate::term_frame::TermFrame::new(model, palette);
    {
        let runs = ctx.ssh_runs.borrow();
        if let Some(run) = runs.get(active.as_str()) {
            frame.sync(&run.session.grid);
        }
    }
    ACTIVE_FRAME.with(|c| *c.borrow_mut() = Some(frame));
    let pulse = tv.get_pulse();
    tv.set_pulse(pulse + 1);
}

/// 激活 SSH 标签时调用：挂载模型 + 刷新面板。
pub fn sync_to_ui(ctx: &Rc<Ctx>, ui: &crate::MainWindow) {
    let active = ctx.active.borrow().clone();
    let state = ctx
        .ssh_runs
        .borrow()
        .get(active.as_str())
        .and_then(|r| r.terminal.as_ref().map(|t| t.state.clone()));
    {
        let ssh_ui = ui.global::<SshUI>();
        match state {
            None => {
                ssh_ui.set_state_kind(0);
                ssh_ui.set_connected(false);
                ssh_ui.set_connect_summary("".into());
            }
            Some(ref st) => {
                ssh_ui.set_connected(matches!(st, SSHConnectionState::Connected(_)));
            }
        }
    }
    if state.is_some() {
        if let Some(run) = ctx.ssh_runs.borrow_mut().get_mut(active.as_str()) {
            run.banner_kind = -1;
        }
        sync_state_to_ui(ctx, &active);
    }
    attach_active_terminal(ctx, ui);
    rebuild_profiles(ctx);
}

/// 生成默认文件夹（首次使用时）。
pub fn ensure_default_folder(ctx: &Rc<Ctx>) {
    let Some(db) = ctx.db() else { return };
    if let Ok(folders) = sstore::list_folders(&db) {
        if folders.is_empty() {
            let _ = sstore::create_folder(&db, None, "我的连接");
        }
    }
}
