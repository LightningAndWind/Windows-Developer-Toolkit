# devkit Windows 端移植计划（Rust + Slint）

> **重构说明（2026-10）**：本文件保留最初的移植计划作为历史记录。
> 实际实现已在「Windows 原生化」重构中调整要点：
> - 样式不再复刻 macOS 外观，改用 Slint 内置 **Fluent（Windows 11 原生）风格**，
>   自绘控件只从 `Palette` 取色，深浅色与系统强调色自动跟随；
> - 界面用原生 `TabWidget` / `ComboBox` / `CheckBox` / `ScrollView` 承载分区与表单，
>   工具条统一 `cross-axis-alignment: center` 垂直居中、末尾放 stretch 占位避免按钮被拉伸；
> - Git 的所有子进程改到后台线程执行，UI 不再被 `pull` / `push` 卡住；
> - 补齐了 macOS 端已有但 Windows 端缺失的功能（HTTP 集合树、Git 仓库登记 / 克隆 /
>   密钥管理 / 远程地址编辑 / 历史分页、JSON 复制子树、终端内全局快捷键等）；
> - 新增 `devkit.exe --screenshot` 无头截图，便于在没有桌面会话的环境里检查排版。
>
> 当前实现说明以 `README.md` 为准。

## Context

工作区已有完整的 macOS SwiftUI 开发者工具箱（`Macos-Developer-Toolkit/devkit/devkit/`），含 HTTP / SSH / SFTP / Git / JSON 五大工具 + 完整 Shell（标签页、分组、Launcher、设置、会话快照、防休眠、外观三态）。用户要求仿照该应用，用 Rust + WinUI/Slint 在 Windows 上完整实现同等功能。

**技术选型决策**：选 **Slint** 而非 WinUI 3——WinUI 3 没有成熟的 Rust 支持（XAML 互操作需 C++ 脚手架），而 Slint 是纯 Rust 声明式 UI 框架，语法理念接近 SwiftUI，Windows 支持成熟。

## 工程结构

新项目 `d:\code\devkit\devkit-windows\`，Cargo workspace 三 crate（对应 Mac 端 Core / Tools / Shell 分层）：

```
devkit-windows/
├─ Cargo.toml                 # workspace
└─ crates/
   ├─ core/                   # 无 UI 依赖：db.rs(10表DDL)、prefs.rs、sleep.rs(防休眠)、
   │                          # appearance.rs(注册表监听深浅色)、session.rs(快照serde)、tool.rs(Tool trait)
   ├─ tools/                  # 五工具纯逻辑：http/ ssh/ sftp/ git/ json/ 各自 client+model+store
   └─ app/                    # Slint UI + 胶水层
      ├─ ui/ MainWindow.slint + theme/globals + widgets/（TabSidebar、Launcher、SettingsPanel、
      │      Table、TreePanel、TerminalView、各工具视图）
      └─ src/ main.rs、shell/(tabmanager, commands)、glue/(http,ssh,sftp,git,json)
