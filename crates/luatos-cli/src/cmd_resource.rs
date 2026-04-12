use crate::OutputFormat;

const RESOURCE_MANIFEST_URLS: &[&str] = &["http://bj02.air32.cn:10888/files/files.json", "http://sh.air32.cn:10888/files/files.json"];

#[derive(serde::Deserialize, Debug)]
struct ResourceManifest {
    #[allow(dead_code)]
    version: u32,
    mirrors: Vec<Mirror>,
    resouces: Vec<ResourceCategory>, // NOTE: typo in server JSON, keep as-is
}

#[derive(serde::Deserialize, Debug, Clone)]
struct Mirror {
    url: String,
    #[allow(dead_code)]
    speed: Option<u32>,
}

#[derive(serde::Deserialize, Debug)]
struct ResourceCategory {
    name: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    url: Option<String>,
    #[serde(default)]
    childrens: Vec<ResourceChild>,
}

#[derive(serde::Deserialize, Debug)]
struct ResourceChild {
    name: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    versions: Vec<ResourceVersion>,
}

#[derive(serde::Deserialize, Debug)]
struct ResourceVersion {
    name: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    files: Vec<serde_json::Value>,
}

#[allow(dead_code)]
struct FileEntry {
    desc: String,
    filename: String,
    sha256: String,
    size: u64,
    path: String,
}

/// Parse a file entry from the JSON value.
/// Format: array `["desc", "filename", "sha256", size_number, "path"]`
fn parse_file_entry(val: &serde_json::Value) -> Option<FileEntry> {
    let arr = val.as_array()?;
    if arr.len() < 5 {
        return None;
    }
    Some(FileEntry {
        desc: arr[0].as_str()?.to_string(),
        filename: arr[1].as_str()?.to_string(),
        sha256: arr[2].as_str()?.to_string(),
        size: arr[3].as_u64()?,
        path: arr[4].as_str()?.to_string(),
    })
}

fn fetch_manifest() -> anyhow::Result<ResourceManifest> {
    let mut last_err = None;
    for url in RESOURCE_MANIFEST_URLS {
        match ureq::get(url).call() {
            Ok(resp) => {
                let body = resp.into_string()?;
                let manifest: ResourceManifest = serde_json::from_str(&body)?;
                return Ok(manifest);
            }
            Err(e) => {
                log::warn!("Failed to fetch {url}: {e}");
                last_err = Some(e);
            }
        }
    }
    anyhow::bail!(
        "Failed to fetch resource manifest from all mirrors: {}",
        last_err.map(|e| e.to_string()).unwrap_or_else(|| "no URLs".into())
    );
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

pub fn cmd_resource_list(module: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    let manifest = fetch_manifest()?;

    match module {
        None => {
            // List all modules
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
            let cat = manifest
                .resouces
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| anyhow::anyhow!("Module '{}' not found", name))?;

            match format {
                OutputFormat::Text => {
                    println!("{} — {}", cat.name, cat.desc.as_deref().unwrap_or(""));
                    for child in &cat.childrens {
                        println!("\n  {} — {}", child.name, child.desc.as_deref().unwrap_or(""));
                        for ver in &child.versions {
                            println!("    {} — {}", ver.name, ver.desc.as_deref().unwrap_or(""));
                            for raw in &ver.files {
                                if let Some(entry) = parse_file_entry(raw) {
                                    println!("      {}  {}  {}", entry.desc, entry.filename, format_size(entry.size));
                                }
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
                                        .files
                                        .iter()
                                        .filter_map(|raw| {
                                            parse_file_entry(raw).map(|e| {
                                                serde_json::json!({
                                                    "desc": e.desc,
                                                    "filename": e.filename,
                                                    "sha256": e.sha256,
                                                    "size": e.size,
                                                    "path": e.path,
                                                })
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
    use sha2::Digest;

    let manifest = fetch_manifest()?;

    let cat = manifest
        .resouces
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(module))
        .ok_or_else(|| anyhow::anyhow!("Module '{}' not found", module))?;

    // Collect all matching files
    let mut files_to_download: Vec<FileEntry> = Vec::new();

    for child in &cat.childrens {
        for ver in &child.versions {
            if let Some(filter) = version_filter {
                if !ver.name.contains(filter) {
                    continue;
                }
            }
            for raw in &ver.files {
                if let Some(entry) = parse_file_entry(raw) {
                    files_to_download.push(entry);
                }
            }
            // If no version filter, only take the first (latest) version per child
            if version_filter.is_none() {
                break;
            }
        }
    }

    if files_to_download.is_empty() {
        anyhow::bail!("No files found for module '{}' with version filter {:?}", module, version_filter);
    }

    // Sort mirrors by speed (descending)
    let mut mirrors = manifest.mirrors.clone();
    mirrors.sort_by(|a, b| b.speed.unwrap_or(0).cmp(&a.speed.unwrap_or(0)));

    let out_path = std::path::Path::new(output);
    std::fs::create_dir_all(out_path)?;

    let mut downloaded = 0u32;
    let mut failed = 0u32;

    for entry in &files_to_download {
        let dest = out_path.join(&entry.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if format == &OutputFormat::Text {
            eprintln!("Downloading {} ({})...", entry.filename, format_size(entry.size));
        }

        let mut success = false;
        for mirror in &mirrors {
            let url = format!("{}{}", mirror.url, entry.path);
            match download_file(&url, &dest, entry.size) {
                Ok(()) => {
                    // Verify SHA256
                    let data = std::fs::read(&dest)?;
                    let hash = sha2::Sha256::digest(&data);
                    let hex = format!("{:X}", hash);
                    if hex.eq_ignore_ascii_case(&entry.sha256) {
                        if format == &OutputFormat::Text {
                            eprintln!("  ✓ SHA256 verified: {}", dest.display());
                        }
                        downloaded += 1;
                        success = true;
                        break;
                    } else {
                        eprintln!("  ✗ SHA256 mismatch for {} (expected {}, got {})", entry.filename, entry.sha256, hex);
                        let _ = std::fs::remove_file(&dest);
                    }
                }
                Err(e) => {
                    log::warn!("  Mirror {} failed: {e}", mirror.url);
                }
            }
        }
        if !success {
            eprintln!("  ✗ Failed to download {}", entry.filename);
            failed += 1;
        }
    }

    match format {
        OutputFormat::Text => {
            println!("\nDownload complete: {} succeeded, {} failed", downloaded, failed);
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": if failed == 0 { "ok" } else { "partial" },
                "command": "resource.download",
                "data": {
                    "module": module,
                    "downloaded": downloaded,
                    "failed": failed,
                    "output": output,
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }

    if failed > 0 {
        anyhow::bail!("{failed} file(s) failed to download");
    }
    Ok(())
}

fn download_file(url: &str, dest: &std::path::Path, size: u64) -> anyhow::Result<()> {
    use std::io::Read;

    let resp = ureq::get(url).call()?;
    let mut reader = resp.into_reader();

    let pb = indicatif::ProgressBar::new(size);
    pb.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("  [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar())
            .progress_chars("##-"),
    );

    let mut file = std::fs::File::create(dest)?;
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n])?;
        pb.inc(n as u64);
    }
    pb.finish_and_clear();
    Ok(())
}
