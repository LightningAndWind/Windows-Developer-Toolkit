fn main() {
    // 无头检查外部运行工具解析结果（排障 / 脚本用）：devkit --check-tools
    if std::env::args().any(|a| a == "--check-tools") {
        check_tools();
        return;
    }
    if let Err(e) = devkit::shell::run_app() {
        eprintln!("devkit 启动失败：{e}");
        std::process::exit(1);
    }
}

/// 打印 git / ssh / ssh-keygen 的解析路径与可用性（解析顺序见 devkit_tools::toolchain）。
fn check_tools() {
    println!(
        "git        : {}",
        devkit_tools::toolchain::git_path().as_deref().unwrap_or("(未解析，回退 PATH 裸名)")
    );
    println!("git 可用   : {}", devkit_tools::toolchain::git_available());
    println!(
        "ssh        : {}",
        devkit_tools::toolchain::ssh_path().as_deref().unwrap_or("(未解析，回退 PATH 裸名)")
    );
    println!("ssh 可用   : {}", devkit_tools::toolchain::ssh_available());
    println!(
        "ssh-keygen : {}",
        devkit_tools::toolchain::ssh_keygen_path().as_deref().unwrap_or("(未解析，回退 PATH 裸名)")
    );
}
