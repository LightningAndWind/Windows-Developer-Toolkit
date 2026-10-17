//! 防休眠：SetThreadExecutionState（对应 macOS 端 SleepKeeper 双断言：
//! 防屏保 + 防系统休眠）。

use windows::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
};

/// 开启后持有防屏保/显示关闭 + 防系统休眠两条断言；关闭即释放。
pub fn set_prevent_sleep(on: bool) {
    unsafe {
        let mut flags = ES_CONTINUOUS;
        if on {
            flags |= ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED;
        }
        let _ = SetThreadExecutionState(flags);
    }
}
