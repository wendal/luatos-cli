//! 文件/目录选择对话框

/// 打开文件选择对话框
#[tauri::command]
pub fn open_file_dialog(title: String, filter_name: String, extensions: Vec<String>) -> Option<String> {
    let exts: Vec<&str> = extensions.iter().map(|s| s.as_str()).collect();
    rfd::FileDialog::new()
        .set_title(&title)
        .add_filter(&filter_name, &exts)
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

/// 打开目录选择对话框
#[tauri::command]
pub fn open_folder_dialog(title: String) -> Option<String> {
    rfd::FileDialog::new().set_title(&title).pick_folder().map(|p| p.to_string_lossy().into_owned())
}