```

## 依赖选型

| 用途 | Crate | 说明 |
|---|---|---|
| UI | slint 1.x | renderer 用 femtovg（Windows 默认，兼容性好）；软件渲染兜底 |
| SQLite | rusqlite (bundled) | 复刻 Mac 端 10 表 DDL 原样 |
| 异步 | tokio (full) | 全局运行时 |
| HTTP | reqwest 0.12 | cookies + stream + rustls；SSE 用 bytes_stream |
| SSH/SFTP | russh + russh-sftp | 纯 Rust 进程内双引擎；保留系统 OpenSSH 子进程 fallback（对应 Mac 双引擎设计） |
| 终端 | portable-pty + vte | PTY + ANSI 解析 → 自绘 Slint 网格终端 |
| 目录选择/打开 | rfd + open | 替代 NSOpenPanel / Finder reveal |
| 剪贴板 | arboard | 复制内容 |
| 防休眠 | windows crate | SetThreadExecutionState（对应 Mac SleepKeeper 双断言） |
| 深浅色 | winreg | 监听 AppsUseLightTheme |
| 序列化 | serde / serde_json / uuid / chrono | 会话快照 + profile_json |

## 关键设计

### Tool trait（对齐 ToolProtocol.swift）

```rust
pub trait Tool {
    fn descriptor(&self) -> &'static Descriptor;  // id/title/category/subtitle/multi_instance
    fn view_kind(&self) -> ViewKind;              // UI 路由分支
    fn dynamic_title(&self) -> Option<String>;
    fn has_unsaved(&self) -> bool { false }
    fn save_via_shell(&mut self) -> Result<(), String>;
    fn teardown(&mut self);                       // 关标签：杀 shell/abort task
    fn session_state(&self) -> Option<serde_json::Value> { None }
    fn restore(&mut self, v: &serde_json::Value);
}
```

Registry 用 **枚举** `enum ToolInstance { Http(..), Ssh(..), ... }` 而非 dyn trait object：Slint 胶水直接 match，快照 serde 用 tagged enum 天然支持。

### Slint UI 架构

- **全局单例**：Theme（三态调色板）、AppState（tabs VecModel、active_id、close/reorder/new_tab 回调）、Toast。
- **标签常驻**：工具视图全部挂组件树用 `visible:` 切换（非 `if`），避免重建丢状态；快捷键 ⌘→Ctrl 映射（Ctrl+T / 1-9 / W / S / E / Ctrl+↑↓ / Ctrl+,）。
- **表格/树**：一律 ListView + 虚拟化；树用扁平化可见节点 Vec<FlatNode{depth,expanded}> 模式。
- **终端渲染（最高风险，M0 先做 spike）**：TerminalGrid{cells, scrollback 3000 行环形} ← vte::Parser 标脏行 → 30ms 合帧只推脏行 → Slint 内按同色合并 span 渲染；宽字符占 2 cell；Plan B：软件光栅化到 SharedPixelBuffer 整帧 blit。

### 数据层

SQLite DDL 从 `Core/Data/DatabaseManager.swift` 原样移植（kv、http_history、http_folders、http_requests、ssh_folders、ssh_profiles、ssh_known_hosts、git_folders、git_repos、git_keys）；会话快照结构对齐 `SessionSnapshot.swift`（TabSnapshot/TabGroupSnapshot/WindowSessionSnapshot），存 kv 表 JSON。

## 实现里程碑（每步可编译可验证）

| M | 内容 | 验收标准 |
|---|---|---|
| 0 | workspace 骨架 + Slint 壳（侧栏拖拽 180-320、Launcher 卡片、亮暗三态、防休眠）+ **终端渲染 spike** | cargo run 正常；spike 1MB/s 输出不卡 |
| 1 | 数据层 10 表 + kv 快照 + 首启数据目录引导 + 重启还原标签 | 重开还原标签结构 |
| 2 | JSON 工具（serde_json + 自写校验层实现行列/尾逗号/单引号诊断、格式化/压缩/排序/树/统计/双视图） | 错误行列精确 |
| 3 | HTTP 工具（表格启用禁用、Body 四类型、Auth、超时档位、响应四视图 10MB 封顶、取消、历史、集合树、cURL 导出、SSE） | 取消生效、大响应不掉帧 |
| 4 | SSH/终端（portable-pty 本地终端 → russh 密码/密钥进程内，OpenSSH 子进程 fallback；TOFU known_hosts、连接横幅、双调色板、关标签杀 shell） | vim/htop 正常、断线重连 |
| 5 | SFTP（russh-sftp + 本地 backend、双面板多选、<500KB 4 并发流水线、HUD 进度/取消） | 本地↔远端互传成功 |
| 6 | Git 工具（系统 git 子进程、四组变更、diff 着色、冲突解决、分支/Stash/历史分页、ssh-keygen、GIT_SSH_COMMAND 注入、克隆） | 全流程跑通 |
| 7 | 打磨（设置四分区面板、快捷键全集、会话快照含工具内部状态、Launcher 搜索） | 断电重启全还原 |

## 风险与对策

1. **终端渲染性能** — M0 spike 前置化解；Plan B 图片 blit。
2. **Slint 大数据表格** — 强制 ListView 懒加载，历史分页。
3. **russh 老服务器算法兼容** — 双引擎兜底（Win10+ 自带 OpenSSH）。
4. **系统 git 缺失** — 启动检测 PATH 并引导。
5. **Windows IME/中文输入** — 实测终端中文输入路径。
6. **femtovg 需 GL** — 软件渲染环境变量兜底。

## 验证方式

每里程碑 `cargo build && cargo run` 启动应用手工验收对应功能；终端 spike 用 `yes | head -c 1M` 类高压输出测帧率；数据层用重开应用验证快照还原；HTTP 用本地 mock 接口验证取消/超时/10MB 封顶。
