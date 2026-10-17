//! SFTP 工具逻辑：双后端（本地文件系统 / 远端 russh-sftp）+ 并发传输。
//! 对应 macOS 端 Tools/SFTP/Logic。

pub mod backend;
pub mod local;
pub mod remote;
pub mod transport;
