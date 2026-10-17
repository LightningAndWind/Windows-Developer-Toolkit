# Windows Developer Toolkit

对标 [Macos-Developer-Toolkit](https://github.com/LightningAndWind/Macos-Developer-Toolkit) 的 Windows 端实现：面向 Windows 开发者的本地化工具集，用标签页方式把 HTTP / SSH / SFTP / Git / JSON 五个高频工具聚合到一个 App 内，追求**精简、快速、低内存占用**。

移植计划与技术决策见 [devkit-windows-rust-slint-plan.md](./devkit-windows-rust-slint-plan.md)。

## 技术栈

- **UI**：Slint 1.13（声明式，femtovg 渲染，软件渲染兜底）
- **语言 / 异步**：Rust + tokio
- **HTTP**：reqwest 0.12（cookie 存储 + stream，支持 SSE 流式）
- **终端**：系统 OpenSSH 子进程 + ConPTY（portable-pty + vte 自绘终端网格，30ms 合帧 + 3000 行回滚）
- **SFTP**：russh + russh-sftp（进程内，密码 / 私钥认证）
- **数据存储**：SQLite（rusqlite bundled，10 表：kv / http_history / http_folders / http_requests / ssh_folders / ssh_profiles / ssh_known_hosts / git_folders / git_repos / git_keys）
- **系统集成**：windows crate（SetThreadExecutionState 防休眠双断言）、winreg（监听深浅色）
- **工程**：Cargo workspace 三 crate —— `core`（无 UI 基础层）/ `tools`（五工具纯逻辑）/ `app`（Slint UI + 胶水）

## 应用外壳

- **首启引导**：强制选择数据目录（校验可写）后才进入主界面
- **标签页管理**：一个标签 = 一个工具实例，多实例并行、分组、自定义标题、双击重命名、拖拽排序；视图常驻不销毁（`visible:` 切换，避免重建丢状态）；关闭标签触发 `teardown` 杀 shell / 中止任务
- **Launcher**：工具卡片分类展示，顶部搜索框过滤
- **会话快照**：重启自动还原标签结构及各工具内部状态（存 kv 表 JSON）
- **外观三态**：跟随系统 / 浅色 / 深色，注册表监听 `AppsUseLightTheme` 实时切换
- **防休眠**：侧栏咖啡杯开关，同时持有防屏保活与防系统休眠两条断言，状态持久化
- **设置面板**：通用 / HTTP 历史 / 终端会话 / Git 仓库 四分区
- **快捷键**：Ctrl+T 新标签、Ctrl+1~9/0 直达标签、Ctrl+↓/↑ 切换上/下一个标签（环绕）、Ctrl+W 关闭、Ctrl+E 重命名、Ctrl+S 保存请求（HTTP 工具）、Ctrl+, 设置

## 内置工具

### HTTP 请求

Params / Headers / Body（无 / JSON / Form / Raw）/ Auth 四类编辑区；超时档位（空闲超时语义，兼容 SSE）；响应 Pretty / Raw / Headers / Preview 四视图；10 MB 响应封顶；发送中可随时停止；SQLite 历史与多级集合树；复制为 cURL。

### SSH 终端

本地终端 + SSH 远程会话统一走系统 OpenSSH + ConPTY，密码 / 口令 / 主机信任提示通过 PTY 自动应答；TOFU known_hosts；连接状态横幅；断线重连；关标签杀 shell 防孤儿进程。

### SFTP

双面板（本地 / 远端）多选传输；小文件（< 500 KB）4 并发流水线，大文件顺序；HUD 实时进度与取消。

### Git

系统 git 子进程：四组变更（暂存 / 未暂存 / 冲突 / 未跟踪）、diff 着色、冲突解决、分支管理、Stash、提交历史分页、克隆；内置 ssh-keygen 密钥生成，私钥认证通过 `GIT_SSH_COMMAND` 注入。

### JSON

自写递归下降解析器：错误带精确行列，识别尾逗号与单引号；格式化 / 压缩 / 排序 / 树形浏览 / 统计，编辑与格式化双视图。

## 开箱即用

应用按以下顺序解析外部工具（git / ssh / ssh-keygen），任一命中即可用，全部未命中才提示缺失：

1. 环境变量 `DEVKIT_GIT` / `DEVKIT_SSH` / `DEVKIT_SSH_KEYGEN` 指定的可执行文件
2. exe 同级 `tools\` 目录（随应用分发的便携版）：`tools\git\cmd\git.exe`（MinGit）、`tools\ssh\ssh.exe`（Win32-OpenSSH）
3. 数据目录 `tools\`（应用内「一键安装」的下载位置）
4. 系统 PATH

- **一键安装**：设置 → 通用 → 运行工具，缺失时可直接从 GitHub 下载便携版 MinGit / Win32-OpenSSH 到数据目录，不写系统目录、不改 PATH；Git / SSH 工具页顶部也有缺失横幅入口
- **发布打包**：仓库根目录 `package.ps1` 生成自包含 zip（devkit.exe + tools\），解压即用，无需系统安装 Git / OpenSSH
- **无头检查**：`devkit.exe --check-tools` 打印三个工具的解析路径与可用性，用于排障
- MinGit 为 GPLv2、Win32-OpenSSH 为 BSD 系许可，随包分发时保留上游许可声明（打包脚本会生成 THIRD-PARTY-NOTICES.txt）

## 构建与运行

```powershell
cd devkit-windows
cargo run
```

- 工具链：`stable-x86_64-pc-windows-gnu`（`.cargo/config.toml` 指定 w64devkit gcc 作为链接器，需安装到 `C:\devtools\w64devkit`）
- 运行依赖：见上文「开箱即用」——内置便携版优先，缺失可一键安装或随发布包分发
- 无 GPU 环境可设 `SLINT_BACKEND=winit-software` 使用软件渲染

## 仓库结构

```
Windows-Developer-Toolkit/
├─ README.md
├─ devkit-windows-rust-slint-plan.md   # 移植计划
└─ devkit-windows/                     # Cargo workspace
   ├─ crates/core/                     # db / prefs / sleep / appearance / session / tool
   ├─ crates/tools/                    # http / ssh / sftp / git / json / terminal
   └─ crates/app/                      # Slint UI（MainWindow + globals + widgets）+ 胶水层
```
