use crate::OutputFormat;

/// 格式化文件大小
fn format_size(bytes: u64) -> String {
    luatos_resource::format_size(bytes)
}

pub fn cmd_resource_list(module: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    let manifest = luatos_resource::fetch_manifest()?;

    match module {
        None => {
            // 列出所有模组
            match format {
                OutputFormat::Text => {
                    println!("{:<20} DESCRIPTION", "MODULE");
                    for cat in &manifest.resouces {
                        println!("{:<20} {}", cat.name, cat.desc.as_deref().unwrap_or(""));
                    }
                }
                OutputFormat::Json => {
                    let modules: Vec<_> = manifest
                        .resouces
                        .iter()
                        .map(|c| {
                            serde_json::json!({
                                "name": c.name,
                                "desc": c.desc,
                            })
                        })
                        .collect();
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "resource.list",
                        "data": modules,
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        Some(name) => {
            let cat = luatos_resource::find_category(&manifest, name).ok_or_else(|| anyhow::anyhow!("Module '{}' not found", name))?;

            match format {
                OutputFormat::Text => {
                    println!("{} — {}", cat.name, cat.desc.as_deref().unwrap_or(""));
                    for child in &cat.childrens {
                        println!("\n  {} — {}", child.name, child.desc.as_deref().unwrap_or(""));
                        for ver in &child.versions {
                            println!("    {} — {}", ver.name, ver.desc.as_deref().unwrap_or(""));
                            for entry in ver.file_entries() {
                                println!("      {}  {}  {}", entry.desc, entry.filename, format_size(entry.size));
                            }
                        }
                    }
                }
                OutputFormat::Json => {
                    let children: Vec<_> = cat
                        .childrens
                        .iter()
                        .map(|child| {
                            let versions: Vec<_> = child
                                .versions
                                .iter()
                                .map(|ver| {
                                    let files: Vec<_> = ver
                                        .file_entries()
                                        .iter()
                                        .map(|e| {
                                            serde_json::json!({
                                                "desc": e.desc,
                                                "filename": e.filename,
                                                "sha256": e.sha256,
                                                "size": e.size,
                                                "path": e.path,
                                            })
                                        })
                                        .collect();
                                    serde_json::json!({
                                        "name": ver.name,
                                        "desc": ver.desc,
                                        "files": files,
                                    })
                                })
                                .collect();
                            serde_json::json!({
                                "name": child.name,
                                "desc": child.desc,
                                "versions": versions,
                            })
                        })
                        .collect();
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "resource.list",
                        "data": {
                            "name": cat.name,
                            "desc": cat.desc,
                            "childrens": children,
                        },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
    }
    Ok(())
}

pub fn cmd_resource_download(module: &str, version_filter: Option<&str>, output: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let manifest = luatos_resource::fetch_manifest()?;

    let cat = luatos_resource::find_category(&manifest, module).ok_or_else(|| anyhow::anyhow!("Module '{}' not found", module))?;

    let files = luatos_resource::collect_files(cat, version_filter);
    if files.is_empty() {
        anyhow::bail!("No files found for module '{}' with version filter {:?}", module, version_filter);
    }

    let out_path = std::path::Path::new(output);

    // 构建进度回调 — CLI 使用 indicatif 进度条
    let pb = std::sync::Mutex::new(None::<indicatif::ProgressBar>);
    let is_text = format == &OutputFormat::Text;

    let callback: luatos_resource::DownloadCallback = Box::new(move |event| match event {
        luatos_resource::DownloadEvent::StartFile { filename, size, index, total } => {
            if is_text {
                eprintln!("[{}/{}] 下载 {} ({})...", index + 1, total, filename, luatos_resource::format_size(*size));
            }
            let new_pb = indicatif::ProgressBar::new(*size);
            new_pb.set_style(
                indicatif::ProgressStyle::default_bar()
                    .template("  [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                    .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar())
                    .progress_chars("##-"),
            );
            *pb.lock().unwrap() = Some(new_pb);
        }
        luatos_resource::DownloadEvent::Progress { downloaded, .. } => {
            if let Some(p) = pb.lock().unwrap().as_ref() {
                p.set_position(*downloaded);
            }
        }
        luatos_resource::DownloadEvent::Verified { filename, dest } => {
            if let Some(p) = pb.lock().unwrap().take() {
                p.finish_and_clear();
            }
            if is_text {
                eprintln!("  ✓ SHA256 校验通过: {} → {}", filename, dest);
            }
        }
        luatos_resource::DownloadEvent::HashMismatch { filename, expected, actual } => {
            if let Some(p) = pb.lock().unwrap().take() {
                p.finish_and_clear();
            }
            eprintln!("  ✗ SHA256 不匹配 {} (期望 {}, 实际 {})", filename, expected, actual);
        }
        luatos_resource::DownloadEvent::MirrorFailed { mirror_url, filename, error } => {
            log::warn!("镜像 {} 下载 {} 失败: {}", mirror_url, filename, error);
        }
        luatos_resource::DownloadEvent::FileFailed { filename } => {
            if let Some(p) = pb.lock().unwrap().take() {
                p.finish_and_clear();
            }
            eprintln!("  ✗ 下载失败: {}", filename);
        }
        luatos_resource::DownloadEvent::Extracted { filename, dest_dir } => {
            if is_text {
                eprintln!("  ⬡ 已解压: {} → {}", filename, dest_dir);
            }
        }
    });

    let report = luatos_resource::download_files(module, &files, &manifest.mirrors, out_path, Some(&callback))?;

    match format {
        OutputFormat::Text => {
            println!("\n下载完成: {} 成功, {} 失败", report.succeeded, report.failed);
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": if report.failed == 0 { "ok" } else { "partial" },
                "command": "resource.download",
                "data": {
                    "module": module,
                    "downloaded": report.succeeded,
                    "failed": report.failed,
                    "output": output,
                    "files": report.files,
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }

    if report.failed > 0 {
        anyhow::bail!("{} 个文件下载失败", report.failed);
    }
    Ok(())
}
