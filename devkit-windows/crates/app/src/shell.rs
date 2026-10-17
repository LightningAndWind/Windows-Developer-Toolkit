//! 应用外壳：启动链路（引导 → 打库 → 恢复会话）、标签管理、工具粘合。
//! 对应 macOS 端 AppState + TabManager + 各工具 Shell 装配。

use crate::registry::DESCRIPTORS;
use crate::{AppState, JsonUI, LauncherTool, MainWindow, TabItemData, Theme};
use anyhow::Result;
use devkit_core::db::Database;
use devkit_core::session::{SessionSnapshot, SessionStore, TabSnapshot, WindowSessionSnapshot};
use devkit_core::tool::ids;
use devkit_tools::json::tool::JsonTool;
use devkit_tools::json::SortMode;
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// JSON 标签运行时。
struct JsonRun {
    tool: JsonTool,
    /// 树形展开集合（路径）。
    expanded: HashSet<Vec<usize>>,
    /// 最近一次扁平化对应的路径缓存（index → path），供 tree-toggled 使用。
    flat_paths: Vec<Vec<usize>>,
}

/// 单个标签元数据。
#[derive(Clone)]
pub struct TabEntry {
    pub id: uuid::Uuid,
    pub tool_id: String,
    pub custom_title: Option<String>,
    pub group_id: Option<uuid::Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 共享上下文（所有 UI 回调持有 Rc）。
pub struct Ctx {
    pub db: RefCell<Option<Database>>,
    pub tabs: RefCell<Vec<TabEntry>>,
    pub json_runs: RefCell<HashMap<String, JsonRun>>,
    pub http_runs: RefCell<HashMap<String, crate::glue_http::HttpRun>>,
    pub ssh_runs: RefCell<HashMap<String, crate::glue_ssh::SshRun>>,
    pub sftp_runs: RefCell<HashMap<String, crate::glue_sftp::SftpRun>>,
    pub git_runs: RefCell<HashMap<String, crate::glue_git::GitRun>>,
    pub groups: RefCell<Vec<TabGroupSnapshot>>,
    pub json_debounce: RefCell<Option<slint::Timer>>,
    pub status_timer: RefCell<Option<slint::Timer>>,
    pub active: RefCell<String>,
}

impl Ctx {
    fn new() -> Self {
        Self {
            db: RefCell::new(None),
            tabs: RefCell::new(Vec::new()),
            json_runs: RefCell::new(HashMap::new()),
            http_runs: RefCell::new(HashMap::new()),
            ssh_runs: RefCell::new(HashMap::new()),
            sftp_runs: RefCell::new(HashMap::new()),
            git_runs: RefCell::new(HashMap::new()),
            groups: RefCell::new(Vec::new()),
            json_debounce: RefCell::new(None),
            status_timer: RefCell::new(None),
            active: RefCell::new(String::new()),
        }
    }
}

/// 当前主窗口句柄（工具粘合层使用）。
pub fn current_window() -> Option<MainWindow> {
    CURRENT_WINDOW.with(|c| c.borrow().as_ref().and_then(|w| w.upgrade()))
}

impl Ctx {
    /// 数据库克隆句柄（未打开返回 None）。
    pub fn db(&self) -> Option<Database> {
        self.db.borrow().as_ref().cloned()
    }
}

thread_local! {
    static CURRENT_WINDOW: RefCell<Option<slint::Weak<MainWindow>>> = const { RefCell::new(None) };
    static TABS_MODEL: RefCell<Option<Rc<VecModel<TabItemData>>>> = const { RefCell::new(None) };
    static RENAME_TARGET: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// 关闭标签的公共实现（on_close_tab / Ctrl+W 共用）。
fn close_tab_impl(ui: &MainWindow, ctx: &Rc<Ctx>, id: &str) {
    let state = ui.global::<AppState>();
    let model = tabs_model_handle();
    let was_active = ctx.active.borrow().as_str() == id;
    ctx.json_runs.borrow_mut().remove(id);
    ctx.http_runs.borrow_mut().remove(id);
    ctx.sftp_runs.borrow_mut().remove(id);
    {
        let mut runs = ctx.ssh_runs.borrow_mut();
        if let Some(run) = runs.remove(id) {
            if let Some(mut t) = run.terminal {
                t.pty.kill();
            }
        }
    }
    ctx.tabs.borrow_mut().retain(|t| t.id.to_string() != id);
    let mut idx = None;
    for i in 0..model.row_count() {
        if model.row_data(i).map(|t| t.id == id).unwrap_or(false) {
            idx = Some(i);
        }
    }
    if let Some(i) = idx {
        model.remove(i);
        if was_active {
            let next = model.row_count();
            if next == 0 {
                state.set_launcher_visible(true);
                state.set_active_id("".into());
                state.set_active_tool_id("".into());
                *ctx.active.borrow_mut() = String::new();
            } else {
                let j = i.min(next - 1);
                if let Some(t) = model.row_data(j) {
                    select_tab(ui, ctx, model.as_ref(), &t.id.to_string());
                }
            }
        }
        save_session(ctx);
    }
    rebuild_sidebar(ctx, ui);
}

/// 填充设置面板数据（tab：0 通用 / 1 HTTP 历史 / 2 终端会话 / 3 Git 仓库）。
fn populate_settings(ctx: &Rc<Ctx>, ui: &MainWindow, tab: i32) {
    let state = ui.global::<AppState>();
    let dir = devkit_core::prefs::data_dir().unwrap_or_default();
    state.set_data_dir_path(dir.into());
    let Some(db) = ctx.db() else { return };
    let to_rows = |v: Vec<(String, String, String)>| -> Vec<crate::SettingsRow> {
        v.into_iter()
            .map(|(id, title, subtitle)| crate::SettingsRow {
                id: id.into(),
                title: title.into(),
                subtitle: subtitle.into(),
            })
            .collect()
    };
    match tab {
        0 => {
            let http_n = devkit_tools::http::stores::count_history(&db).unwrap_or(0);
            let ssh_n = devkit_tools::ssh::store::list_profiles(&db).map(|v| v.len()).unwrap_or(0);
            let git_n = devkit_tools::git::store::list_repos(&db).map(|v| v.len()).unwrap_or(0);
            state.set_settings_summary(
                format!("HTTP 历史记录 {http_n} 条 · 终端会话记录 {ssh_n} 条 · Git 仓库登记 {git_n} 个\n\n删除操作立即生效且不可恢复。").into(),
            );
        }
        1 => {
            let rows = devkit_tools::http::stores::list_history(&db, 200).unwrap_or_default();
            let n = rows.len();
            state.set_settings_rows(ModelRc::from(Rc::new(VecModel::from(to_rows(
                rows.into_iter()
                    .map(|h| {
                        (
                            h.id,
                            format!("{} {}", h.method, h.url),
                            crate::glue_http::format_time(h.created_at),
                        )
                    })
                    .collect(),
            )))));
            state.set_settings_summary(format!("共 {n} 条历史记录").into());
        }
        2 => {
            let rows = devkit_tools::ssh::store::list_profiles(&db).unwrap_or_default();
            let n = rows.len();
            state.set_settings_rows(ModelRc::from(Rc::new(VecModel::from(to_rows(
                rows.into_iter()
                    .map(|p| {
                        let sub = if p.profile.is_local() {
                            "本地终端".to_string()
                        } else {
                            p.profile.connect_summary()
                        };
                        (p.id, p.profile.name, sub)
                    })
                    .collect(),
            )))));
            state.set_settings_summary(format!("共 {n} 条终端会话记录").into());
        }
        _ => {
            let rows = devkit_tools::git::store::list_repos(&db).unwrap_or_default();
            let n = rows.len();
            state.set_settings_rows(ModelRc::from(Rc::new(VecModel::from(to_rows(
                rows.into_iter()
                    .map(|r| (r.id, r.repo.alias, r.repo.path))
                    .collect(),
            )))));
            state.set_settings_summary(format!("共 {n} 个仓库登记").into());
        }
    }
}

fn tabs_model_handle() -> Rc<VecModel<TabItemData>> {
    TABS_MODEL.with(|t| t.borrow().clone().expect("tabs model 未初始化"))
}

/// 重建侧栏分组行模型（分组头 + 成员 + 未分组）。
fn rebuild_sidebar(ctx: &Rc<Ctx>, ui: &MainWindow) {
    let state = ui.global::<AppState>();
    let model = tabs_model_handle();
    let mut by_id: HashMap<String, TabItemData> = HashMap::new();
    for i in 0..model.row_count() {
        if let Some(t) = model.row_data(i) {
            by_id.insert(t.id.to_string(), t);
        }
    }
    let groups = ctx.groups.borrow();
    let tabs = ctx.tabs.borrow();
    let mut rows: Vec<crate::SidebarRow> = Vec::new();
    for g in groups.iter() {
        let members: Vec<&TabEntry> = tabs
            .iter()
            .filter(|t| t.group_id.map(|g2| g2.to_string() == g.id).unwrap_or(false))
            .collect();
        let color_index: i32 = g.color_raw.parse().unwrap_or(0);
        rows.push(crate::SidebarRow {
            kind: 0,
            name: g.name.clone().into(),
            color_index,
            collapsed: g.is_collapsed,
            group_id: g.id.clone().into(),
            tab: TabItemData {
                id: "".into(),
                tool_id: "".into(),
                title: "".into(),
                icon: "".into(),
                dirty: false,
                group_id: "".into(),
            },
            group_count: members.len() as i32,
        });
        if !g.is_collapsed {
            for m in members {
                if let Some(td) = by_id.get(&m.id.to_string()) {
                    rows.push(crate::SidebarRow {
                        kind: 1,
                        name: "".into(),
                        color_index: 0,
                        collapsed: false,
                        group_id: g.id.clone().into(),
                        tab: td.clone(),
                        group_count: 0,
                    });
                }
            }
        }
    }
    for t in tabs.iter().filter(|t| t.group_id.is_none()) {
        if let Some(td) = by_id.get(&t.id.to_string()) {
            rows.push(crate::SidebarRow {
                kind: 1,
                name: "".into(),
                color_index: 0,
                collapsed: false,
                group_id: "".into(),
                tab: td.clone(),
                group_count: 0,
            });
        }
    }
    state.set_sidebar_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
}

pub fn run_app() -> Result<()> {
    let ui = MainWindow::new()?;
    CURRENT_WINDOW.with(|c| *c.borrow_mut() = Some(ui.as_weak()));
    let theme = ui.global::<Theme>();
    let state = ui.global::<AppState>();

    // ---- 外观模式 ----
    let mode = devkit_core::prefs::get_int(devkit_core::prefs::KEY_APPEARANCE_MODE).unwrap_or(0);
    state.set_appearance_mode(mode as i32);
    apply_appearance(&theme, mode);
    spawn_appearance_watcher(ui.as_weak());

    // ---- 侧栏宽度 ----
    let sidebar =
        devkit_core::prefs::get_int(devkit_core::prefs::KEY_SIDEBAR_WIDTH).unwrap_or(220) as f32;
    state.set_sidebar_width(sidebar.clamp(180.0, 320.0));

    // ---- 防休眠 ----
    let prevent =
        devkit_core::prefs::get_int(devkit_core::prefs::KEY_PREVENT_SLEEP).unwrap_or(0) == 1;
    state.set_prevent_sleep(prevent);
    devkit_core::sleep::set_prevent_sleep(prevent);

    // ---- 共享上下文 ----
    let ctx = Rc::new(Ctx::new());
    let tabs_model: Rc<VecModel<TabItemData>> = Rc::new(VecModel::default());
    state.set_tabs(ModelRc::from(tabs_model.clone()));
    TABS_MODEL.with(|t| *t.borrow_mut() = Some(tabs_model.clone()));

    // ---- 数据目录 / 引导 ----
    match devkit_core::prefs::data_dir() {
        Some(dir) => {
            if let Err(e) = init_data_dir(&ui, &ctx, &dir) {
                state.set_needs_onboarding(true);
                state.set_onboarding_error(format!("数据目录打开失败：{e}").into());
            }
        }
        None => {
            state.set_needs_onboarding(true);
        }
    }

    // ---- 回调：外壳 ----
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_new_tab(move |tool_id| {
            let Some(ui) = ui.upgrade() else { return };
            let Some(desc) = crate::registry::find(&tool_id) else { return };
            let model = tabs_model_handle();
            // 单实例工具：已打开则直接选中
            {
                let tabs = ctx.tabs.borrow();
                if let Some(t) = tabs.iter().find(|t| t.tool_id == desc.id) {
                    let id = t.id.to_string();
                    drop(tabs);
                    select_tab(&ui, &ctx, model.as_ref(), &id);
                    return;
                }
            }
            let id = uuid::Uuid::new_v4();
            if desc.id == ids::JSON {
                ctx.json_runs.borrow_mut().insert(
                    id.to_string(),
                    JsonRun {
                        tool: JsonTool::default(),
                        expanded: HashSet::new(),
                        flat_paths: Vec::new(),
                    },
                );
            } else if desc.id == ids::HTTP {
                ctx.http_runs.borrow_mut().insert(id.to_string(), Default::default());
            } else if desc.id == ids::SSH {
                ctx.ssh_runs.borrow_mut().insert(id.to_string(), Default::default());
            } else if desc.id == ids::SFTP {
                ctx.sftp_runs.borrow_mut().insert(id.to_string(), Default::default());
            } else if desc.id == ids::GIT {
                ctx.git_runs.borrow_mut().insert(id.to_string(), Default::default());
            }
            ctx.tabs.borrow_mut().push(TabEntry {
                id,
                tool_id: desc.id.to_string(),
                custom_title: None,
                group_id: None,
                created_at: chrono::Utc::now(),
            });
            model.push(TabItemData {
                id: id.to_string().into(),
                tool_id: desc.id.into(),
                title: desc.title.into(),
                icon: desc.icon.into(),
                dirty: false,
                group_id: "".into(),
            });
            select_tab(&ui, &ctx, model.as_ref(), &id.to_string());
            rebuild_sidebar(&ctx, &ui);
            save_session(&ctx);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_select_tab(move |id| {
            let Some(ui) = ui.upgrade() else { return };
            let model = tabs_model_handle();
            select_tab(&ui, &ctx, model.as_ref(), &id);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_close_tab(move |id| {
            let Some(ui) = ui.upgrade() else { return };
            close_tab_impl(&ui, &ctx, &id);
        });
    }
    {
        // Ctrl+W：关闭当前标签
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_close_current_tab(move || {
            let Some(ui) = ui.upgrade() else { return };
            let id = ctx.active.borrow().clone();
            if !id.is_empty() {
                close_tab_impl(&ui, &ctx, &id);
            }
        });
    }
    {
        // Ctrl+1..9/0：按序号切换标签
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_select_tab_index(move |index| {
            let Some(ui) = ui.upgrade() else { return };
            let model = tabs_model_handle();
            if (index as usize) < model.row_count() {
                if let Some(t) = model.row_data(index as usize) {
                    select_tab(&ui, &ctx, model.as_ref(), &t.id.to_string());
                }
            }
        });
    }
    {
        // 重命名（双击标签 / Ctrl+E）
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_rename_requested(move |id| {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            let entry = ctx.tabs.borrow().iter().find(|t| t.id.to_string() == id.as_str()).cloned();
            let Some(entry) = entry else { return };
            let desc = crate::registry::find(&entry.tool_id);
            let current = entry
                .custom_title
                .clone()
                .unwrap_or_else(|| desc.map(|d| d.title).unwrap_or("").to_string());
            state.set_rename_input(current.into());
            state.set_rename_dialog_visible(true);
            RENAME_TARGET.with(|c| *c.borrow_mut() = Some(id.to_string()));
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_rename_confirm(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            let target = RENAME_TARGET.with(|c| c.borrow().clone());
            let Some(target) = target else { return };
            let name = state.get_rename_input().trim().to_string();
            let model = tabs_model_handle();
            // 更新 runtime 标题 + 模型
            for i in 0..model.row_count() {
                if let Some(t) = model.row_data(i) {
                    if t.id.as_str() == target {
                        let mut t2 = t;
                        t2.title = if name.is_empty() {
                            crate::registry::find(
                                ctx.tabs
                                    .borrow()
                                    .iter()
                                    .find(|e| e.id.to_string() == target)
                                    .map(|e| e.tool_id.as_str())
                                    .unwrap_or(""),
                            )
                            .map(|d| d.title)
                            .unwrap_or("未知工具")
                            .into()
                        } else {
                            name.clone().into()
                        };
                        model.set_row_data(i, t2);
                        break;
                    }
                }
            }
            if let Ok(mut tabs) = ctx.tabs.try_borrow_mut() {
                if let Some(e) = tabs.iter_mut().find(|e| e.id.to_string() == target) {
                    e.custom_title = if name.is_empty() { None } else { Some(name) };
                }
            }
            state.set_rename_dialog_visible(false);
            RENAME_TARGET.with(|c| *c.borrow_mut() = None);
            rebuild_sidebar(&ctx, &ui);
            save_session(&ctx);
        });
    }
    {
        state.on_rename_cancel(move || {
            RENAME_TARGET.with(|c| *c.borrow_mut() = None);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_settings_opened(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            state.set_settings_visible(true);
            state.set_settings_tab(0);
            populate_settings(&ctx, &ui, 0);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_settings_closed(move || {
            let Some(ui) = ui.upgrade() else { return };
            ui.global::<AppState>().set_settings_visible(false);
            let _ = ctx;
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_settings_tab_changed(move |tab| {
            let Some(ui) = ui.upgrade() else { return };
            ui.global::<AppState>().set_settings_tab(tab);
            populate_settings(&ctx, &ui, tab);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_settings_row_deleted(move |id| {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            let tab = state.get_settings_tab();
            if let Some(db) = ctx.db() {
                let _ = match tab {
                    1 => devkit_tools::http::stores::delete_history(&db, &id),
                    2 => devkit_tools::ssh::store::delete_profile(&db, &id),
                    3 => devkit_tools::git::store::delete_repo(&db, &id),
                    _ => Ok(()),
                };
            }
            populate_settings(&ctx, &ui, tab);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_settings_history_cleared(move || {
            let Some(ui) = ui.upgrade() else { return };
            if let Some(db) = ctx.db() {
                let _ = devkit_tools::http::stores::clear_history(&db);
            }
            populate_settings(&ctx, &ui, 1);
        });
    }
    {
        state.on_open_data_dir(move || {
            if let Some(dir) = devkit_core::prefs::data_dir() {
                let _ = open::that(dir);
            }
        });
    }
    {
        let ui = ui.as_weak();
        state.on_set_appearance(move |mode| {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            state.set_appearance_mode(mode);
            let _ = devkit_core::prefs::set_int(devkit_core::prefs::KEY_APPEARANCE_MODE, mode as u32);
            apply_appearance(&ui.global::<Theme>(), mode as u32);
        });
    }
    // ---- 标签分组（M7）----
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_new_group(move || {
            let Some(ui) = ui.upgrade() else { return };
            let mut groups = ctx.groups.borrow_mut();
            let n = groups.len() + 1;
            groups.push(TabGroupSnapshot {
                id: uuid::Uuid::new_v4(),
                name: format!("分组 {n}"),
                color_raw: ((n - 1) % 9).to_string(),
                is_collapsed: false,
            });
            drop(groups);
            rebuild_sidebar(&ctx, &ui);
            save_session(&ctx);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_delete_group(move |id| {
            let Some(ui) = ui.upgrade() else { return };
            ctx.groups.borrow_mut().retain(|g| g.id.to_string() != id.as_str());
            let gid = uuid::Uuid::parse_str(&id).ok();
            if let Ok(mut tabs) = ctx.tabs.try_borrow_mut() {
                for t in tabs.iter_mut() {
                    if t.group_id.is_some() && t.group_id == gid {
                        t.group_id = None;
                    }
                }
            }
            let model = tabs_model_handle();
            for i in 0..model.row_count() {
                if let Some(mut t) = model.row_data(i) {
                    if t.group_id == id {
                        t.group_id = "".into();
                        model.set_row_data(i, t);
                    }
                }
            }
            rebuild_sidebar(&ctx, &ui);
            save_session(&ctx);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_toggle_group(move |id| {
            let Some(ui) = ui.upgrade() else { return };
            if let Ok(mut groups) = ctx.groups.try_borrow_mut() {
                if let Some(g) = groups.iter_mut().find(|g| g.id.to_string() == id.as_str()) {
                    g.is_collapsed = !g.is_collapsed;
                }
            }
            rebuild_sidebar(&ctx, &ui);
            save_session(&ctx);
        });
    }
    {
        state.on_open_group_picker(move |tab_id| {
            let Some(ui) = current_window() else { return };
            let state = ui.global::<AppState>();
            state.set_group_picker_tab(tab_id.clone());
            state.set_group_picker_visible(true);
        });
    }
    {
        state.on_close_group_picker(move || {
            let Some(ui) = current_window() else { return };
            ui.global::<AppState>().set_group_picker_visible(false);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_move_tab_to_group(move |gid| {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            let tab_id = state.get_group_picker_tab().to_string();
            let new_gid = if gid.is_empty() { None } else { uuid::Uuid::parse_str(&gid).ok() };
            if let Ok(mut tabs) = ctx.tabs.try_borrow_mut() {
                if let Some(e) = tabs.iter_mut().find(|e| e.id.to_string() == tab_id) {
                    e.group_id = new_gid;
                }
            }
            let model = tabs_model_handle();
            for i in 0..model.row_count() {
                if let Some(mut t) = model.row_data(i) {
                    if t.id.as_str() == tab_id.as_str() {
                        t.group_id = gid.clone();
                        model.set_row_data(i, t);
                        break;
                    }
                }
            }
            state.set_group_picker_visible(false);
            rebuild_sidebar(&ctx, &ui);
            save_session(&ctx);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_new_group_for_picker(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            let tab_id = state.get_group_picker_tab().to_string();
            let gid = uuid::Uuid::new_v4();
            {
                let mut groups = ctx.groups.borrow_mut();
                let n = groups.len() + 1;
                groups.push(TabGroupSnapshot {
                    id: gid,
                    name: format!("分组 {n}"),
                    color_raw: ((n - 1) % 9).to_string(),
                    is_collapsed: false,
                });
            }
            if let Ok(mut tabs) = ctx.tabs.try_borrow_mut() {
                if let Some(e) = tabs.iter_mut().find(|e| e.id.to_string() == tab_id) {
                    e.group_id = Some(gid);
                }
            }
            let model = tabs_model_handle();
            for i in 0..model.row_count() {
                if let Some(mut t) = model.row_data(i) {
                    if t.id.as_str() == tab_id.as_str() {
                        t.group_id = gid.to_string().into();
                        model.set_row_data(i, t);
                        break;
                    }
                }
            }
            state.set_group_picker_visible(false);
            rebuild_sidebar(&ctx, &ui);
            save_session(&ctx);
        });
    }
    {
        let ui = ui.as_weak();
        state.on_sidebar_width_changed(move || {
            let Some(ui) = ui.upgrade() else { return };
            let w = ui.global::<AppState>().get_sidebar_width();
            let _ = devkit_core::prefs::set_int(
                devkit_core::prefs::KEY_SIDEBAR_WIDTH,
                w.round().clamp(180.0, 320.0) as u32,
            );
        });
    }
    {
        let ui = ui.as_weak();
        state.on_sleep_toggled(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            let on = !state.get_prevent_sleep();
            state.set_prevent_sleep(on);
            devkit_core::sleep::set_prevent_sleep(on);
            let _ = devkit_core::prefs::set_int(devkit_core::prefs::KEY_PREVENT_SLEEP, on as u32);
        });
    }
    {
        let ui = ui.as_weak();
        state.on_appearance_cycled(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            let next = (state.get_appearance_mode() + 1) % 3;
            state.set_appearance_mode(next);
            let _ =
                devkit_core::prefs::set_int(devkit_core::prefs::KEY_APPEARANCE_MODE, next as u32);
            apply_appearance(&ui.global::<Theme>(), next as u32);
        });
    }
    {
        let ui = ui.as_weak();
        state.on_launcher_search(move |q| {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            refresh_launcher(&state, &q);
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_choose_data_dir(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            if let Some(dir) = rfd::FileDialog::new().set_title("选择数据目录").pick_folder() {
                let dir = dir.to_string_lossy().to_string();
                match init_data_dir(&ui, &ctx, &dir) {
                    Ok(()) => state.set_needs_onboarding(false),
                    Err(e) => state.set_onboarding_error(format!("目录不可用：{e}").into()),
                }
            }
        });
    }
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_use_default_dir(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            let base = std::env::var("APPDATA").unwrap_or_else(|_| {
                format!("{}\\AppData\\Roaming", std::env::var("USERPROFILE").unwrap_or_default())
            });
            let dir = format!("{base}\\devkit");
            match init_data_dir(&ui, &ctx, &dir) {
                Ok(()) => state.set_needs_onboarding(false),
                Err(e) => state.set_onboarding_error(format!("默认目录不可用：{e}").into()),
            }
        });
    }

    // ---- 回调：JSON 工具 ----
    wire_json_callbacks(ctx.clone());
    // ---- 回调：HTTP 工具 ----
    crate::glue_http::wire_http_callbacks(ctx.clone());
    // ---- 回调：SSH 工具 ----
    crate::glue_ssh::wire_ssh_callbacks(ctx.clone());
    // ---- 回调：SFTP 工具 ----
    crate::glue_sftp::wire_sftp_callbacks(ctx.clone());
    // ---- 回调：Git 工具 ----
    crate::glue_git::wire_git_callbacks(ctx.clone());

    // ---- 运行 ----
    let result = ui.run();

    // 退出前保存会话
    if ctx.db.borrow().is_some() {
        save_session(&ctx);
    }
    result?;
    Ok(())
}

/// 打开（或创建）数据目录与数据库，并恢复会话。
fn init_data_dir(ui: &MainWindow, ctx: &Rc<Ctx>, dir: &str) -> Result<()> {
    devkit_core::prefs::validate_writable(dir)?;
    devkit_core::prefs::set_data_dir(dir)?;
    let db = Database::open(&std::path::Path::new(dir).join("devkit.sqlite3"))?;
    *ctx.db.borrow_mut() = Some(db);
    restore_session(ui, ctx);
    Ok(())
}

/// 选择标签：加载目标状态到全局镜像。
fn select_tab(ui: &MainWindow, ctx: &Rc<Ctx>, model: &VecModel<TabItemData>, id: &str) {
    if ctx.active.borrow().clone() == id {
        return;
    }
    let state = ui.global::<AppState>();
    let entry = ctx.tabs.borrow().iter().find(|t| t.id.to_string() == id).cloned();
    let Some(entry) = entry else { return };
    *ctx.active.borrow_mut() = id.to_string();
    state.set_active_id(id.into());
    state.set_active_tool_id(entry.tool_id.as_str().into());
    state.set_launcher_visible(false);
    if entry.tool_id == ids::JSON {
        let json_ui = ui.global::<JsonUI>();
        {
            let runs = ctx.json_runs.borrow();
            if let Some(r) = runs.get(id) {
                json_ui.set_text(r.tool.text.as_str().into());
                json_ui.set_indent_mode(indent_index(r.tool.indent) as i32);
                json_ui.set_view_mode(r.tool.view_mode as i32);
                json_ui.set_valid(r.tool.parse().is_ok());
            }
        }
        validate_json(ctx, ui);
    } else if entry.tool_id == ids::HTTP {
        crate::glue_http::sync_to_ui(ctx, ui);
    } else if entry.tool_id == ids::SSH {
        crate::glue_ssh::sync_to_ui(ctx, ui);
    } else if entry.tool_id == ids::SFTP {
        crate::glue_sftp::sync_all(ctx, ui);
    } else if entry.tool_id == ids::GIT {
        crate::glue_git::sync_repo_open(ctx);
        crate::glue_git::rebuild_repos(ctx);
        crate::glue_git::refresh_all(ctx);
    }
    save_session(ctx);
}

fn indent_index(i: devkit_tools::json::Indent) -> u8 {
    match i {
        devkit_tools::json::Indent::Two => 0,
        devkit_tools::json::Indent::Four => 1,
        devkit_tools::json::Indent::Tab => 2,
    }
}

fn indent_from_index(i: i32) -> devkit_tools::json::Indent {
    match i {
        1 => devkit_tools::json::Indent::Four,
        2 => devkit_tools::json::Indent::Tab,
        _ => devkit_tools::json::Indent::Two,
    }
}

/// 注册 JSON 工具全部回调。
fn wire_json_callbacks(ctx: Rc<Ctx>) {
    let Some(ui) = current_window() else { return };
    let json_ui = ui.global::<JsonUI>();

    {
        let ctx = ctx.clone();
        json_ui.on_text_edited(move |t| {
            let active = ctx.active.borrow().clone();
            if let Some(run) = ctx.json_runs.borrow_mut().get_mut(active.as_str()) {
                run.tool.text = t.to_string();
            }
            // 防抖 300ms 校验
            let ctx2 = ctx.clone();
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(300),
                move || {
                    let Some(ui) = current_window() else { return };
                    validate_json(&ctx2, &ui);
                },
            );
            *ctx.json_debounce.borrow_mut() = Some(timer);
        });
    }
    {
        let ctx = ctx.clone();
        json_ui.on_view_changed(move |mode| {
            let Some(ui) = current_window() else { return };
            let json_ui = ui.global::<JsonUI>();
            json_ui.set_view_mode(mode);
            let active = ctx.active.borrow().clone();
            if let Some(run) = ctx.json_runs.borrow_mut().get_mut(active.as_str()) {
                run.tool.view_mode = mode as u8;
            }
            if mode == 1 {
                rebuild_json_tree(&ctx, &ui);
            }
        });
    }
    {
        let ctx = ctx.clone();
        json_ui.on_format(move || {
            apply_json_transform(&ctx, "已格式化", |run| run.tool.formatted());
        });
    }
    {
        let ctx = ctx.clone();
        json_ui.on_compress(move || {
            apply_json_transform(&ctx, "已压缩", |run| run.tool.compressed());
        });
    }
    {
        let ctx = ctx.clone();
        json_ui.on_sort_asc(move || apply_json_sort(&ctx, false));
    }
    {
        let ctx = ctx.clone();
        json_ui.on_sort_desc_btn(move || apply_json_sort(&ctx, true));
    }
    {
        let ctx = ctx.clone();
        json_ui.on_indent_cycled(move || {
            let Some(ui) = current_window() else { return };
            let json_ui = ui.global::<JsonUI>();
            let next = (json_ui.get_indent_mode() + 1) % 3;
            json_ui.set_indent_mode(next);
            let active = ctx.active.borrow().clone();
            if let Some(run) = ctx.json_runs.borrow_mut().get_mut(active.as_str()) {
                run.tool.indent = indent_from_index(next);
            }
            let label = match next {
                0 => "2 空格",
                1 => "4 空格",
                _ => "Tab",
            };
            set_status(&json_ui, &format!("缩进已切换为 {label}"));
        });
    }
    {
        let ctx = ctx.clone();
        json_ui.on_copy_all(move || {
            let Some(ui) = current_window() else { return };
            let json_ui = ui.global::<JsonUI>();
            let text = json_ui.get_text().to_string();
            match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
                Ok(()) => set_status(&json_ui, "已复制全部内容"),
                Err(e) => set_status(&json_ui, &format!("复制失败：{e}")),
            }
        });
    }
    {
        let ctx = ctx.clone();
        json_ui.on_tree_toggled(move |index| {
            let Some(ui) = current_window() else { return };
            let active = ctx.active.borrow().clone();
            let path = {
                let runs = ctx.json_runs.borrow();
                runs.get(active.as_str()).and_then(|r| r.flat_paths.get(index as usize).cloned())
            };
            let Some(path) = path else { return };
            if let Some(run) = ctx.json_runs.borrow_mut().get_mut(active.as_str()) {
                if run.expanded.contains(&path) {
                    run.expanded.remove(&path);
                } else {
                    run.expanded.insert(path);
                }
            }
            rebuild_json_tree(&ctx, &ui);
        });
    }
    {
        json_ui.on_tree_copied(move |_index| {
            // 复制子树：后续增强
        });
    }

    // 状态栏自动清除
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(2600),
        || {
            let Some(ui) = current_window() else { return };
            let json_ui = ui.global::<JsonUI>();
            if !json_ui.get_status().is_empty() {
                json_ui.set_status("".into());
            }
        },
    );
    *ctx.status_timer.borrow_mut() = Some(timer);
}

fn set_status(json_ui: &JsonUI, msg: &str) {
    json_ui.set_status(msg.into());
}

/// 对当前激活 JSON 标签应用一个变换（格式化/压缩）。
fn apply_json_transform(
    ctx: &Rc<Ctx>,
    ok_msg: &str,
    f: impl FnOnce(&JsonRun) -> Result<String, devkit_tools::json::JsonError>,
) {
    let Some(ui) = current_window() else { return };
    let json_ui = ui.global::<JsonUI>();
    let active = ctx.active.borrow().clone();
    let mut runs = ctx.json_runs.borrow_mut();
    let Some(run) = runs.get_mut(active.as_str()) else { return };
    match f(run) {
        Ok(text) => {
            run.tool.text = text.clone();
            drop(runs);
            json_ui.set_text(text.into());
            validate_json(ctx, &ui);
            set_status(&json_ui, ok_msg);
        }
        Err(e) => {
            json_ui.set_valid(false);
            json_ui.set_error_line(e.line as i32);
            json_ui.set_error_col(e.col as i32);
            json_ui.set_error_msg(e.message.into());
        }
    }
}

fn apply_json_sort(ctx: &Rc<Ctx>, desc: bool) {
    let Some(ui) = current_window() else { return };
    let json_ui = ui.global::<JsonUI>();
    let recursive = json_ui.get_sort_recursive();
    let mode = match (desc, recursive) {
        (false, true) => SortMode::AscRecursive,
        (true, true) => SortMode::DescRecursive,
        (false, false) => SortMode::AscTop,
        (true, false) => SortMode::DescTop,
    };
    let active = ctx.active.borrow().clone();
    let mut runs = ctx.json_runs.borrow_mut();
    let Some(run) = runs.get_mut(active.as_str()) else { return };
    match run.tool.sorted(mode) {
        Ok(text) => {
            run.tool.text = text.clone();
            drop(runs);
            json_ui.set_text(text.into());
            validate_json(ctx, &ui);
            set_status(&json_ui, "键已排序");
        }
        Err(e) => {
            json_ui.set_valid(false);
            json_ui.set_error_line(e.line as i32);
            json_ui.set_error_col(e.col as i32);
            json_ui.set_error_msg(e.message.into());
        }
    }
}

/// 校验当前 JSON 文本并刷新状态栏 / 树。
fn validate_json(ctx: &Rc<Ctx>, ui: &MainWindow) {
    let json_ui = ui.global::<JsonUI>();
    let active = ctx.active.borrow().clone();
    if active.is_empty() {
        return;
    }
    let runs = ctx.json_runs.borrow();
    let Some(run) = runs.get(active.as_str()) else { return };
    match run.tool.parse() {
        Ok(_) => {
            json_ui.set_valid(true);
            json_ui.set_error_line(-1);
            if let Some(stats) = run.tool.stats() {
                json_ui.set_stats_line(stats.line().into());
            }
        }
        Err(e) => {
            json_ui.set_valid(false);
            json_ui.set_error_line(e.line as i32);
            json_ui.set_error_col(e.col as i32);
            json_ui.set_error_msg(e.message.into());
            json_ui.set_stats_line("".into());
        }
    }
    drop(runs);
    rebuild_json_tree(ctx, ui);
}

/// 重建树形模型。
fn rebuild_json_tree(ctx: &Rc<Ctx>, ui: &MainWindow) {
    let json_ui = ui.global::<JsonUI>();
    if json_ui.get_view_mode() != 1 {
        return;
    }
    let active = ctx.active.borrow().clone();
    let (items, paths) = {
        let runs = ctx.json_runs.borrow();
        let Some(run) = runs.get(active.as_str()) else { return };
        let Some(rows) = run.tool.flatten_tree(&run.expanded) else { return };
        let paths = rows.iter().map(|n| n.path.clone()).collect::<Vec<_>>();
        let items: Vec<crate::JsonTreeNode> = rows
            .iter()
            .map(|n| crate::JsonTreeNode {
                key: n.key.clone().into(),
                kind: n.kind.into(),
                preview: n.preview.clone().into(),
                depth: n.depth as i32,
                expanded: n.expanded,
                has_children: n.has_children,
                child_count: n.child_count as i32,
            })
            .collect();
        (items, paths)
    };
    if let Some(run) = ctx.json_runs.borrow_mut().get_mut(active.as_str()) {
        run.flat_paths = paths;
    }
    json_ui.set_tree(ModelRc::from(Rc::new(VecModel::from(items))));
}

/// 恢复会话。
fn restore_session(ui: &MainWindow, ctx: &Rc<Ctx>) {
    let Some(db) = ctx.db() else { return };
    let Some(snapshot) = SessionStore::load(&db) else { return };
    let Some(window) = snapshot.windows.first() else { return };
    let model = tabs_model_handle();
    let mut restored_active = String::new();
    for tab in &window.tabs {
        let id = tab.id.to_string();
        if tab.tool_id == ids::JSON {
            let mut run = JsonRun {
                tool: JsonTool::default(),
                expanded: HashSet::new(),
                flat_paths: Vec::new(),
            };
            if let Some(ts) = &tab.tool_state {
                if let Ok(t) = serde_json::from_value::<JsonTool>(ts.clone()) {
                    run.tool = t;
                }
            }
            ctx.json_runs.borrow_mut().insert(id.clone(), run);
        } else if tab.tool_id == ids::HTTP {
            let mut run = crate::glue_http::HttpRun::default();
            if let Some(ts) = &tab.tool_state {
                if let Ok(s) = serde_json::from_value::<crate::glue_http::HttpToolState>(ts.clone()) {
                    run.request = s.request;
                    run.saved_id = s.saved_id;
                    run.saved_name = s.saved_name;
                }
            }
            ctx.http_runs.borrow_mut().insert(id.clone(), run);
        } else if tab.tool_id == ids::SSH {
            let mut run = crate::glue_ssh::SshRun::default();
            if let Some(ts) = &tab.tool_state {
                if let Ok(s) = serde_json::from_value::<crate::glue_ssh::SshToolState>(ts.clone()) {
                    run.profile_id = s.profile_id;
                }
            }
            ctx.ssh_runs.borrow_mut().insert(id.clone(), run);
        }
        ctx.tabs.borrow_mut().push(TabEntry {
            id: tab.id,
            tool_id: tab.tool_id.clone(),
            custom_title: tab.custom_title.clone(),
            group_id: tab.group_id,
            created_at: tab.created_at,
        });
        let desc = crate::registry::find(&tab.tool_id);
        model.push(TabItemData {
            id: id.clone().into(),
            tool_id: tab.tool_id.as_str().into(),
            title: tab
                .custom_title
                .clone()
                .unwrap_or_else(|| desc.map(|d| d.title).unwrap_or("未知工具").to_string())
                .into(),
            icon: desc.map(|d| d.icon).unwrap_or("?").into(),
            dirty: false,
            group_id: tab
                .group_id
                .map(|g| g.to_string())
                .unwrap_or_default()
                .into(),
        });
        if window.selected_tab_id == Some(tab.id) {
            restored_active = id;
        }
    }
    *ctx.groups.borrow_mut() = window.groups.clone();
    rebuild_sidebar(ctx, ui);
    if !restored_active.is_empty() {
        select_tab(ui, ctx, model.as_ref(), &restored_active);
    }
}

/// 保存会话。
fn save_session(ctx: &Rc<Ctx>) {
    let Some(db) = ctx.db() else { return };
    let tabs = ctx.tabs.borrow();
    let active = ctx.active.borrow().clone();
    let mut tab_snaps = Vec::new();
    let mut selected = None;
    for t in tabs.iter() {
        let tool_state = match t.tool_id.as_str() {
            ids::JSON => ctx
                .json_runs
                .borrow()
                .get(t.id.to_string().as_str())
                .and_then(|r| serde_json::to_value(&r.tool).ok()),
            ids::HTTP => {
                let runs = ctx.http_runs.borrow();
                runs.get(t.id.to_string().as_str()).and_then(|r| {
                    serde_json::to_value(&crate::glue_http::HttpToolState {
                        request: r.request.clone(),
                        saved_id: r.saved_id.clone(),
                        saved_name: r.saved_name.clone(),
                    })
                    .ok()
                })
            }
            ids::SSH => ctx
                .ssh_runs
                .borrow()
                .get(t.id.to_string().as_str())
                .and_then(|r| {
                    serde_json::to_value(&crate::glue_ssh::SshToolState {
                        profile_id: r.profile_id.clone(),
                    })
                    .ok()
                }),
            _ => None,
        };
        if t.id.to_string() == active {
            selected = Some(t.id);
        }
        tab_snaps.push(TabSnapshot {
            id: t.id,
            tool_id: t.tool_id.clone(),
            custom_title: t.custom_title.clone(),
            group_id: t.group_id,
            is_pinned: false,
            created_at: t.created_at,
            tool_state,
        });
    }
    let groups = ctx.groups.borrow().clone();
    let snapshot = SessionSnapshot {
        version: 1,
        saved_at: chrono::Utc::now(),
        windows: vec![WindowSessionSnapshot {
            window_id: uuid::Uuid::new_v4(),
            tabs: tab_snaps,
            groups,
            selected_tab_id: selected,
        }],
    };
    SessionStore::save(&db, &snapshot);
}

/// 应用外观模式（0 跟随系统 / 1 浅 / 2 深）。
fn apply_appearance(theme: &Theme, mode: u32) {
    let dark = match mode {
        1 => false,
        2 => true,
        _ => devkit_core::appearance::system_prefers_dark(),
    };
    theme.set_dark(dark);
}

/// 跟随系统模式下每 2 秒轮询注册表。
fn spawn_appearance_watcher(ui: Weak<MainWindow>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let ui = ui.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            if state.get_appearance_mode() == 0 {
                let theme = ui.global::<Theme>();
                let dark = devkit_core::appearance::system_prefers_dark();
                if theme.get_dark() != dark {
                    theme.set_dark(dark);
                }
            }
        });
    });
}

/// 按搜索词刷新 Launcher 四个分类模型。
fn refresh_launcher(state: &AppState, query: &str) {
    let q = query.trim().to_lowercase();
    let mut buckets: [Vec<LauncherTool>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for d in DESCRIPTORS {
        let hit = q.is_empty()
            || d.title.to_lowercase().contains(&q)
            || d.subtitle.to_lowercase().contains(&q)
            || d.category.label().contains(&q);
        if hit {
            let cat_idx = match d.category {
                devkit_core::tool::ToolCategory::Network => 0,
                devkit_core::tool::ToolCategory::Terminal => 1,
                devkit_core::tool::ToolCategory::Data => 2,
                devkit_core::tool::ToolCategory::Utility => 3,
            };
            buckets[cat_idx].push(LauncherTool {
                id: d.id.into(),
                title: d.title.into(),
                icon: d.icon.into(),
                subtitle: d.subtitle.into(),
            });
        }
    }
    let to_model = |v: Vec<LauncherTool>| ModelRc::from(Rc::new(VecModel::from(v)));
    state.set_tools_network(to_model(buckets[0].clone()));
    state.set_tools_terminal(to_model(buckets[1].clone()));
    state.set_tools_data(to_model(buckets[2].clone()));
    state.set_tools_utility(to_model(buckets[3].clone()));
}
