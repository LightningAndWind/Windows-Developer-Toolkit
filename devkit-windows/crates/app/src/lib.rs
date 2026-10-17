//! devkit 应用层：Slint UI 装配 + 胶水（对应 macOS 端 Shell/）。
//! 同时服务两个二进制：主程序 devkit 与终端 spike。

slint::include_modules!();

pub mod glue_git;
pub mod glue_http;
pub mod glue_ssh;
pub mod glue_sftp;
pub mod registry;
pub mod shell;
pub mod term_frame;
pub mod term_host;
