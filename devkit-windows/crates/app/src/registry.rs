//! 工具注册表：静态元数据（对应 macOS 端 ToolRegistry + ToolRegistration）。

use devkit_core::tool::{ToolCategory, ToolDescriptor};

pub const DESCRIPTORS: &[ToolDescriptor] = &[
    ToolDescriptor {
        id: devkit_core::tool::ids::HTTP,
        title: "HTTP 请求",
        icon: "🌐",
        category: ToolCategory::Network,
        subtitle: "构造并发送 HTTP 请求，查看响应与历史记录",
        allows_multiple_instances: true,
    },
    ToolDescriptor {
        id: devkit_core::tool::ids::SSH,
        title: "SSH 终端",
        icon: "🖥",
        category: ToolCategory::Terminal,
        subtitle: "本地终端与 SSH 会话，连接配置按文件夹归类管理",
        allows_multiple_instances: true,
    },
    ToolDescriptor {
        id: devkit_core::tool::ids::SFTP,
        title: "SFTP 传输",
        icon: "📁",
        category: ToolCategory::Terminal,
        subtitle: "左右双面板文件互传与远端文件管理",
        allows_multiple_instances: true,
    },
    ToolDescriptor {
        id: devkit_core::tool::ids::JSON,
        title: "JSON 工具",
        icon: "{ }",
        category: ToolCategory::Data,
        subtitle: "文本 / 树形双视图编辑、格式化、校验、键排序",
        allows_multiple_instances: true,
    },
    ToolDescriptor {
        id: devkit_core::tool::ids::GIT,
        title: "Git 管理",
        icon: "🌿",
        category: ToolCategory::Utility,
        subtitle: "仓库登记与密钥绑定，工作区变更 / 冲突 / 分支管理",
        allows_multiple_instances: true,
    },
];

pub fn find(id: &str) -> Option<&'static ToolDescriptor> {
    DESCRIPTORS.iter().find(|d| d.id == id)
}
