//! 应用外壳：启动链路（引导 → 打库 → 恢复会话）、标签管理、工具粘合。
//! 对应 macOS 端 AppState + TabManager + 各工具 Shell 装配。

use crate::registry::DESCRIPTORS;
use crate::{AppState, HttpUI, JsonUI, LauncherTool, MainWindow, TabItemData, Theme};
use anyhow::Result;
use devkit_core::db::Database;
use devkit_core::session::{SessionSnapshot, SessionStore, TabGroupSnapshot, TabSnapshot, WindowSessionSnapshot};
use devkit_core::tool::ids;
use devkit_tools::json::tool::JsonTool;
use devkit_tools::json::SortMode;
use devkit_tools::toolchain::setup::{self, Tool};
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// JSON 标签运行时。
pub struct JsonRun {
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
    /// Git 后台线程 → UI 的消息通道。
    pub git_chan: RefCell<Option<crossbeam_channel::Sender<crate::glue_git::GitMsg>>>,
    pub git_rx: RefCell<Option<crossbeam_channel::Receiver<crate::glue_git::GitMsg>>>,
    /// 轮询 Git 后台消息的定时器。
    pub git_timer: RefCell<Option<slint::Timer>>,
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
            git_chan: RefCell::new(None),
            git_rx: RefCell::new(None),
            git_timer: RefCell::new(None),
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

/// 更新标签的「未保存」标记（侧栏圆点）。
pub fn set_tab_dirty(ctx: &Rc<Ctx>, id: &str, dirty: bool) {
    let model = tabs_model_handle();
    for i in 0..model.row_count() {
        if let Some(mut t) = model.row_data(i) {
            if t.id.as_str() == id {
                if t.dirty != dirty {
                    t.dirty = dirty;
                    model.set_row_data(i, t);
                    if let Some(ui) = current_window() {
                        rebuild_sidebar(ctx, &ui);
                    }
                }
                break;
            }
        }
    }
}

/// 标签是否有未保存内容（当前仅 HTTP 工具）。
pub fn tab_has_unsaved(ctx: &Rc<Ctx>, id: &str) -> bool {
    ctx.http_runs
        .borrow()
        .get(id)
        .map(|r| r.dirty)
        .unwrap_or(false)
}

/// 请求关闭标签：若有未保存内容则先弹确认框。
fn request_close_tab(ui: &MainWindow, ctx: &Rc<Ctx>, id: &str) {
    if tab_has_unsaved(ctx, id) {
        let state = ui.global::<AppState>();
        state.set_confirm_target(id.into());
        state.set_confirm_message("该标签有未保存的内容，关闭后这些修改将丢失。".into());
        state.set_confirm_visible(true);
        return;
    }
    close_tab_impl(ui, ctx, id);
}

/// 关闭标签的公共实现（on_close_tab / Ctrl+W 共用）。
fn close_tab_impl(ui: &MainWindow, ctx: &Rc<Ctx>, id: &str) {
    let state = ui.global::<AppState>();
    let model = tabs_model_handle();
    let was_active = ctx.active.borrow().as_str() == id;
    ctx.json_runs.borrow_mut().remove(id);
    ctx.http_runs.borrow_mut().remove(id);
    ctx.sftp_runs.borrow_mut().remove(id);
    ctx.git_runs.borrow_mut().remove(id);
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

/// 把标签移入指定分组（gid 为空 = 移出分组）。分组选择器与拖拽落放共用。
fn move_tab_to_group_impl(ui: &MainWindow, ctx: &Rc<Ctx>, tab_id: &str, gid: &str) {
    let new_gid = if gid.is_empty() { None } else { uuid::Uuid::parse_str(gid).ok() };
    if let Ok(mut tabs) = ctx.tabs.try_borrow_mut() {
        if let Some(e) = tabs.iter_mut().find(|e| e.id.to_string() == tab_id) {
            e.group_id = new_gid;
        }
    }
    let model = tabs_model_handle();
    for i in 0..model.row_count() {
        if let Some(mut t) = model.row_data(i) {
            if t.id.as_str() == tab_id {
                t.group_id = gid.to_string().into();
                model.set_row_data(i, t);
                break;
            }
        }
    }
    rebuild_sidebar(ctx, ui);
    save_session(ctx);
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
            .filter(|t| t.group_id == Some(g.id))
            .collect();
        let color_index: i32 = g.color_raw.parse().unwrap_or(0);
        rows.push(crate::SidebarRow {
            kind: 0,
            name: g.name.clone().into(),
            color_index,
            collapsed: g.is_collapsed,
            group_id: g.id.to_string().into(),
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
                        color_index,
                        collapsed: false,
                        group_id: g.id.to_string().into(),
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
    let (ui, ctx) = setup()?;
    // ---- 运行 ----
    // 先显示窗口：preferred-size 要等窗口映射后才落到 winit，此时尺寸才可读可改。
    ui.show()?;
    {
        // 窗口映射、首帧渲染完成前 size() 恒为 0，这里等事件循环稳定后再收敛。
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(250), move || {
            if weak.upgrade().is_some() {
                clamp_window_to_workarea();
            }
        });
    }
    let result = ui.run();

    // 退出前保存会话
    if ctx.db.borrow().is_some() {
        save_session(&ctx);
    }
    result?;
    Ok(())
}

/// 无头渲染平台：用 Slint 软件渲染器把窗口画到内存缓冲里。
struct ScreenshotPlatform {
    window: Rc<slint::platform::software_renderer::MinimalSoftwareWindow>,
    buffer: Rc<RefCell<Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>>>,
}

impl slint::platform::Platform for ScreenshotPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::platform::PlatformError> {
        Ok(self.window.clone())
    }

    fn run_event_loop(&self) -> Result<(), slint::platform::PlatformError> {
        // 走 Slint 的定时器/动画驱动，而不是自己 sleep：
        // 否则界面里的定时器（终端喂数据、Git 轮询、Toast 等）根本不会触发，
        // 无头截图只能拿到静态首帧，测不出任何交互行为。
        let window = self.window.clone();
        let buffer = self.buffer.clone();
        // DEVKIT_SHOT_MS：截图模式运行时长（默认 2.5 秒），便于等更慢的启动过程。
        let run_ms = std::env::var("DEVKIT_SHOT_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(2500);
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(run_ms);
        let mut size = (0u32, 0u32);
        while std::time::Instant::now() < deadline {
            // 触发定时器 / 动画 / 属性变更回调（必须在渲染前）。
            slint::platform::update_timers_and_animations();

            // 尺寸变化时重建缓冲。
            let sz = window.size();
            if sz.width > 0 && sz.height > 0 {
                let (w, h) = (sz.width as u32, sz.height as u32);
                if size != (w, h) {
                    size = (w, h);
                    *buffer.borrow_mut() =
                        Some(slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(w, h));
                }
            }

            // 画一帧。
            {
                let mut buf = buffer.borrow_mut();
                if let Some(b) = buf.as_mut() {
                    let stride = b.width() as usize;
                    window.draw_if_needed(|renderer| {
                        let _ = renderer.render(b.make_mut_slice(), stride);
                    });
                }
            }

            // 等下一个定时器（最多一帧 16ms）。
            let wait = slint::platform::duration_until_next_timer_update()
                .unwrap_or(std::time::Duration::from_millis(16))
                .min(std::time::Duration::from_millis(16));
            std::thread::sleep(wait);
        }
        Ok(())
    }
}

/// 无头渲染一张界面截图（排障 / 文档 / 回归测试用）。
///
/// 用 Slint 的软件渲染器离屏渲染，不需要桌面会话，
/// 因此可以在没有显示器的环境（CI / 远程 shell）里检查界面排版。
/// 期间跑的是真实事件循环（定时器、回调都会触发），所以截到的是
/// 「应用运行 2.5 秒后」的画面，而不只是静态首帧。
pub fn screenshot(path: &str) -> Result<()> {
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};

    // 排障/测试模式：不写会话快照，别动用户已保存的标签。
    NO_SESSION_SAVE.with(|c| *c.borrow_mut() = true);

    let msw = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let buffer: Rc<RefCell<Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>>> =
        Rc::new(RefCell::new(None));
    let platform = ScreenshotPlatform {
        window: msw.clone(),
        buffer: buffer.clone(),
    };
    slint::platform::set_platform(Box::new(platform))
        .map_err(|e| anyhow::anyhow!("设置软件渲染平台失败：{e}"))?;

    let (ui, ctx) = setup()?;
    ui.show()?;
    // 窗口显示后再设置尺寸，确保适配器拿到正确的物理尺寸。
    ui.window().set_size(slint::PhysicalSize::new(1400, 900));
    msw.set_size(slint::PhysicalSize::new(1400, 900));
    // DEVKIT_SHOT_TOOL=http|ssh|sftp|git|json|launcher|settings：截图前切到指定界面。
    let shot = std::env::var("DEVKIT_SHOT_TOOL").unwrap_or_default();
    if shot == "dark" {
        let theme = ui.global::<Theme>();
        theme.set_dark(true);
    } else if shot == "launcher" {
        ui.global::<AppState>().invoke_new_tab("".into());
    } else if shot == "settings" {
        ui.global::<AppState>().invoke_settings_opened();
    } else if !shot.is_empty() {
        ui.global::<AppState>().invoke_new_tab(shot.as_str().into());
    }
    // DEVKIT_SHOT_REPO=<路径>：登记并打开一个 Git 仓库（验证工作台排版）。
    if let Ok(repo) = std::env::var("DEVKIT_SHOT_REPO") {
        if !repo.is_empty() {
            crate::glue_git::register_and_open(&ctx, &repo);
        }
    }
    // DEVKIT_SHOT_HTTP_TAB=<0..5>：切到 HTTP 工具的某个分区。
    if let Ok(tab) = std::env::var("DEVKIT_SHOT_HTTP_TAB") {
        if let Ok(idx) = tab.parse::<i32>() {
            ui.global::<HttpUI>().set_editor_tab(idx);
        }
    }
    // DEVKIT_SHOT_HTTP_AUTH=<0|1|2>[,<0|1|2>...]：切换 HTTP 认证方式（回归测试用）。
    // 认证方式下拉框 → 触发 on_auth_kind_changed → 曾经因 RefCell 重复借出而闪退。
    for kind in env_list("DEVKIT_SHOT_HTTP_AUTH") {
        ui.global::<HttpUI>().invoke_auth_kind_changed(kind);
    }
    // DEVKIT_SHOT_HTTP_BODY=<0..3>[,...]：切换 HTTP Body 类型（回归测试用，同上）。
    for kind in env_list("DEVKIT_SHOT_HTTP_BODY") {
        ui.global::<HttpUI>().invoke_body_kind_changed(kind);
    }
    // DEVKIT_SHOT_HTTP_TIMEOUT=<次数>：切换 HTTP 空闲超时档位（回归测试用，同上）。
    for _ in env_list("DEVKIT_SHOT_HTTP_TIMEOUT") {
        ui.global::<HttpUI>().invoke_timeout_cycled();
    }
    // DEVKIT_SHOT_SSH_LOCAL=1：打开本地终端并执行一条多行输出命令（回归测试用）。
    // 配合截图可验证「终端新输出自动贴底」：截图里应看到命令的最后几行而不是开头。
    if std::env::var("DEVKIT_SHOT_SSH_LOCAL").is_ok() {
        crate::glue_ssh::connect_local(&ctx);
        let ui_weak = ui.as_weak();
        // shell 启动需要时间：等一会儿再发命令（DEVKIT_SHOT_MS 可延长总时长）。
        let run_ms = std::env::var("DEVKIT_SHOT_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(2500);
        let delay = (run_ms / 2).clamp(800, 2500);
        slint::Timer::single_shot(std::time::Duration::from_millis(delay), move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let tv = ui.global::<crate::TermHostUI>();
            tv.invoke_send_key("1..60 | % { \"line $_\" }\r".into(), false, false, false);
        });
    }
    // 事件循环在 ScreenshotPlatform::run_event_loop 里跑完（渲染若干帧后返回）。
    let _ = ui.run();

    let buffer = buffer.borrow().clone();
    let Some(buffer) = buffer else {
        anyhow::bail!("没有渲染出任何画面");
    };
    let (w, h) = (buffer.width(), buffer.height());
    let mut img = image::RgbImage::new(w, h);
    for (dst, src) in img.pixels_mut().zip(buffer.as_slice().iter()) {
        dst[0] = src.r;
        dst[1] = src.g;
        dst[2] = src.b;
    }
    img.save(path).map_err(|e| anyhow::anyhow!("保存截图失败：{e}"))?;
    Ok(())
}

/// 构建主窗口与共享上下文（不进入事件循环）。
fn setup() -> Result<(MainWindow, Rc<Ctx>)> {
    // Slint 只在启动时读取缩放覆盖值，必须在创建窗口之前设置。
    apply_ui_scale();
    let ui = MainWindow::new()?;
    CURRENT_WINDOW.with(|c| *c.borrow_mut() = Some(ui.as_weak()));
    let theme = ui.global::<Theme>();
    let state = ui.global::<AppState>();

    // ---- 外观模式 ----
    let mode = devkit_core::prefs::get_int(devkit_core::prefs::KEY_APPEARANCE_MODE).unwrap_or(0);
    state.set_appearance_mode(mode as i32);
    apply_appearance(&theme, mode);

    // ---- 界面缩放 ----
    let ui_scale = devkit_core::prefs::get_int(devkit_core::prefs::KEY_UI_SCALE).unwrap_or(0);
    state.set_ui_scale(ui_scale as i32);
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
    // Git 后台任务的消息通道 + 轮询定时器（所有 git 子进程都在后台线程跑）。
    {
        let (tx, rx) = crossbeam_channel::unbounded::<crate::glue_git::GitMsg>();
        *ctx.git_chan.borrow_mut() = Some(tx);
        *ctx.git_rx.borrow_mut() = Some(rx);
        let ctx2 = ctx.clone();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(120),
            move || {
                crate::glue_git::poll_git(&ctx2);
            },
        );
        *ctx.git_timer.borrow_mut() = Some(timer);
    }
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

    // 刷新外部运行工具状态（内置便携版 → PATH）
    refresh_tool_status(&ui);

    // ---- 回调：外壳 ----
    {
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_new_tab(move |tool_id| {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            // 空 tool_id = 侧栏「新标签」/Ctrl+T：回到 Launcher（对齐 macOS ⌘T）
            if tool_id.is_empty() {
                state.set_launcher_visible(true);
                state.set_active_id("".into());
                state.set_active_tool_id("".into());
                *ctx.active.borrow_mut() = String::new();
                save_session(&ctx);
                return;
            }
            let Some(desc) = crate::registry::find(&tool_id) else { return };
            let model = tabs_model_handle();
            // 五个工具都允许多实例（对齐 macOS 端 allowsMultipleInstances）：
            // 每次从 Launcher 打开都新建一个标签页，互不干扰。
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
            request_close_tab(&ui, &ctx, &id);
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
                request_close_tab(&ui, &ctx, &id);
            }
        });
    }
    {
        // 关闭确认框
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_confirm_close_yes(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            let id = state.get_confirm_target().to_string();
            state.set_confirm_visible(false);
            state.set_confirm_target("".into());
            if !id.is_empty() {
                close_tab_impl(&ui, &ctx, &id);
            }
        });
    }
    {
        let ui = ui.as_weak();
        state.on_confirm_close_no(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            state.set_confirm_visible(false);
            state.set_confirm_target("".into());
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
        // Ctrl+S：保存当前标签（对应 macOS ⌘S；仅 HTTP 工具支持保存）
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_save_current(move || {
            let Some(ui) = ui.upgrade() else { return };
            let active = ctx.active.borrow().clone();
            let is_http = ctx
                .tabs
                .borrow()
                .iter()
                .any(|t| t.id.to_string() == active && t.tool_id == ids::HTTP);
            if is_http {
                ui.global::<HttpUI>().invoke_save();
            }
        });
    }
    {
        // Ctrl+↓/↑：下一个/上一个标签（环绕，对应 macOS ⌃↓/⌃↑）
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_cycle_tab(move |delta| {
            let Some(ui) = ui.upgrade() else { return };
            let tabs = ctx.tabs.borrow().clone();
            if tabs.is_empty() {
                return;
            }
            let active = ctx.active.borrow().clone();
            let next = match tabs.iter().position(|t| t.id.to_string() == active) {
                Some(i) => (i as isize + delta as isize).rem_euclid(tabs.len() as isize) as usize,
                // 无选中时对齐 macOS：↓ 回到第一个，↑ 到最后一个
                None => {
                    if delta >= 0 {
                        0
                    } else {
                        tabs.len() - 1
                    }
                }
            };
            let id = tabs[next].id.to_string();
            let model = tabs_model_handle();
            select_tab(&ui, &ctx, model.as_ref(), &id);
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
            refresh_tool_status(&ui);
        });
    }
    {
        // 一键安装运行工具（0=git 1=ssh）：后台线程下载解压，进度经事件循环回 UI
        let ui = ui.as_weak();
        state.on_install_tool(move |which| {
            let Some(w) = ui.upgrade() else { return };
            {
                let state = w.global::<AppState>();
                if state.get_tools_installing() {
                    return;
                }
                state.set_tools_installing(true);
                state.set_tools_status(
                    (if which == 1 { "正在获取 OpenSSH 下载地址…" } else { "正在获取 Git 下载地址…" }).into(),
                );
            }
            let ui2 = ui.clone();
            std::thread::Builder::new()
                .name("tool-setup".into())
                .spawn(move || {
                    let tool = if which == 1 { Tool::Ssh } else { Tool::Git };
                    let label: &str = if which == 1 { "OpenSSH" } else { "Git" };
                    let ui3 = ui2.clone();
                    let progress = move |pct: u8| {
                        let ui4 = ui3.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = ui4.upgrade() {
                                w.global::<AppState>()
                                    .set_tools_status(format!("{label} 下载中 {pct}%…").into());
                            }
                        });
                    };
                    let result = setup::install(tool, &progress);
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(w) = ui2.upgrade() else { return };
                        let state = w.global::<AppState>();
                        state.set_tools_installing(false);
                        match result {
                            Ok(dir) => {
                                state.set_tools_status(
                                    format!("{label} 已就绪（{}）", dir.display()).into(),
                                );
                                refresh_tool_status(&w);
                            }
                            Err(e) => {
                                state.set_tools_status(format!("{label} 安装失败：{e:#}").into());
                            }
                        }
                    });
                })
                .ok();
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
    {
        let ui = ui.as_weak();
        state.on_set_ui_scale(move |level| {
            let Some(ui) = ui.upgrade() else { return };
            ui.global::<AppState>().set_ui_scale(level);
            let _ = devkit_core::prefs::set_int(devkit_core::prefs::KEY_UI_SCALE, level as u32);
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
            move_tab_to_group_impl(&ui, &ctx, &tab_id, &gid);
            state.set_group_picker_visible(false);
        });
    }
    {
        // 标签拖拽落放：落点分组由分组头行在悬停时写入 drag-hover-group
        let ctx = ctx.clone();
        let ui = ui.as_weak();
        state.on_tab_dropped(move || {
            let Some(ui) = ui.upgrade() else { return };
            let state = ui.global::<AppState>();
            let tab_id = state.get_drag_tab_id().to_string();
            let gid = state.get_drag_hover_group().to_string();
            state.set_drag_active(false);
            state.set_drag_hover_group("".into());
            state.set_drag_tab_id("".into());
            if !gid.is_empty() && !tab_id.is_empty() {
                move_tab_to_group_impl(&ui, &ctx, &tab_id, &gid);
            }
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

    // 首屏工具卡片：启动即填充（此后仅在搜索时由 on_launcher_search 刷新）
    refresh_launcher(&state, "");

    Ok((ui, ctx))
}

/// 打开（或创建）数据目录与数据库，并恢复会话。
fn init_data_dir(ui: &MainWindow, ctx: &Rc<Ctx>, dir: &str) -> Result<()> {
    devkit_core::prefs::validate_writable(dir)?;
    devkit_core::prefs::set_data_dir(dir)?;
    let db = Database::open(&std::path::Path::new(dir).join("devkit.sqlite3"))?;
    *ctx.db.borrow_mut() = Some(db);
    // 数据目录可能被重置过，先清空既有标签，避免恢复时追加重复标签。
    clear_tabs(ctx);
    restore_session(ui, ctx);
    // 数据目录变化会影响 tools 解析（应用内安装位置）
    refresh_tool_status(ui);
    Ok(())
}

/// 清空所有标签与运行时（切换数据目录前调用）。
fn clear_tabs(ctx: &Rc<Ctx>) {
    {
        let mut runs = ctx.ssh_runs.borrow_mut();
        for (_, run) in runs.drain() {
            if let Some(mut t) = run.terminal {
                t.pty.kill();
            }
        }
    }
    ctx.json_runs.borrow_mut().clear();
    ctx.http_runs.borrow_mut().clear();
    ctx.sftp_runs.borrow_mut().clear();
    ctx.git_runs.borrow_mut().clear();
    ctx.tabs.borrow_mut().clear();
    ctx.groups.borrow_mut().clear();
    *ctx.active.borrow_mut() = String::new();
    if let Some(ui) = current_window() {
        let state = ui.global::<AppState>();
        state.set_active_id("".into());
        state.set_active_tool_id("".into());
        state.set_launcher_visible(true);
        tabs_model_handle().clear();
    }
}

/// 刷新外部运行工具（git / OpenSSH）状态：设置页展示 + 工具页缺失横幅。
fn refresh_tool_status(ui: &MainWindow) {
    let state = ui.global::<AppState>();
    state.set_git_missing(!devkit_tools::toolchain::git_available());
    state.set_ssh_missing(!devkit_tools::toolchain::ssh_available());
    state.set_git_path(devkit_tools::toolchain::git_path().unwrap_or_default().into());
    state.set_ssh_path(devkit_tools::toolchain::ssh_path().unwrap_or_default().into());
}

/// 选择标签：加载目标状态到全局镜像。
fn select_tab(ui: &MainWindow, ctx: &Rc<Ctx>, _model: &VecModel<TabItemData>, id: &str) {
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
        let _ctx = ctx.clone();
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
        let ctx = ctx.clone();
        json_ui.on_tree_copied(move |index| {
            let Some(ui) = current_window() else { return };
            let json_ui = ui.global::<JsonUI>();
            let active = ctx.active.borrow().clone();
            let path = {
                let runs = ctx.json_runs.borrow();
                runs.get(active.as_str()).and_then(|r| r.flat_paths.get(index as usize).cloned())
            };
            let Some(path) = path else { return };
            let runs = ctx.json_runs.borrow();
            let Some(run) = runs.get(active.as_str()) else { return };
            let Some(value) = run.tool.value_at(&path) else {
                set_status(&json_ui, "无法定位该节点");
                return;
            };
            let text = devkit_tools::json::format::pretty(&value, devkit_tools::json::Indent::Two);
            drop(runs);
            match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
                Ok(()) => set_status(&json_ui, "已复制该节点"),
                Err(e) => set_status(&json_ui, &format!("复制失败：{e}")),
            }
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
    json_ui.set_sort_desc(desc);
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
        } else if tab.tool_id == ids::SFTP {
            // SFTP 会话（连接状态）不持久化，但必须建立运行时，
            // 否则恢复出来的标签所有回调都会静默失效。
            ctx.sftp_runs.borrow_mut().insert(id.clone(), Default::default());
        } else if tab.tool_id == ids::GIT {
            let mut run = crate::glue_git::GitRun::default();
            if let Some(ts) = &tab.tool_state {
                if let Ok(s) = serde_json::from_value::<crate::glue_git::GitToolState>(ts.clone()) {
                    run.repo_id = s.repo_id;
                }
            }
            ctx.git_runs.borrow_mut().insert(id.clone(), run);
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

thread_local! {
    /// 无头截图模式下跳过会话写盘（排障/回归测试不应污染用户已保存的标签）。
    static NO_SESSION_SAVE: RefCell<bool> = const { RefCell::new(false) };
}

/// 读取形如 "1,2,3" 的环境变量，返回其中解析成功的整数（供截图模式批量触发回调）。
fn env_list(key: &str) -> Vec<i32> {
    std::env::var(key)
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse::<i32>().ok())
        .collect()
}

/// 保存会话。
fn save_session(ctx: &Rc<Ctx>) {
    // 无头截图模式只用于排障/回归测试，不该改写用户已保存的标签集合。
    if NO_SESSION_SAVE.with(|c| *c.borrow()) {
        return;
    }
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
            ids::GIT => ctx
                .git_runs
                .borrow()
                .get(t.id.to_string().as_str())
                .and_then(|r| {
                    serde_json::to_value(&crate::glue_git::GitToolState {
                        repo_id: r.repo_id.clone(),
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
/// 用户选择的界面缩放档位（0=跟随系统，1=110%，2=125%，3=150%）。
///
/// Slint 没有运行期缩放 API，只能在创建窗口前用 `SLINT_SCALE_FACTOR` 覆盖渲染比例。
/// 该变量是「绝对值」而非倍数，所以必须叠加系统 DPI 比例，否则在高分屏上会把
/// 系统已经放大的界面又缩回去。默认档位 0 不设置变量，保留 winit 的多屏自适应。
fn apply_ui_scale() {
    let level = devkit_core::prefs::get_int(devkit_core::prefs::KEY_UI_SCALE).unwrap_or(0);
    let factor = match level {
        1 => 1.1_f32,
        2 => 1.25,
        3 => 1.5,
        _ => return,
    };
    let base = system_dpi_scale();
    std::env::set_var("SLINT_SCALE_FACTOR", format!("{:.4}", base * factor));
}

/// 启动时把窗口收敛到当前显示器工作区内。
///
/// 窗口的理想尺寸是固定逻辑像素（1200x800），在大缩放下会换算成超过屏幕的物理尺寸，
/// 加上 Windows 的级联定位，底部状态栏会被任务栏压住。这里读真实窗口矩形，
/// 只在溢出时按工作区的 92% 收缩并居中。用 Win32 而非 Slint API，是因为窗口刚映射时
/// `Window::size()` 仍返回 0，且 set_size 不会调整位置。
fn clamp_window_to_workarea() {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Rect {
            left: i32,
            top: i32,
            right: i32,
            bottom: i32,
        }
        #[link(name = "user32")]
        extern "system" {
            fn SystemParametersInfoW(
                action: u32,
                param: u32,
                data: *mut core::ffi::c_void,
                win_ini: u32,
            ) -> i32;
            fn GetActiveWindow() -> isize;
            fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
            fn GetCurrentProcessId() -> u32;
            fn GetWindowRect(hwnd: isize, rect: *mut Rect) -> i32;
            fn SetWindowPos(
                hwnd: isize,
                after: isize,
                x: i32,
                y: i32,
                cx: i32,
                cy: i32,
                flags: u32,
            ) -> i32;
        }
        const SPI_GETWORKAREA: u32 = 0x0030;
        const SWP_NOZORDER: u32 = 0x0004;
        const SWP_NOACTIVATE: u32 = 0x0010;

        let mut work = Rect { left: 0, top: 0, right: 0, bottom: 0 };
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                &mut work as *mut Rect as *mut core::ffi::c_void,
                0,
            )
        };
        if ok == 0 {
            return;
        }
        let hwnd = unsafe { GetActiveWindow() };
        if hwnd == 0 {
            return;
        }
        // 只操作本进程的窗口，避免活动窗口恰好是别的程序时误移。
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
        }
        if pid != unsafe { GetCurrentProcessId() } {
            return;
        }
        let mut cur = Rect { left: 0, top: 0, right: 0, bottom: 0 };
        if unsafe { GetWindowRect(hwnd, &mut cur) } == 0 {
            return;
        }
        let (cur_w, cur_h) = (cur.right - cur.left, cur.bottom - cur.top);
        let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);
        let max_w = (work_w as f32 * 0.92) as i32;
        let max_h = (work_h as f32 * 0.92) as i32;
        if cur_w <= max_w && cur_h <= max_h && cur.bottom <= work.bottom {
            return;
        }
        let w = cur_w.min(max_w);
        let h = cur_h.min(max_h);
        let x = work.left + (work_w - w) / 2;
        let y = work.top + (work_h - h) / 2;
        unsafe {
            SetWindowPos(hwnd, 0, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// 读取主显示器的 DPI 比例。调用前需让本进程具备 DPI 感知，
/// 否则 `GetDpiForSystem` 在虚拟化环境下恒返回 96。
#[cfg(windows)]
fn system_dpi_scale() -> f32 {
    #[link(name = "user32")]
    extern "system" {
        fn SetProcessDpiAwarenessContext(value: isize) -> i32;
        fn GetDpiForSystem() -> u32;
    }
    const PER_MONITOR_AWARE_V2: isize = -4;
    unsafe {
        let _ = SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2);
        let dpi = GetDpiForSystem();
        if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
    }
}

#[cfg(not(windows))]
fn system_dpi_scale() -> f32 {
    1.0
}

/// 应用外观模式（0 跟随系统 / 1 浅 / 2 深）。
///
/// 自绘控件用 `Theme.dark`；Slint 标准控件（输入框 / 下拉框 / 复选框）的配色由
/// MainWindow.slint 里的 `changed dark-mode` 同步到 `Palette.color-scheme`。
fn apply_appearance(theme: &Theme, mode: u32) {
    let dark = match mode {
        1 => false,
        2 => true,
        _ => devkit_core::appearance::system_prefers_dark(),
    };
    theme.set_dark(dark);
}

/// 跟随系统模式下轮询注册表（Windows 没有轻量的主题变更通知，
/// 用 1 秒轮询 + 变更比对，代价极低）。
fn spawn_appearance_watcher(ui: Weak<MainWindow>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
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
