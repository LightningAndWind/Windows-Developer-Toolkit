//! HTTP 工具粘合层：UI 全局镜像 ↔ 每标签运行时 ↔ 存储层 ↔ 网络。
//! 对应 macOS 端 HTTPTool.swift 的视图模型职责。

use crate::shell::{current_window, Ctx};
use crate::{HttpHistoryRow, HttpTreeNode, HttpUI, KvRow, MainWindow};
use anyhow::Result;
use devkit_core::tool::ids;
use devkit_tools::http::client::{send_blocking, SendError};
use devkit_tools::http::curl::to_curl;
use devkit_tools::http::model::{
    AuthKind, BodyKind, HTTPKV, HTTPRequestModel, HTTPResponseModel,
};
use devkit_tools::http::stores as hstore;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// HTTP 标签运行时。
pub struct HttpRun {
    pub request: HTTPRequestModel,
    pub saved_id: Option<String>,
    pub saved_name: String,
    pub dirty: bool,
    /// 树展开集合（节点 id）。
    pub tree_expanded: HashSet<String>,
    /// 响应
    pub response: Option<HTTPResponseModel>,
    pub sending: bool,
    pub cancel: Option<Arc<AtomicBool>>,
    pub rx: Option<crossbeam_channel::Receiver<Result<HTTPResponseModel, SendError>>>,
    pub poll: Option<slint::Timer>,
    pub history_search: String,
}

impl Default for HttpRun {
    fn default() -> Self {
        Self {
            request: HTTPRequestModel::default(),
            saved_id: None,
            saved_name: String::new(),
            dirty: false,
            tree_expanded: HashSet::new(),
            response: None,
            sending: false,
            cancel: None,
            rx: None,
            poll: None,
            history_search: String::new(),
        }
    }
}

/// 随会话快照持久化的 HTTP 状态。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct HttpToolState {
    pub request: HTTPRequestModel,
    #[serde(default)]
    pub saved_id: Option<String>,
    #[serde(default)]
    pub saved_name: String,
}

