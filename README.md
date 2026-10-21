# Windows Developer Toolkit

对标 [Macos-Developer-Toolkit](https://github.com/LightningAndWind/Macos-Developer-Toolkit) 的 Windows 端实现：面向 Windows 开发者的本地化工具集，用标签页方式把 HTTP / SSH / SFTP / Git / JSON 五个高频工具聚合到一个 App 内，追求**精简、快速、低内存占用**。

移植计划与技术决策见 [devkit-windows-rust-slint-plan.md](./devkit-windows-rust-slint-plan.md)。

## 技术栈

- **UI**：Slint 1.18（声明式，femtovg 渲染，软件渲染兜底）；**样式直接采用 Slint 内置的 Fluent（Windows 11 原生）风格**
- **语言 / 异步**：Rust + tokio
- **HTTP**：reqwest 0.12（cookie 存储 + stream，支持 SSE 流式）
- **终端**：系统 OpenSSH 子进程 + ConPTY（portable-pty + vte 自绘终端网格，30ms 合帧 + 3000 行回滚）
- **SFTP**：russh + russh-sftp（进程内，密码 / 私钥认证，1MB 分块传输）
- **数据存储**：SQLite（rusqlite bundled，10 表：kv / http_history / http_folders / http_requests / ssh_folders / ssh_profiles / ssh_known_hosts / git_folders / git_repos / git_keys）
- **系统集成**：windows crate（SetThreadExecutionState 防休眠双断言）、winreg（读取 `AppsUseLightTheme`）
- **工程**：Cargo workspace 三 crate —— `core`（无 UI 基础层）/ `tools`（五工具纯逻辑）/ `app`（Slint UI + 胶水）

## 界面设计

**原则：不复刻 macOS 外观，直接用 Windows 原生观感；界面尽量简单、不能错位。**

- **原生外观**：所有标准控件（`LineEdit` / `TextEdit` / `ComboBox` / `CheckBox` / `TabWidget` / `ScrollView` / `Popup`）都是 Slint 内置 Fluent 风格，跟随系统的深浅色与**强调色**（`SlintInternal.accent-color`，用户改 Windows 主题色时应用自动跟随）。自绘部分（侧栏、工具条、卡片）只从 `Palette` 派生颜色，不再自造一套配色
- **设计令牌**：`ui/globals/Theme.slint` 只做「语义命名」（`bg` / `surface` / `content-bg` / `text` / `accent` / `danger` …）与度量（间距 / 圆角 / 字号），颜色一律来自 `Palette`
- **通用控件**：`ui/components/Controls.slint` 提供 `AppButton`（默认 / 主操作 / 危险 / 无边框）、`AppIconButton`、`Label`、`StatusPill`、`EmptyState`、`TreeRow` 等；所有控件显式高度，避免在布局中被拉伸成色块
- **图标**：统一使用 Segoe MDL2 Assets 字形（Windows 系统字体，不额外打包矢量图）
- **字号基准**：`fs-caption/ small/ body/ title = 12/13/14/16px`，正文下限锁 12px
- **布局约束（错位的根因与对策）**
  - `HorizontalLayout.alignment` 是**主轴**（水平）对齐，`cross-axis-alignment` 才是垂直对齐；横排一律用 `cross-axis-alignment: center` 做垂直居中，用默认的 `start` 保持左对齐
  - 同一行里如果没有任何控件带 `horizontal-stretch`，Slint 会把剩余空间**平分**给它们；工具条末尾统一放一个 `Rectangle { horizontal-stretch: 1; }` 占位，按钮才会保持原尺寸
  - 弹窗尺寸统一写成 `min(root.width - 48px, 700px)`，小窗口下不会算出负坐标
  - 滚动区必须显式给高度（`vertical-stretch: 1` 或 `content-height`），否则内容会被裁掉且滚不动
- **无头截图**：`devkit.exe --screenshot out.png` 用软件渲染器离屏渲染一张界面图（不需要桌面会话），配合 `DEVKIT_SHOT_TOOL=http|ssh|sftp|git|json|launcher|settings|dark` 切换要查看的界面、`DEVKIT_SHOT_HTTP_TAB=0..5` 切 HTTP 分区、`DEVKIT_SHOT_REPO=<路径>` 打开一个 Git 仓库，便于在 CI / 远程 shell 里检查排版
  - 截图模式跑的是**真实事件循环**（定时器 / 回调都会触发），因此可以验证交互行为而不只是静态首帧
  - 回归测试用的开关：`DEVKIT_SHOT_HTTP_AUTH=0,1,2`（认证方式）、`DEVKIT_SHOT_HTTP_BODY=0..3`（Body 类型）、`DEVKIT_SHOT_HTTP_TIMEOUT=1`（超时档位）、`DEVKIT_SHOT_SSH_LOCAL=1`（打开本地终端并执行一条多行输出命令，用来验证终端自动贴底）
  - `DEVKIT_SHOT_MS=<毫秒>` 调整截图模式的运行时长（默认 2500）
  - 截图模式**不写会话快照**，不会动用户已保存的标签集合

## 应用外壳

- **首启引导**：强制选择数据目录（校验可写）后才进入主界面
- **标签页管理**：一个标签 = 一个工具实例，多实例并行、分组、自定义标题、双击重命名、拖拽排序；视图常驻不销毁（`visible:` 切换，避免重建丢状态）；关闭标签触发 `teardown` 杀 shell / 中止任务
- **Launcher**：工具卡片分类展示，顶部搜索框过滤
- **会话快照**：重启自动还原标签结构及各工具内部状态（存 kv 表 JSON）
- **外观三态**：跟随系统 / 浅色 / 深色。跟随系统时读注册表 `AppsUseLightTheme`（1 秒轮询），并把结果同步到 Slint 的 `Palette.color-scheme`，标准控件与自绘控件同时切换
- **界面缩放**：跟随系统 / 110% / 125% / 150%，重启生效。Slint 无数运行期缩放 API，启动前用 `SLINT_SCALE_FACTOR = 系统 DPI 比例 × 用户系数` 覆盖；窗口若超出工作区，启动后按工作区 92% 收缩并居中
- **防休眠**：侧栏咖啡杯开关，同时持有防屏保活与防系统休眠两条断言，状态持久化
- **设置面板**：通用 / HTTP 历史 / 终端会话 / Git 仓库 四分区（原生 `TabWidget`）
- **快捷键**：Ctrl+T 新标签、Ctrl+1~9/0 直达标签、Ctrl+↓/↑ 切换上/下一个标签（环绕）、Ctrl+W 关闭、Ctrl+E 重命名、Ctrl+S 保存请求（HTTP 工具）、Ctrl+, 设置
  - 快捷键绑定在窗口根部的 `FocusScope` 上，终端等输入控件会 `reject` 掉带 Ctrl 的按键，事件沿父链冒泡回窗口，因此**终端里也能用全局快捷键**

## 内置工具

### HTTP 请求

请求栏一行完成「方法 / 地址 / 发送 / 保存」；编辑区用原生 `TabWidget` 分成 **参数 / 请求头 / Body / 认证 / 历史 / 集合** 六个分区：

- **参数**：键值表（逐行启用开关 + 删除）、从 URL 解析查询参数
- **请求头**：键值表
- **Body**：无 / JSON / Form / Raw 四种，JSON 可一键格式化
- **认证**：无 / Bearer Token / Basic Auth（密码用密码框），底部可调空闲超时（15/30/60/120/300 秒）
- **历史**：SQLite 落库，可搜索、重发、删除
- **集合**：多级文件夹 + 已保存请求的树，可新建文件夹、重命名、删除、打开

响应区分 **美化 / 原始 / 响应头 / 预览** 四视图；10 MB 响应封顶（超限中止并提示）；发送中可随时停止；一键复制为 cURL。

### SSH 终端

本地终端 + SSH 远程会话统一走系统 OpenSSH + ConPTY，密码 / 口令 / 主机信任提示通过 PTY 自动应答；TOFU known_hosts；关标签杀 shell 防孤儿进程。

**会话记录**：本地 / 远程两类记录，支持文件夹分组、编辑（主机 / 端口 / 用户名 / 认证方式 / 私钥 / 默认目录）、删除；记录编辑器可「保存并连接」。

**终端渲染**：每个输出段由 Rust 侧给出起始列与覆盖单元格数，UI 按 `char-width = font-size × 0.55`（Consolas 实测步进）绝对定位，等宽表输出、中文宽字符、光标反色均与网格逐像素对齐；`select` 输出与 `Get-ChildItem` 表格已按列对齐验证。终端配色跟随应用深浅色（深色 / 浅色两套 ANSI 调色板）。

**连接状态**：横幅区分「连接中 / 已连接 / 连接失败 / 已结束」四种状态并显示真实原因（超时、拒绝、认证失败、主机密钥校验失败等）；进程退出即时反馈，失败态提供「重新连接」与「会话列表」；终端几何在每帧轮询校正，窗口缩放后 PTY 列数同步。

**中文输入**：本地 shell 启动时统一切换控制台代码页到 UTF-8（PowerShell 同时设置 `InputEncoding / OutputEncoding`），中文输入与回显正常。

### SFTP

双面板（本地 / 远端）多选传输；中间一列两个方向按钮；面板内可新建文件夹、删除、刷新、返回上级；传输按 **1MB 分块**进行（不会把整个大文件读进内存），HUD 实时进度与取消，失败时显示具体原因。

### Git

系统 git 子进程，**所有操作都在后台线程执行**（UI 不卡死）：

- **仓库登记**：选择本地目录即登记（校验 `.git`），也支持从远程克隆（填写 URL / 目标目录 / 别名）
- **工作台**：变更 / 分支 / 历史 / Stash 四个分区
  - 变更：冲突 / 未暂存 / 已暂存三组列表，行内暂存 / 取消暂存 / 丢弃；右侧差异面板（新增绿 / 删除红 / 冲突标记高亮）
  - 提交栏：提交信息、Amend、全存 / 全消 / 提交
  - 分支：新建 / 切换 / 合并 / 删除；历史：分页加载、cherry-pick / reset / 复制哈希；Stash：暂存 / pop / drop
- **远程**：顶部可直接编辑远程地址（回车保存），提供获取 / 拉取 / 推送
- **密钥管理**：生成（Ed25519）/ 导入私钥、复制公钥、绑定到当前仓库（可解绑）

### JSON

自写递归下降解析器：错误带精确行列，识别尾逗号与单引号；格式化 / 压缩 / 排序（升序降序 × 递归或仅顶层）/ 树形浏览（可复制子树）/ 统计，文本与树形双视图。

## 开箱即用

应用按以下顺序解析外部工具（git / ssh / ssh-keygen），任一命中即可用，全部未命中才提示缺失：

1. 环境变量 `DEVKIT_GIT` / `DEVKIT_SSH` / `DEVKIT_SSH_KEYGEN` 指定的可执行文件
2. exe 同级 `tools\` 目录（随应用分发的便携版）：`tools\git\cmd\git.exe`（MinGit）、`tools\ssh\ssh.exe`（Win32-OpenSSH）
3. 数据目录 `tools\`（应用内「一键安装」的下载位置）
4. 系统 PATH

- **一键安装**：设置 → 通用 → 运行工具，缺失时可直接从 GitHub 下载便携版 MinGit / Win32-OpenSSH 到数据目录，不写系统目录、不改 PATH；Git / SSH 工具页顶部也有缺失横幅入口
- **不闪黑框**：应用是 GUI 子系统程序，启动 git / ssh / powershell 等控制台子进程时统一带 `CREATE_NO_WINDOW`（见 `crates/tools/src/proc.rs`），启动和使用过程中都不会弹命令行窗口
- **发布打包**：仓库根目录 `package.ps1` 生成自包含 zip（devkit.exe + tools\），解压即用，无需系统安装 Git / OpenSSH
- **无头检查**：`devkit.exe --check-tools` 打印三个工具的解析路径与可用性，用于排障
- **无头截图**：`devkit.exe --screenshot out.png` 离屏渲染一张界面图（配合 `DEVKIT_SHOT_TOOL` 切换界面），用于在没有桌面会话的环境里检查排版
- MinGit 为 GPLv2、Win32-OpenSSH 为 BSD 系许可，随包分发时保留上游许可声明（打包脚本会生成 THIRD-PARTY-NOTICES.txt）

## 构建与运行

```powershell
cd devkit-windows
cargo run
```

- 工具链：`stable-x86_64-pc-windows-gnu`（`.cargo/config.toml` 指定 w64devkit gcc 作为链接器，需安装到 `C:\devtools\w64devkit`）
  - 链接器需要能找到 `ld.exe`，若 `C:\devtools\w64devkit\bin` 不在 PATH 里，请先把它加进 PATH
- 运行依赖：见上文「开箱即用」——内置便携版优先，缺失可一键安装或随发布包分发
- 无 GPU 环境可设 `SLINT_BACKEND=winit-software` 使用软件渲染

## 测试

- **单元 / 集成测试**：`cargo test`（终端网格、PTY 冒烟、HTTP 选项切换不 panic）
- **UI 交互点击测试**：`tests/ui/` 下是一套基于 **Windows UI Automation + 真实鼠标/键盘事件** 的脚本，
  会启动真实窗口、逐个点击控件并断言回调是否真的生效（响应体是否渲染、命令是否执行、
  提交是否落库、快捷键是否触发……），而不是只截图看排版

  ```powershell
  cd devkit-windows
  cargo build --release
  powershell -NoProfile -ExecutionPolicy Bypass -File tests\ui\run-all.ps1
  ```

  用例覆盖启动器、HTTP、JSON、SSH、SFTP、Git、外壳与设置；详见 [tests/ui/README.md](./devkit-windows/tests/ui/README.md)

## 仓库结构

```
Windows-Developer-Toolkit/
├─ README.md
├─ devkit-windows-rust-slint-plan.md   # 移植计划
└─ devkit-windows/                     # Cargo workspace
   ├─ crates/core/                     # db / prefs / sleep / appearance / session / tool
   ├─ crates/tools/                    # http / ssh / sftp / git / json / terminal
   ├─ crates/app/                      # Slint UI（MainWindow + globals + widgets）+ 胶水层
   │  ├─ ui/globals/                   # Theme（设计令牌）+ 各工具的 UI 状态镜像
   │  ├─ ui/components/Controls.slint  # 通用控件（按钮 / 标签 / 徽标 / 空状态 / 树行）
   │  ├─ ui/widgets/                   # 五个工具视图 + 侧栏 / Launcher / 设置 / 终端
   │  └─ src/                          # shell.rs（外壳）+ glue_*.rs（各工具胶水）
   └─ tests/ui/                        # UI 交互点击测试（UIA + 真实鼠标键盘事件）
```
