fn main() {
    if let Err(e) = devkit::shell::run_app() {
        eprintln!("devkit 启动失败：{e}");
        std::process::exit(1);
    }
}
