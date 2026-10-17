//! devkit 核心层：数据、偏好、外观、防休眠、会话快照、工具协议。
//! 对应 macOS 端 `Core/` 目录，无 UI 依赖。

pub mod appearance;
pub mod db;
pub mod prefs;
pub mod session;
pub mod sleep;
pub mod tool;
