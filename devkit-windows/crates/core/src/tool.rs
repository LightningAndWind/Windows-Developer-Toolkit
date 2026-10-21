//! 工具注册协议：所有工具实现统一接口，Shell 只负责装配。
//! 对应 macOS 端 `ToolProtocol.swift`。
//!
//! 说明：各工具的运行时状态由 app crate 的 `*Run` 结构体承载（见 glue_*.rs），
//! 会话快照的序列化也放在那里；本模块只保留与 UI 无关的静态描述与分类。

/// 工具分类（Launcher 分组展示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    Network,
    Terminal,
    Data,
    Utility,
}

impl ToolCategory {
    pub fn label(&self) -> &'static str {
        match self {
            ToolCategory::Network => "网络",
            ToolCategory::Terminal => "终端",
            ToolCategory::Data => "数据",
            ToolCategory::Utility => "实用工具",
        }
    }

    pub fn all() -> [ToolCategory; 4] {
        [ToolCategory::Network, ToolCategory::Terminal, ToolCategory::Data, ToolCategory::Utility]
    }
}

/// 工具静态描述：图标、名称、能力声明。所有工具实例共享一份。
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    /// 工具唯一 ID，用作持久化 key（不可变更）。
    pub id: &'static str,
    /// 工具显示名称，如 "HTTP 请求"。
    pub title: &'static str,
    /// 图标字符（SF Symbol 的 Windows 替代，先用字形，后续可换矢量图标）。
    pub icon: &'static str,
    /// 工具分类。
    pub category: ToolCategory,
    /// 简短描述（Launcher 卡片副标题）。
    pub subtitle: &'static str,
    /// 是否允许多实例（多个标签同时打开）。
    pub allows_multiple_instances: bool,
}

/// 工具运行时接口。
///
/// 保留给后续把各工具运行时统一到 trait 之下用；当前实现直接使用 app crate 里的
/// `JsonRun / HttpRun / SshRun / SftpRun / GitRun`，因此本 trait 暂未接线。
#[allow(dead_code)]
pub trait Tool {
    /// 动态标签标题；返回 None 时 Shell 使用 descriptor.title。
    fn dynamic_title(&self) -> Option<String> {
        None
    }

    /// 标签是否有未保存内容（关闭时提醒）。
    fn has_unsaved(&self) -> bool {
        false
    }

    /// 是否支持 Ctrl+S 保存。
    fn supports_save(&self) -> bool {
        false
    }

    /// 当前内容是否已关联到一条已保存记录。
    fn is_saved(&self) -> bool {
        false
    }

    /// 原地保存到既有记录；仅在 is_saved 为真时由 Shell 调用。
    fn save_via_shell(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// 标签被关闭时由 Shell 调用：释放网络连接、子进程、文件句柄等。
    fn teardown_on_tab_close(&mut self) {}

    /// 将工具内部状态编码为随标签持久化的 JSON；返回 None 表示无需持久化。
    fn session_state(&self) -> Option<serde_json::Value> {
        None
    }

    /// 从快照恢复工具内部状态（App 重启后重建标签时调用）。
    fn restore(&mut self, _state: &serde_json::Value) {}
}

/// 内置工具 ID。
pub mod ids {
    pub const HTTP: &str = "http";
    pub const SSH: &str = "ssh";
    pub const SFTP: &str = "sftp";
    pub const GIT: &str = "git";
    pub const JSON: &str = "json";
}
