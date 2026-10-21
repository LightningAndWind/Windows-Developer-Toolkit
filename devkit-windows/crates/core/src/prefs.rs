//! 应用偏好：存 Windows 注册表 HKCU\Software\devkit（对应 macOS 端 UserDefaults）。
//! 注意：数据目录路径也存这里（数据库文件在数据目录内，偏好必须先于数据库可用）。

use anyhow::{anyhow, Result};
use winreg::enums::*;
use winreg::RegKey;

const SUBKEY: &str = r"Software\devkit";

pub const KEY_SIDEBAR_WIDTH: &str = "sidebar_width";
pub const KEY_APPEARANCE_MODE: &str = "appearance_mode"; // 0=跟随系统 1=浅色 2=深色
pub const KEY_PREVENT_SLEEP: &str = "prevent_sleep";
pub const KEY_UI_SCALE: &str = "ui_scale"; // 0=跟随系统 1=110% 2=125% 3=150%
pub const KEY_DATA_DIR: &str = "data_dir";

fn open_or_create() -> Result<RegKey> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    Ok(hkcu.create_subkey(SUBKEY)?.0)
}

pub fn get_str(key: &str) -> Option<String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let sk = hkcu.open_subkey(SUBKEY).ok()?;
    sk.get_value(key).ok()
}

pub fn set_str(key: &str, value: &str) -> Result<()> {
    open_or_create()?.set_value(key, &value).map_err(|e| anyhow!("{e}"))
}

pub fn get_int(key: &str) -> Option<u32> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let sk = hkcu.open_subkey(SUBKEY).ok()?;
    sk.get_value(key).ok()
}

pub fn set_int(key: &str, value: u32) -> Result<()> {
    open_or_create()?.set_value(key, &value).map_err(|e| anyhow!("{e}"))
}

pub fn remove(key: &str) {
    if let Ok(hkcu) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(SUBKEY, KEY_SET_VALUE) {
        let _ = hkcu.delete_value(key);
    }
}

/// 已登记的数据目录；不存在或为空返回 None。
pub fn data_dir() -> Option<String> {
    get_str(KEY_DATA_DIR).filter(|s| !s.trim().is_empty())
}

/// 设置数据目录（校验可写）。
pub fn set_data_dir(path: &str) -> Result<()> {
    validate_writable(path)?;
    set_str(KEY_DATA_DIR, path)
}

/// 校验目录可写：写入探针文件后删除。
pub fn validate_writable(path: &str) -> Result<()> {
    let p = std::path::Path::new(path);
    std::fs::create_dir_all(p)?;
    let probe = p.join(format!(".devkit-write-test-{}", uuid::Uuid::new_v4()));
    std::fs::write(&probe, b"").map_err(|e| anyhow!("目录不可写：{}（{e}）", path))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}
