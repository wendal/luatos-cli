// 合宙简易量产刷机程序 — eframe 入口。
//
// 功能：
//   - 选取 SOC 固件文件
//   - 选取串口（UART 类芯片）
//   - 开始/停止刷机
//   - 实时进度与日志显示

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod state;
mod worker;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let version = env!("CARGO_PKG_VERSION");
    let title = format!("合宙简易量产刷机程序 v{version}");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(&title)
            .with_inner_size([640.0, 580.0])
            .with_min_inner_size([560.0, 480.0]),
        ..Default::default()
    };

    if let Err(e) = eframe::run_native(&title, options, Box::new(|cc| Ok(Box::new(app::MfguiApp::new(cc))))) {
        eprintln!("启动失败: {e}");
        std::process::exit(1);
    }
}
