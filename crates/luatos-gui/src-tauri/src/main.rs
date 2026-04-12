// 在 release 模式下隐藏 Windows 控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;

use state::AppState;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            // 串口
            commands::serial::serial_list,
            // 刷机
            commands::flash::flash_run,
            commands::flash::flash_cancel,
            // 日志
            commands::log::log_start,
            commands::log::log_start_binary,
            commands::log::log_stop,
            // SOC 信息
            commands::soc::soc_info,
            // 项目管理
            commands::project::project_new,
            commands::project::project_open,
            commands::project::project_save,
            commands::project::project_import,
            // 构建
            commands::build::build_luac,
            commands::build::build_filesystem,
            // 资源下载
            commands::resource::resource_list,
            commands::resource::resource_download,
            commands::resource::resource_cancel,
            // 设置
            commands::settings::settings_load,
            commands::settings::settings_save,
            // 对话框
            commands::dialog::open_file_dialog,
            commands::dialog::open_folder_dialog,
        ])
        .run(tauri::generate_context!())
        .expect("启动 LuatOS GUI 失败");
}
