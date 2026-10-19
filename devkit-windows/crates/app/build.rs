use std::path::PathBuf;
use std::process::Command;

fn main() {
    slint_build::compile("ui/MainWindow.slint").unwrap();
    embed_windows_icon();
}

/// 把 assets/devkit.ico 嵌入 exe 资源（资源管理器文件图标、固定到任务栏的默认图标）。
/// 运行中的窗口图标由 MainWindow 的 icon 属性经 WM_SETICON 设置，两处独立。
fn embed_windows_icon() {
    let assets = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    let ico = assets.join("devkit.ico");
    if !ico.exists() {
        println!(
            "cargo:warning=未找到 {}，跳过图标嵌入（仓库根目录 make-icon.ps1 可生成）",
            ico.display()
        );
        return;
    }
    println!("cargo:rerun-if-changed={}", ico.display());
    println!(
        "cargo:rerun-if-changed={}",
        assets.join("app.rc").display()
    );

    // w64devkit 自带 windres：先找 PATH，再退回常见安装位置
    let candidates = ["windres", "C:/devtools/w64devkit/bin/windres.exe"];
    let mut obj = None;
    let mut last_err = None;
    for windres in candidates {
        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("devkit_icon.res");
        match Command::new(windres)
            .current_dir(&assets)
            .args(["-i", "app.rc", "-O", "coff", "-o"])
            .arg(&out)
            .status()
        {
            Ok(status) if status.success() => {
                obj = Some(out);
                break;
            }
            Ok(status) => last_err = Some(format!("{windres} 退出码 {status}")),
            Err(e) => last_err = Some(format!("{windres}: {e}")),
        }
    }
    let Some(obj) = obj else {
        let msg = last_err.unwrap_or_default();
        println!("cargo:warning=windres 不可用（{msg}），跳过图标嵌入");
        return;
    };
    println!("cargo:rustc-link-arg={}", obj.display());
}
