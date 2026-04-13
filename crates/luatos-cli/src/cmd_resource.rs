use crate::{
    event::{self, MessageLevel},
    OutputFormat,
};

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
                OutputFormat::Json | OutputFormat::Jsonl => {
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
                    event::emit_result(format, "resource.list", "ok", modules)?;
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
                OutputFormat::Json | OutputFormat::Jsonl => {
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
                    event::emit_result(
                        format,
                        "resource.list",
                        "ok",
                        serde_json::json!({
                            "name": cat.name,
                            "desc": cat.desc,
                            "childrens": children,
                        }),
                    )?;
                }
            }
        }
    }
    Ok(())
}

pub fn cmd_resource_download(category: &str, sub: Option<&str>, item: Option<&str>, output: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let manifest = luatos_resource::fetch_manifest()?;

    let cat = luatos_resource::find_category(&manifest, category).ok_or_else(|| anyhow::anyhow!("资源大类 '{}' 不存在，可用: resource list", category))?;

    // sub 必须提供
    let sub = sub.ok_or_else(|| anyhow::anyhow!("请指定子项或版本，例如:\n  resource download {} <sub_or_version> [item]", category))?;

    // 歧义消解：sub 是否是已知的子项名称？
    let files = if let Some(child) = luatos_resource::find_child(cat, sub) {
        // sub 是子项名 (如 soc_script) → item 为版本过滤
        luatos_resource::collect_files_for_child(child, item)
    } else {
        // sub 作为版本过滤器 (如 V2032) → item 为文件名过滤
        luatos_resource::collect_files_for_version(cat, sub, item)
    };

    if files.is_empty() {
        anyhow::bail!(
            "未找到匹配的文件 (category={}, sub={}, item={:?})\n提示: 使用 'resource list {}' 查看可用内容",
            category,
            sub,
            item,
            category
        );
    }

    let out_path = std::path::Path::new(output);

    // 构建进度回调 — CLI 使用 indicatif 进度条
    let pb = std::sync::Mutex::new(None::<indicatif::ProgressBar>);
    let format_clone = *format;

    let callback: luatos_resource::DownloadCallback = Box::new(move |event| match event {
        luatos_resource::DownloadEvent::StartFile { filename, size, index, total } => {
            match format_clone {
                OutputFormat::Text | OutputFormat::Json => {
                    let _ = event::emit_message(
                        &format_clone,
                        "resource.download",
                        MessageLevel::Info,
                        format!("[{}/{}] 下载 {} ({})...", index + 1, total, filename, luatos_resource::format_size(*size)),
                    );
                }
                OutputFormat::Jsonl => {
                    let _ = event::emit_jsonl_event(
                        &format_clone,
                        serde_json::json!({
                            "type": "resource_download",
                            "command": "resource.download",
                            "event": "start_file",
                            "filename": filename,
                            "size": size,
                            "index": index,
                            "total": total,
                        }),
                    );
                }
            }
            if format_clone == OutputFormat::Text {
                let new_pb = indicatif::ProgressBar::new(*size);
                new_pb.set_style(
                    indicatif::ProgressStyle::default_bar()
                        .template("  [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                        .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar())
                        .progress_chars("##-"),
                );
                *pb.lock().unwrap() = Some(new_pb);
            }
        }
        luatos_resource::DownloadEvent::Progress { downloaded, .. } => {
            if let Some(p) = pb.lock().unwrap().as_ref() {
                p.set_position(*downloaded);
            }
            let _ = event::emit_jsonl_event(
                &format_clone,
                serde_json::json!({
                    "type": "resource_download",
                    "command": "resource.download",
                    "event": "progress",
                    "downloaded": downloaded,
                }),
            );
        }
        luatos_resource::DownloadEvent::Verified { filename, dest } => {
            if let Some(p) = pb.lock().unwrap().take() {
                p.finish_and_clear();
            }
            match format_clone {
                OutputFormat::Text | OutputFormat::Json => {
                    let _ = event::emit_message(
                        &format_clone,
                        "resource.download",
                        MessageLevel::Info,
                        format!("  ✓ SHA256 校验通过: {} → {}", filename, dest),
                    );
                }
                OutputFormat::Jsonl => {
                    let _ = event::emit_jsonl_event(
                        &format_clone,
                        serde_json::json!({
                            "type": "resource_download",
                            "command": "resource.download",
                            "event": "verified",
                            "filename": filename,
                            "dest": dest,
                        }),
                    );
                }
            }
        }
        luatos_resource::DownloadEvent::HashMismatch { filename, expected, actual } => {
            if let Some(p) = pb.lock().unwrap().take() {
                p.finish_and_clear();
            }
            match format_clone {
                OutputFormat::Text | OutputFormat::Json => {
                    let _ = event::emit_message(
                        &format_clone,
                        "resource.download",
                        MessageLevel::Error,
                        format!("  ✗ SHA256 不匹配 {} (期望 {}, 实际 {})", filename, expected, actual),
                    );
                }
                OutputFormat::Jsonl => {
                    let _ = event::emit_jsonl_event(
                        &format_clone,
                        serde_json::json!({
                            "type": "resource_download",
                            "command": "resource.download",
                            "event": "hash_mismatch",
                            "filename": filename,
                            "expected": expected,
                            "actual": actual,
                        }),
                    );
                }
            }
        }
        luatos_resource::DownloadEvent::MirrorFailed { mirror_url, filename, error } => {
            log::warn!("镜像 {} 下载 {} 失败: {}", mirror_url, filename, error);
            let _ = event::emit_jsonl_event(
                &format_clone,
                serde_json::json!({
                    "type": "resource_download",
                    "command": "resource.download",
                    "event": "mirror_failed",
                    "mirror_url": mirror_url,
                    "filename": filename,
                    "error": error,
                }),
            );
        }
        luatos_resource::DownloadEvent::FileFailed { filename } => {
            if let Some(p) = pb.lock().unwrap().take() {
                p.finish_and_clear();
            }
            match format_clone {
                OutputFormat::Text | OutputFormat::Json => {
                    let _ = event::emit_message(&format_clone, "resource.download", MessageLevel::Error, format!("  ✗ 下载失败: {}", filename));
                }
                OutputFormat::Jsonl => {
                    let _ = event::emit_jsonl_event(
                        &format_clone,
                        serde_json::json!({
                            "type": "resource_download",
                            "command": "resource.download",
                            "event": "file_failed",
                            "filename": filename,
                        }),
                    );
                }
            }
        }
        luatos_resource::DownloadEvent::Extracted { filename, dest_dir } => match format_clone {
            OutputFormat::Text | OutputFormat::Json => {
                let _ = event::emit_message(&format_clone, "resource.download", MessageLevel::Info, format!("  ⬡ 已解压: {} → {}", filename, dest_dir));
            }
            OutputFormat::Jsonl => {
                let _ = event::emit_jsonl_event(
                    &format_clone,
                    serde_json::json!({
                        "type": "resource_download",
                        "command": "resource.download",
                        "event": "extracted",
                        "filename": filename,
                        "dest_dir": dest_dir,
                    }),
                );
            }
        },
    });

    let report = luatos_resource::download_files(category, &files, &manifest.mirrors, out_path, Some(&callback))?;

    match format {
        OutputFormat::Text => {
            println!("\n下载完成: {} 成功, {} 失败", report.succeeded, report.failed);
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "resource.download",
            if report.failed == 0 { "ok" } else { "partial" },
            serde_json::json!({
                "category": category,
                "sub": sub,
                "item": item,
                "downloaded": report.succeeded,
                "failed": report.failed,
                "output": output,
                "files": report.files,
            }),
        )?,
    }

    if report.failed > 0 {
        anyhow::bail!("{} 个文件下载失败", report.failed);
    }
    Ok(())
}
