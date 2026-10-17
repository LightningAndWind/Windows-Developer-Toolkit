//! 深浅色外观：读 Windows 系统设置 AppsUseLightTheme（对应 macOS 端 NSApp.appearance 三态）。
//! 应用层负责轮询/监听变化后刷新 UI。

use winreg::enums::*;
use winreg::RegKey;

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// 系统当前是否偏好深色应用。读取失败默认浅色。
pub fn system_prefers_dark() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(sk) = hkcu.open_subkey(PERSONALIZE) else { return false };
    let light: u32 = sk.get_value("AppsUseLightTheme").unwrap_or(1);
    light == 0
}