pub fn wire_http_callbacks(ctx: Rc<Ctx>) {
    let Some(ui) = crate::shell::current_window() else { return };
    let http_ui = ui.global::<HttpUI>();

    macro_rules! on {
        ($name:ident, $($cap:expr),* ; $f:expr) => {
            http_ui.$name({
                $(let $cap = $cap.clone();)*
                move |$($cap,)*| { $f }
            });
        };
    }

    // ---- 发送 / 停止 ----
    {
        let ctx = ctx.clone();
        http_ui.on_send(move || {
            let Some(ui) = crate::shell::current_window() else { return };
            let active = ctx.active.borrow().clone();
            if active.is_empty() { return; }
            // 读取 UI → 运行时
            {
                let http_ui = ui.global::<HttpUI>();
                let mut runs = ctx.http_runs.borrow_mut();
                let Some(run) = runs.get_mut(active.as_str()) else { return };
                ui_to_http(&http_ui, &mut run.request);
                run.dirty = true;
            }
            let (tx, rx) = crossbeam_channel::unbounded();
            let cancel = Arc::new(AtomicBool::new(false));
            let req = {
                let runs = ctx.http_runs.borrow();
                let run = runs.get(active.as_str()).unwrap();
                run.request.clone()
            };
            let cancel2 = cancel.clone();
            std::thread::Builder::new().name("http-send".into()).spawn(move || {
                let result = send_blocking(&req, cancel2);
                let _ = tx.send(result);
            }).ok();
            let mut runs = ctx.http_runs.borrow_mut();
            let run = runs.get_mut(active.as_str()).unwrap();
            run.sending = true;
            run.cancel = Some(cancel);
            run.rx = Some(rx);
            // 轮询定时器（保活在 run 内）
            let ctx2 = ctx.clone();
            let tab_id = active.clone();
            let timer = slint::Timer::default();
            timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(120), move || {
                poll_send_result(&ctx2, &tab_id);
            });
            run.poll = Some(timer);
            let http_ui = ui.global::<HttpUI>();
            http_ui.set_sending(true);
            http_ui.set_resp_error("请求中…".into());
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_stop(move || {
            let active = ctx.active.borrow().clone();
            if let Some(run) = ctx.http_runs.borrow_mut().get_mut(active.as_str()) {
                if let Some(c) = &run.cancel {
                    c.store(true, Ordering::Relaxed);
                }
            }
        });
    }

    // ---- 分区切换 ----
    {
        let ctx = ctx.clone();
        http_ui.on_editor_tab_changed(move |tab| {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            http_ui.set_editor_tab(tab);
            if tab == 4 {
                reload_history(&ctx);
            }
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_resp_tab_changed(move |tab| {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            http_ui.set_resp_tab(tab);
            let active = ctx.active.borrow().clone();
            let runs = ctx.http_runs.borrow();
            if let Some(run) = runs.get(active.as_str()) {
                apply_response_view(&http_ui, run, tab as usize);
            }
        });
    }

    // ---- 表格 ----
    {
        let ctx = ctx.clone();
        http_ui.on_kv_edited(move |table, index, field, value| {
            edit_kv(&ctx, table as usize, index as usize, field as usize, &value);
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_kv_toggled(move |table, index| {
            edit_kv_bool(&ctx, table as usize, index as usize, |kv| kv.enabled = !kv.enabled);
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_kv_added(move |table| {
            let Some(ui) = crate::shell::current_window() else { return };
            let active = ctx.active.borrow().clone();
            let mut runs = ctx.http_runs.borrow_mut();
            let Some(run) = runs.get_mut(active.as_str()) else { return };
            let list = match table as usize {
                0 => &mut run.request.params,
                1 => &mut run.request.headers,
                _ => &mut run.request.body.form,
            };
            list.push(HTTPKV { key: String::new(), value: String::new(), enabled: true });
            run.dirty = true;
            drop(runs);
            sync_tables(&ctx, &ui.global::<HttpUI>(), &active);
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_kv_deleted(move |table, index| {
            let Some(ui) = crate::shell::current_window() else { return };
            let active = ctx.active.borrow().clone();
            let mut runs = ctx.http_runs.borrow_mut();
            let Some(run) = runs.get_mut(active.as_str()) else { return };
            let list = match table as usize {
                0 => &mut run.request.params,
                1 => &mut run.request.headers,
                _ => &mut run.request.body.form,
            };
            if (index as usize) < list.len() {
                list.remove(index as usize);
            }
            run.dirty = true;
            drop(runs);
            sync_tables(&ctx, &ui.global::<HttpUI>(), &active);
        });
    }

    // ---- URL / Body / Auth / 超时 ----
    {
        let ctx = ctx.clone();
        http_ui.on_url_parsed(move || {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            let url = http_ui.get_url().to_string();
            let mut params = Vec::new();
            if let Some((_, q)) = url.split_once('?') {
                for pair in q.split('&') {
                    if pair.is_empty() { continue; }
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    params.push(HTTPKV {
                        key: urldecode(k),
                        value: urldecode(v),
                        enabled: true,
                    });
                }
            }
            let active = ctx.active.borrow().clone();
            let mut runs = ctx.http_runs.borrow_mut();
            if let Some(run) = runs.get_mut(active.as_str()) {
                run.request.params = params.clone();
                run.dirty = true;
            }
            drop(runs);
            http_ui.set_params(ModelRc::from(Rc::new(VecModel::from(
                params.into_iter().map(|kv| KvRow {
                    key: kv.key.into(), value: kv.value.into(), enabled: kv.enabled,
                }).collect::<Vec<_>>()
            ))));
            set_status(&http_ui, "已从 URL 解析参数");
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_body_kind_changed(move |kind| {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            http_ui.set_body_kind(kind);
            let active = ctx.active.borrow().clone();
            let mut runs = ctx.http_runs.borrow_mut();
            if let Some(run) = runs.get_mut(active.as_str()) {
                run.request.body.kind = match kind {
                    1 => BodyKind::Json,
                    2 => BodyKind::Form,
                    3 => BodyKind::Raw,
                    _ => BodyKind::None,
                };
                run.dirty = true;
            }
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_body_formatted(move || {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            let text = http_ui.get_body_text().to_string();
            match devkit_tools::json::parser::parse(&text) {
                Ok(v) => {
                    let pretty = devkit_tools::json::format::pretty(&v, devkit_tools::json::Indent::Two);
                    http_ui.set_body_text(pretty.into());
                    let active = ctx.active.borrow().clone();
                    if let Some(run) = ctx.http_runs.borrow_mut().get_mut(active.as_str()) {
                        run.request.body.text = http_ui.get_body_text().to_string();
                        run.dirty = true;
                    }
                    set_status(&http_ui, "Body JSON 已格式化");
                }
                Err(e) => set_status(&http_ui, &format!("Body 不是合法 JSON：第 {} 行 {}", e.line, e.message)),
            }
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_auth_kind_changed(move |kind| {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            http_ui.set_auth_kind(kind);
            let active = ctx.active.borrow().clone();
            let mut runs = ctx.http_runs.borrow_mut();
            if let Some(run) = runs.get_mut(active.as_str()) {
                run.request.auth.kind = match kind {
                    1 => AuthKind::Bearer,
                    2 => AuthKind::Basic,
                    _ => AuthKind::None,
                };
                run.dirty = true;
            }
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_timeout_cycled(move || {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            let tiers = [15u32, 30, 60, 120, 300];
            let cur = http_ui.get_timeout_secs() as u32;
            let next = match tiers.iter().position(|&t| t >= cur) {
                Some(i) if i + 1 < tiers.len() => tiers[i + 1],
                _ => tiers[0],
            };
            http_ui.set_timeout_secs(next as i32);
            let active = ctx.active.borrow().clone();
            let mut runs = ctx.http_runs.borrow_mut();
            if let Some(run) = runs.get_mut(active.as_str()) {
                run.request.timeout_secs = next;
                run.dirty = true;
            }
            set_status(&http_ui, &format!("空闲超时：{next} 秒"));
        });
    }

    // ---- cURL / 清空 / 保存 ----
    {
        let ctx = ctx.clone();
        http_ui.on_copy_curl(move || {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            let active = ctx.active.borrow().clone();
            let runs = ctx.http_runs.borrow();
            let Some(run) = runs.get(active.as_str()) else { return };
            let mut req = run.request.clone();
            drop(runs);
            {
                let http_ui = ui.global::<HttpUI>();
                ui_to_http(&http_ui, &mut req);
            }
            let curl = to_curl(&req);
            match arboard::Clipboard::new().and_then(|mut c| c.set_text(curl)) {
                Ok(()) => set_status(&http_ui, "已复制为 cURL"),
                Err(e) => set_status(&http_ui, &format!("复制失败：{e}")),
            }
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_clear_request(move || {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            let active = ctx.active.borrow().clone();
            if let Some(run) = ctx.http_runs.borrow_mut().get_mut(active.as_str()) {
                *run = HttpRun { history_search: run.history_search.clone(), ..Default::default() };
            }
            sync_to_ui(&ctx, &ui);
            set_status(&http_ui, "请求已清空");
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_save(move || {
            let Some(ui) = crate::shell::current_window() else { return };
            let active = ctx.active.borrow().clone();
            let bound = ctx.http_runs.borrow().get(active.as_str())
                .map(|r| r.saved_id.is_some()).unwrap_or(false);
            if bound {
                if save_current(&ctx, &ui, None).is_ok() {
                    let http_ui = ui.global::<HttpUI>();
                    set_status(&http_ui, "已保存");
                }
            } else {
                open_save_dialog(&ctx);
            }
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_save_as_dialog(move || open_save_dialog(&ctx));
    }
    {
        let ctx = ctx.clone();
        http_ui.on_save_dialog_cancel(move || {
            let Some(ui) = crate::shell::current_window() else { return };
            ui.global::<HttpUI>().set_save_dialog_visible(false);
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_save_dialog_confirm(move || {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            let name = http_ui.get_save_name_input().to_string();
            if name.trim().is_empty() {
                set_status(&http_ui, "名称不能为空");
                return;
            }
            let combo_idx = http_ui.get_save_combo_index();
            let new_folder = http_ui.get_save_new_folder().to_string();
            match save_current_with_dialog(&ctx, &ui, &name, combo_idx as i64, &new_folder) {
                Ok(()) => {
                    http_ui.set_save_dialog_visible(false);
                    set_status(&http_ui, "已保存到集合");
                }
                Err(e) => set_status(&http_ui, &format!("保存失败：{e}")),
            }
        });
    }

    // ---- 历史 ----
    {
        let ctx = ctx.clone();
        http_ui.on_history_reload(move || reload_history(&ctx));
    }
    {
        let ctx = ctx.clone();
        http_ui.on_history_searched(move |kw| {
            let active = ctx.active.borrow().clone();
            if let Some(run) = ctx.http_runs.borrow_mut().get_mut(active.as_str()) {
                run.history_search = kw.to_string();
            }
            reload_history(&ctx);
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_history_selected(move |index| {
            load_history_row(&ctx, index as usize, false);
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_load_history(move |index| {
            load_history_row(&ctx, index as usize, false);
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_history_deleted(move |index| {
            delete_history_row(&ctx, index as usize);
        });
    }
    {
        let ctx = ctx.clone();
        http_ui.on_history_resent(move |index| {
            load_history_row(&ctx, index as usize, true);
        });
    }

    // ---- 集合树 ----
    {
        let ctx = ctx.clone();
        http_ui.on_tree_toggled(move |index| toggle_tree_node(&ctx, index as usize));
    }
    {
        let ctx = ctx.clone();
        http_ui.on_tree_selected(move |index| select_tree_node(&ctx, index as usize));
    }
    {
        let ctx = ctx.clone();
        http_ui.on_tree_deleted(move |index| delete_tree_node(&ctx, index as usize));
    }
    {
        let ctx = ctx.clone();
        http_ui.on_new_folder(move |parent| create_folder_at(&ctx, parent as i64));
    }

    // ---- 响应复制 ----
    {
        http_ui.on_copy_body(move || {
            let Some(ui) = crate::shell::current_window() else { return };
            let http_ui = ui.global::<HttpUI>();
            let body = http_ui.get_resp_body().to_string();
            match arboard::Clipboard::new().and_then(|mut c| c.set_text(body)) {
                Ok(()) => set_status(&http_ui, "已复制响应内容"),
                Err(e) => set_status(&http_ui, &format!("复制失败：{e}")),
            }
        });
    }

    // 保存对话框文件夹选项初始化
    refresh_save_folders(&ctx);
}

// ---------- 同步 ----------

/// 运行时请求 → UI 全局。
pub fn sync_to_ui(ctx: &Rc<Ctx>, ui: &MainWindow) {
    let http_ui = ui.global::<HttpUI>();
    let active = ctx.active.borrow().clone();
    let runs = ctx.http_runs.borrow();
    let Some(run) = runs.get(active.as_str()) else { return };
    http_ui.set_method(run.request.method.as_str().into());
    http_ui.set_url(run.request.url.as_str().into());
    http_ui.set_body_kind(body_kind_index(run.request.body.kind) as i32);
    http_ui.set_body_text(run.request.body.text.as_str().into());
    http_ui.set_auth_kind(auth_kind_index(run.request.auth.kind) as i32);
    http_ui.set_auth_token(run.request.auth.token.as_str().into());
    http_ui.set_auth_user(run.request.auth.username.as_str().into());
    http_ui.set_auth_pass(run.request.auth.password.as_str().into());
    http_ui.set_timeout_secs(run.request.timeout_secs as i32);
    http_ui.set_sending(run.sending);
    http_ui.set_saved_name(run.saved_name.as_str().into());
    http_ui.set_dirty(run.dirty);
    http_ui.set_resp_error("".into());
    drop(runs);
    sync_tables(ctx, &http_ui, &active);
    let runs = ctx.http_runs.borrow();
    if let Some(run) = runs.get(active.as_str()) {
        apply_response_view(&http_ui, run, http_ui.get_resp_tab() as usize);
    }
    rebuild_tree(ctx, ui);
}

fn sync_tables(ctx: &Rc<Ctx>, http_ui: &HttpUI, active: &str) {
    let runs = ctx.http_runs.borrow();
    let Some(run) = runs.get(active) else { return };
    let to_rows = |v: &[HTTPKV]| -> Vec<KvRow> {
        v.iter().map(|kv| KvRow {
            key: kv.key.clone().into(),
            value: kv.value.clone().into(),
            enabled: kv.enabled,
        }).collect()
    };
    http_ui.set_params(ModelRc::from(Rc::new(VecModel::from(to_rows(&run.request.params)))));
    http_ui.set_headers(ModelRc::from(Rc::new(VecModel::from(to_rows(&run.request.headers)))));
    http_ui.set_form(ModelRc::from(Rc::new(VecModel::from(to_rows(&run.request.body.form)))));
}

/// UI 全局 → 运行时请求。
fn ui_to_http(http_ui: &HttpUI, req: &mut HTTPRequestModel) {
    req.method = http_ui.get_method().to_string();
    req.url = http_ui.get_url().to_string();
    req.body.text = http_ui.get_body_text().to_string();
    req.auth.token = http_ui.get_auth_token().to_string();
    req.auth.username = http_ui.get_auth_user().to_string();
    req.auth.password = http_ui.get_auth_pass().to_string();
    req.timeout_secs = http_ui.get_timeout_secs().clamp(1, 3600) as u32;
}

fn body_kind_index(k: BodyKind) -> u8 {
    match k { BodyKind::None => 0, BodyKind::Json => 1, BodyKind::Form => 2, BodyKind::Raw => 3 }
}

fn auth_kind_index(k: AuthKind) -> u8 {
    match k { AuthKind::None => 0, AuthKind::Bearer => 1, AuthKind::Basic => 2 }
}

// ---------- 表格编辑 ----------

fn edit_kv(ctx: &Rc<Ctx>, table: usize, index: usize, field: usize, value: &str) {
    let Some(ui) = crate::shell::current_window() else { return };
    let active = ctx.active.borrow().clone();
    let mut runs = ctx.http_runs.borrow_mut();
    let Some(run) = runs.get_mut(active.as_str()) else { return };
    let list = match table {
        0 => &mut run.request.params,
        1 => &mut run.request.headers,
        _ => &mut run.request.body.form,
    };
    if let Some(kv) = list.get_mut(index) {
        match field {
            0 => kv.key = value.to_string(),
            _ => kv.value = value.to_string(),
        }
        run.dirty = true;
    }
}

fn edit_kv_bool(ctx: &Rc<Ctx>, table: usize, index: usize, f: impl FnOnce(&mut HTTPKV)) {
    let Some(ui) = crate::shell::current_window() else { return };
    let active = ctx.active.borrow().clone();
    let mut runs = ctx.http_runs.borrow_mut();
    let Some(run) = runs.get_mut(active.as_str()) else { return };
    let list = match table {
        0 => &mut run.request.params,
        1 => &mut run.request.headers,
        _ => &mut run.request.body.form,
    };
    if let Some(kv) = list.get_mut(index) {
        f(kv);
        run.dirty = true;
    }
    drop(runs);
    sync_tables(ctx, &ui.global::<HttpUI>(), &active);
}

// ---------- 发送 ----------

fn poll_send_result(ctx: &Rc<Ctx>, tab_id: &str) {
    let Some(ui) = crate::shell::current_window() else { return };
    let result = {
        let mut runs = ctx.http_runs.borrow_mut();
        let Some(run) = runs.get_mut(tab_id) else { return };
        let Some(rx) = &run.rx else { return };
        let result = rx.try_recv().ok();
        if result.is_some() {
            run.sending = false;
            run.rx = None;
            run.poll = None;
        }
        result
    };
    let Some(result) = result else { return };
    let mut response = None;
    let error_text;
    let mut request_for_history = None;
    let mut saved_id_for_history = None;
    match result {
        Ok(resp) => {
            error_text = String::new();
            response = Some(resp);
        }
        Err(SendError::Cancelled) => {
            error_text = "请求已取消".into();
        }
        Err(e) => {
            error_text = format!("{e}");
        }
    }
    // 写回运行时
    {
        let mut runs = ctx.http_runs.borrow_mut();
        let Some(run) = runs.get_mut(tab_id) else { return };
        run.response = response.clone();
        run.dirty = true;
        request_for_history = Some(run.request.clone());
        saved_id_for_history = run.saved_id.clone();
    }
    // 落历史（有响应或主动取消都落）
    if let Some(db) = ctx.db() {
        if let Some(req) = request_for_history {
            let _ = hstore::insert_history(&db, &req, response.as_ref(), saved_id_for_history.as_deref());
        }
    }
    // 更新 UI（仅当该标签仍激活）
    let active = ctx.active.borrow().clone();
    if active == tab_id {
        let http_ui = ui.global::<HttpUI>();
        http_ui.set_sending(false);
        let runs = ctx.http_runs.borrow();
        if let Some(run) = runs.get(tab_id) {
            if error_text.is_empty() {
                http_ui.set_resp_error("".into());
                apply_response_view(&http_ui, run, http_ui.get_resp_tab() as usize);
            } else {
                http_ui.set_resp_error(error_text.clone().into());
            }
        }
        http_ui.set_dirty(true);
    }
}

fn apply_response_view(http_ui: &HttpUI, run: &HttpRun, tab: usize) {
    let Some(resp) = &run.response else {
        http_ui.set_resp_status_line("尚无响应".into());
        http_ui.set_resp_body("".into());
        http_ui.set_resp_headers_text("".into());
        http_ui.set_resp_ok(false);
        http_ui.set_resp_truncated(false);
        http_ui.set_resp_image_ok(false);
        return;
    };
    let ok = resp.status_code < 400;
    http_ui.set_resp_ok(ok);
    http_ui.set_resp_truncated(resp.truncated);
    let size_text = if resp.size_bytes >= 1024 * 1024 {
        format!("{:.1} MB", resp.size_bytes as f64 / 1048576.0)
    } else if resp.size_bytes >= 1024 {
        format!("{:.1} KB", resp.size_bytes as f64 / 1024.0)
    } else {
        format!("{} B", resp.size_bytes)
    };
    http_ui.set_resp_status_line(
        format!("{} {} · {} ms · {} · {}", resp.status_code, resp.status_text, resp.duration_ms, size_text, resp.content_type).into(),
    );
    http_ui.set_resp_headers_text(
        resp.headers.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n").into(),
    );
    // 图片预览（image/* 且非截断）
    let show_image = tab == 3
        && resp.content_type.starts_with("image/")
        && !resp.truncated
        && !resp.body_raw.is_empty();
    if show_image {
        match image::load_from_memory(&resp.body_raw) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = (rgba.width(), rgba.height());
                let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    rgba.as_raw(),
                    w,
                    h,
                );
                http_ui.set_resp_image(slint::Image::from_rgba8(buffer));
                http_ui.set_resp_image_ok(true);
            }
            Err(_) => http_ui.set_resp_image_ok(false),
        }
    } else {
        http_ui.set_resp_image_ok(false);
    }
    let body_text = match tab {
        0 => {
            // Pretty：JSON 美化，失败则原样
            match devkit_tools::json::parser::parse(&resp.body) {
                Ok(v) => devkit_tools::json::format::pretty(&v, devkit_tools::json::Indent::Two),
                Err(_) => resp.body.clone(),
            }
        }
        _ => resp.body.clone(),
    };
    http_ui.set_resp_body(body_text.into());
}

// ---------- 历史 ----------

pub fn reload_history(ctx: &Rc<Ctx>) {
    let Some(ui) = crate::shell::current_window() else { return };
    let Some(db) = ctx.db() else { return };
    let active = ctx.active.borrow().clone();
    let keyword = ctx.http_runs.borrow().get(active.as_str()).map(|r| r.history_search.clone()).unwrap_or_default();
    let items: Vec<HttpHistoryRow> = if keyword.trim().is_empty() {
        hstore::list_history(&db, 300)
    } else {
        hstore::search_history(&db, keyword.trim(), 300)
    }
    .unwrap_or_default()
    .into_iter()
    .map(|h| HttpHistoryRow {
        id: h.id.into(),
        method: h.method.into(),
        url: h.url.into(),
        time_text: format_time(h.created_at).into(),
        status_code: h.status_code.unwrap_or(-1) as i32,
        duration_text: h.duration_ms.map(|d| format!("{d} ms")).unwrap_or_default().into(),
    })
    .collect();
    let http_ui = ui.global::<HttpUI>();
    http_ui.set_history(ModelRc::from(Rc::new(VecModel::from(items))));
}

pub fn format_time(unix: f64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let dt = Local.timestamp_opt(unix as i64, 0).single();
    let Some(dt) = dt else { return String::new() };
    let now = Local::now();
    if dt.date_naive() == now.date_naive() {
        dt.format("%H:%M").to_string()
    } else if dt.year() == now.year() {
        dt.format("%m-%d %H:%M").to_string()
    } else {
        dt.format("%Y-%m-%d %H:%M").to_string()
    }
}

fn history_id_at(ctx: &Rc<Ctx>, index: usize) -> Option<String> {
    let Some(ui) = crate::shell::current_window() else { return None };
    let http_ui = ui.global::<HttpUI>();
    let model = http_ui.get_history();
    model.row_data(index).map(|r| r.id.to_string())
}

fn load_history_row(ctx: &Rc<Ctx>, index: usize, resend: bool) {
    let Some(id) = history_id_at(ctx, index) else { return };
    let Some(db) = ctx.db() else { return };
    let Ok(Some((request, response))) = hstore::get_history(&db, &id) else { return };
    let active = ctx.active.borrow().clone();
    let has_resp = response.is_some();
    if let Some(run) = ctx.http_runs.borrow_mut().get_mut(active.as_str()) {
        run.request = request;
        run.response = response;
        run.dirty = true;
        run.saved_id = None;
        run.saved_name = String::new();
    }
    let Some(ui) = crate::shell::current_window() else { return };
    sync_to_ui(&ctx, &ui);
    if resend && has_resp {
        // 重发：直接触发发送
        let http_ui = ui.global::<HttpUI>();
        http_ui.set_editor_tab(0);
        http_ui.invoke_send();
    } else {
        let http_ui = ui.global::<HttpUI>();
        set_status(&http_ui, "已载入历史记录");
    }
}

fn delete_history_row(ctx: &Rc<Ctx>, index: usize) {
    let Some(id) = history_id_at(ctx, index) else { return };
    let Some(db) = ctx.db() else { return };
    let _ = hstore::delete_history(&db, &id);
    reload_history(ctx);
}

// ---------- 集合树 ----------

pub fn rebuild_tree(ctx: &Rc<Ctx>, ui: &MainWindow) {
    let Some(db) = ctx.db() else { return };
    let active = ctx.active.borrow().clone();
    let (expanded, active_saved) = {
        let runs = ctx.http_runs.borrow();
        let run = runs.get(active.as_str());
        (
            run.map(|r| r.tree_expanded.clone()).unwrap_or_default(),
            run.and_then(|r| r.saved_id.clone()),
        )
    };
    let folders = hstore::list_folders(&db).unwrap_or_default();

    let mut nodes: Vec<HttpTreeNode> = Vec::new();
    walk_folders(&db, &folders, None, 0, &expanded, active_saved.as_deref(), &mut nodes);
    let http_ui = ui.global::<HttpUI>();
    http_ui.set_tree(ModelRc::from(Rc::new(VecModel::from(nodes))));
}

fn walk_folders(
    db: &devkit_core::db::Database,
    folders: &[hstore::FolderRow],
    folder: Option<&str>,
    depth: i32,
    expanded: &HashSet<String>,
    active_saved: Option<&str>,
    nodes: &mut Vec<HttpTreeNode>,
) {
    for f in folders.iter().filter(|f| f.parent_id.as_deref() == folder) {
        let has_children = folders.iter().any(|c| c.parent_id.as_deref() == Some(f.id.as_str()))
            || hstore::list_requests(db, Some(&f.id)).map(|r| !r.is_empty()).unwrap_or(false);
        let is_open = expanded.contains(&f.id);
        nodes.push(HttpTreeNode {
            id: f.id.clone().into(),
            name: f.name.clone().into(),
            is_folder: true,
            depth,
            expanded: is_open && has_children,
            has_children,
            bound: false,
        });
        if is_open {
            walk_folders(db, folders, Some(&f.id), depth + 1, expanded, active_saved, nodes);
        }
    }
    if let Ok(reqs) = hstore::list_requests(db, folder) {
        for r in reqs {
            nodes.push(HttpTreeNode {
                id: r.id.clone().into(),
                name: r.name.clone().into(),
                is_folder: false,
                depth,
                expanded: false,
                has_children: false,
                bound: active_saved == Some(r.id.as_str()),
            });
        }
    }
}

fn tree_id_at(ctx: &Rc<Ctx>, index: usize) -> Option<(String, bool)> {
    let Some(ui) = crate::shell::current_window() else { return None };
    let http_ui = ui.global::<HttpUI>();
    http_ui.get_tree().row_data(index).map(|n| (n.id.to_string(), n.is_folder))
}

fn toggle_tree_node(ctx: &Rc<Ctx>, index: usize) {
    let Some((id, is_folder)) = tree_id_at(ctx, index) else { return };
    if !is_folder { return; }
    let active = ctx.active.borrow().clone();
    if let Some(run) = ctx.http_runs.borrow_mut().get_mut(active.as_str()) {
        if run.tree_expanded.contains(&id) {
            run.tree_expanded.remove(&id);
        } else {
            run.tree_expanded.insert(id);
        }
    }
    let Some(ui) = crate::shell::current_window() else { return };
    rebuild_tree(ctx, &ui);
}

fn select_tree_node(ctx: &Rc<Ctx>, index: usize) {
    let Some((id, is_folder)) = tree_id_at(ctx, index) else { return };
    if is_folder { return; }
    let Some(db) = ctx.db() else { return };
    let Ok(Some(row)) = hstore::get_request(&db, &id) else { return };
    let active = ctx.active.borrow().clone();
    if let Some(run) = ctx.http_runs.borrow_mut().get_mut(active.as_str()) {
        run.request = row.request;
        run.saved_id = Some(row.id);
        run.saved_name = row.name;
        run.dirty = false;
        run.response = None;
    }
    let Some(ui) = crate::shell::current_window() else { return };
    sync_to_ui(&ctx, &ui);
}

fn delete_tree_node(ctx: &Rc<Ctx>, index: usize) {
    let Some((id, is_folder)) = tree_id_at(ctx, index) else { return };
    let Some(db) = ctx.db() else { return };
    let result = if is_folder {
        hstore::delete_folder_cascade(&db, &id)
    } else {
        hstore::delete_request(&db, &id)
    };
    if result.is_ok() {
        // 解绑当前编辑器（若绑定了被删请求）
        let active = ctx.active.borrow().clone();
        let mut runs = ctx.http_runs.borrow_mut();
        if let Some(run) = runs.get_mut(active.as_str()) {
            if run.saved_id.as_deref() == Some(id.as_str()) {
                run.saved_id = None;
                run.saved_name = String::new();
            }
            run.tree_expanded.remove(&id);
        }
        drop(runs);
        let Some(ui) = crate::shell::current_window() else { return };
        let http_ui = ui.global::<HttpUI>();
        sync_to_ui(&ctx, &ui);
        set_status(&http_ui, "已删除");
    }
}

fn create_folder_at(ctx: &Rc<Ctx>, parent_index: i64) {
    let Some(db) = ctx.db() else { return };
    let parent: Option<String> = if parent_index < 0 {
        None
    } else {
        match tree_id_at(ctx, parent_index as usize) {
            Some((id, true)) => Some(id),
            Some((fid, false)) => {
                // 请求上新建 → 建在其所在文件夹
                hstore::get_request(&db, &fid).ok().flatten().and_then(|r| r.folder_id)
            }
            None => None,
        }
    };
    let _ = hstore::create_folder(&db, parent.as_deref(), "新建文件夹");
    let Some(ui) = crate::shell::current_window() else { return };
    rebuild_tree(ctx, &ui);
    refresh_save_folders(&ctx);
}

// ---------- 保存 ----------

fn open_save_dialog(ctx: &Rc<Ctx>) {
    let Some(ui) = crate::shell::current_window() else { return };
    let http_ui = ui.global::<HttpUI>();
    let active = ctx.active.borrow().clone();
    let name = ctx.http_runs.borrow().get(active.as_str())
        .map(|r| {
            if r.saved_name.is_empty() {
                // 用 host 兜底命名
                r.request.url.split('/').nth(2).unwrap_or("未命名请求").to_string()
            } else {
                r.saved_name.clone()
            }
        })
        .unwrap_or_default();
    http_ui.set_save_name_input(name.into());
    http_ui.set_save_new_folder("".into());
    refresh_save_folders(&ctx);
    http_ui.set_save_dialog_visible(true);
}

fn refresh_save_folders(ctx: &Rc<Ctx>) {
    let Some(ui) = crate::shell::current_window() else { return };
    let Some(db) = ctx.db() else { return };
    let folders = hstore::list_folders(&db).unwrap_or_default();
    // 构造显示路径
    let mut options = vec!["（根目录）".to_string()];
    let by_id: std::collections::HashMap<&str, &hstore::FolderRow> =
        folders.iter().map(|f| (f.id.as_str(), f)).collect();
    let mut path_of = |id: &str| -> String {
        let mut parts = Vec::new();
        let mut cur = Some(id.to_string());
        let mut hops = 0;
        while let Some(c) = cur {
            if let Some(f) = by_id.get(c.as_str()) {
                parts.push(f.name.clone());
                cur = f.parent_id.clone();
            } else {
                break;
            }
            hops += 1;
            if hops > 32 { break; }
        }
        parts.reverse();
        parts.join(" / ")
    };
    for f in &folders {
        options.push(path_of(&f.id));
    }
    let http_ui = ui.global::<HttpUI>();
    http_ui.set_save_folder_options(ModelRc::from(Rc::new(VecModel::from(
        options.into_iter().map(slint::SharedString::from).collect::<Vec<_>>(),
    ))));
    http_ui.set_save_combo_index(0);
}

fn save_current(ctx: &Rc<Ctx>, ui: &MainWindow, folder_override: Option<Option<String>>) -> Result<()> {
    let Some(db) = ctx.db() else { anyhow::bail!("数据库未打开") };
    let http_ui = ui.global::<HttpUI>();
    let active = ctx.active.borrow().clone();
    let (id, name, folder) = {
        let mut runs = ctx.http_runs.borrow_mut();
        let run = runs.get_mut(active.as_str()).ok_or_else(|| anyhow::anyhow!("无激活标签"))?;
        ui_to_http(&http_ui, &mut run.request);
        let id = run.saved_id.clone();
        let name = if name_is_empty(&run.saved_name) { "未命名请求".to_string() } else { run.saved_name.clone() };
        let folder = match folder_override {
            Some(f) => f,
            None => hstore::get_request(&db, id.as_deref().unwrap_or("")).ok().flatten().and_then(|r| r.folder_id),
        };
        (id, name, folder)
    };
    let new_id = hstore::upsert_request(&db, id.as_deref(), folder.as_deref(), &name, &ctx_request(ctx, &active)?)?;
    let mut runs = ctx.http_runs.borrow_mut();
    if let Some(run) = runs.get_mut(active.as_str()) {
        run.saved_id = Some(new_id);
        run.saved_name = name;
        run.dirty = false;
    }
    drop(runs);
    http_ui.set_saved_name(ctx.http_runs.borrow().get(active.as_str()).map(|r| r.saved_name.clone()).unwrap_or_default().into());
    http_ui.set_dirty(false);
    rebuild_tree(ctx, &ui);
    Ok(())
}

fn name_is_empty(s: &str) -> bool { s.trim().is_empty() }

fn ctx_request(ctx: &Rc<Ctx>, active: &str) -> Result<HTTPRequestModel> {
    let runs = ctx.http_runs.borrow();
    runs.get(active).map(|r| r.request.clone()).ok_or_else(|| anyhow::anyhow!("无激活标签"))
}

fn save_current_with_dialog(ctx: &Rc<Ctx>, ui: &MainWindow, name: &str, folder_idx: i64, new_folder: &str) -> Result<()> {
    let Some(db) = ctx.db() else { anyhow::bail!("数据库未打开") };
    // 新建文件夹（支持 a/b/c 多级）
    let mut folder_id: Option<String> = if folder_idx <= 0 {
        None
    } else {
        let folders = hstore::list_folders(&db)?;
        folders.get((folder_idx - 1) as usize).map(|f| f.id.clone())
    };
    if !new_folder.trim().is_empty() {
        for part in new_folder.split('/') {
            let part = part.trim();
            if part.is_empty() { continue; }
            let new_id = hstore::create_folder(&db, folder_id.as_deref(), part)?;
            folder_id = Some(new_id);
        }
    }
    let http_ui = ui.global::<HttpUI>();
    let active = ctx.active.borrow().clone();
    let req = ctx_request(ctx, &active)?;
    let id = ctx.http_runs.borrow().get(active.as_str()).and_then(|r| r.saved_id.clone());
    let new_id = hstore::upsert_request(&db, id.as_deref(), folder_id.as_deref(), name, &req)?;
    if let Some(run) = ctx.http_runs.borrow_mut().get_mut(active.as_str()) {
        run.saved_id = Some(new_id);
        run.saved_name = name.to_string();
        run.dirty = false;
    }
    http_ui.set_saved_name(name.into());
    http_ui.set_dirty(false);
    rebuild_tree(ctx, &ui);
    Ok(())
}

fn set_status(http_ui: &HttpUI, msg: &str) {
    http_ui.set_status(msg.into());
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}
