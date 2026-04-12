//! 固件资源命令 — 清单获取与下载

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

/// 资源分类信息（返回给前端）
#[derive(Debug, Clone, Serialize)]
pub struct CategoryInfo {
    pub name: String,
    pub desc: Option<String>,
    pub children: Vec<ChildInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChildInfo {
    pub name: String,
    pub desc: Option<String>,
    pub versions: Vec<VersionInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionInfo {
    pub name: String,
    pub desc: Option<String>,
    pub files: Vec<FileInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileInfo {
    pub desc: String,
    pub filename: String,
    pub sha256: String,
    pub size: u64,
    pub size_text: String,
    pub path: String,
}

/// 下载进度事件（通过 Tauri 事件发送给前端）
#[derive(Debug, Clone, Serialize)]
pub struct ResourceProgress {
    pub filename: String,
    pub downloaded: u64,
    pub total: u64,
    pub index: usize,
    pub file_count: usize,
    pub status: String, // "downloading" | "verified" | "failed" | "hash_mismatch"
    pub message: String,
}

/// 获取资源清单
#[tauri::command]
pub fn resource_list() -> Result<Vec<CategoryInfo>, String> {
    let manifest = luatos_resource::fetch_manifest().map_err(|e| format!("获取资源清单失败: {e}"))?;

    let categories = manifest
        .resouces
        .iter()
        .map(|cat| CategoryInfo {
            name: cat.name.clone(),
            desc: cat.desc.clone(),
            children: cat
                .childrens
                .iter()
                .map(|child| ChildInfo {
                    name: child.name.clone(),
                    desc: child.desc.clone(),
                    versions: child
                        .versions
                        .iter()
                        .map(|ver| VersionInfo {
                            name: ver.name.clone(),
                            desc: ver.desc.clone(),
                            files: ver
                                .file_entries()
                                .iter()
                                .map(|f| FileInfo {
                                    desc: f.desc.clone(),
                                    filename: f.filename.clone(),
                                    sha256: f.sha256.clone(),
                                    size: f.size,
                                    size_text: luatos_resource::format_size(f.size),
                                    path: f.path.clone(),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();

    Ok(categories)
}

/// 资源下载取消标志（全局）
static DOWNLOAD_CANCEL: std::sync::LazyLock<Arc<AtomicBool>> = std::sync::LazyLock::new(|| Arc::new(AtomicBool::new(false)));

/// 下载指定模组的资源文件
#[tauri::command]
pub fn resource_download(app: AppHandle, module: String, version_filter: Option<String>, output_dir: String) -> Result<(), String> {
    DOWNLOAD_CANCEL.store(false, Ordering::Relaxed);

    let manifest = luatos_resource::fetch_manifest().map_err(|e| format!("获取资源清单失败: {e}"))?;

    let cat = luatos_resource::find_category(&manifest, &module).ok_or_else(|| format!("未找到模组: {module}"))?;

    let files = luatos_resource::collect_files(cat, version_filter.as_deref());
    if files.is_empty() {
        return Err(format!("未找到 {module} 的匹配文件 (版本过滤: {:?})", version_filter));
    }

    let out_path = std::path::PathBuf::from(&output_dir);
    let cancel = Arc::clone(&*DOWNLOAD_CANCEL);

    // 在后台线程执行下载
    std::thread::spawn(move || {
        let app2 = app.clone();
        let file_count = files.len();

        let callback: luatos_resource::DownloadCallback = Box::new(move |event| {
            // 检查取消标志
            if cancel.load(Ordering::Relaxed) {
                return;
            }

            let progress = match event {
                luatos_resource::DownloadEvent::StartFile { filename, size, index, total } => ResourceProgress {
                    filename: filename.clone(),
                    downloaded: 0,
                    total: *size,
                    index: *index,
                    file_count: *total,
                    status: "downloading".into(),
                    message: format!("开始下载 {filename}"),
                },
                luatos_resource::DownloadEvent::Progress { filename, downloaded, total } => ResourceProgress {
                    filename: filename.clone(),
                    downloaded: *downloaded,
                    total: *total,
                    index: 0,
                    file_count,
                    status: "downloading".into(),
                    message: format!("{}/{}", luatos_resource::format_size(*downloaded), luatos_resource::format_size(*total)),
                },
                luatos_resource::DownloadEvent::Verified { filename, dest } => ResourceProgress {
                    filename: filename.clone(),
                    downloaded: 0,
                    total: 0,
                    index: 0,
                    file_count,
                    status: "verified".into(),
                    message: format!("SHA256 校验通过: {dest}"),
                },
                luatos_resource::DownloadEvent::HashMismatch { filename, expected, actual } => ResourceProgress {
                    filename: filename.clone(),
                    downloaded: 0,
                    total: 0,
                    index: 0,
                    file_count,
                    status: "hash_mismatch".into(),
                    message: format!("SHA256 不匹配 (期望 {expected}, 实际 {actual})"),
                },
                luatos_resource::DownloadEvent::MirrorFailed { filename, error, .. } => ResourceProgress {
                    filename: filename.clone(),
                    downloaded: 0,
                    total: 0,
                    index: 0,
                    file_count,
                    status: "downloading".into(),
                    message: format!("镜像失败，尝试下一个: {error}"),
                },
                luatos_resource::DownloadEvent::FileFailed { filename } => ResourceProgress {
                    filename: filename.clone(),
                    downloaded: 0,
                    total: 0,
                    index: 0,
                    file_count,
                    status: "failed".into(),
                    message: "所有镜像均失败".into(),
                },
            };

            let _ = app2.emit("resource:progress", &progress);
        });

        let result = luatos_resource::download_files(&module, &files, &manifest.mirrors, &out_path, Some(&callback));

        let final_progress = match result {
            Ok(report) => ResourceProgress {
                filename: String::new(),
                downloaded: 0,
                total: 0,
                index: 0,
                file_count,
                status: if report.failed == 0 { "complete".into() } else { "partial".into() },
                message: format!("下载完成: {} 成功, {} 失败", report.succeeded, report.failed),
            },
            Err(e) => ResourceProgress {
                filename: String::new(),
                downloaded: 0,
                total: 0,
                index: 0,
                file_count,
                status: "error".into(),
                message: format!("下载出错: {e}"),
            },
        };
        let _ = app.emit("resource:progress", &final_progress);
    });

    Ok(())
}

/// 取消下载
#[tauri::command]
pub fn resource_cancel() {
    DOWNLOAD_CANCEL.store(true, Ordering::Relaxed);
}
