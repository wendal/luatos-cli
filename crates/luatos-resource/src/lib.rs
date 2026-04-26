//! LuatOS 固件资源清单获取与下载
//!
//! 从 CDN 获取资源清单（files.json），支持按模组/版本筛选，
//! 下载固件文件并进行 SHA256 校验。
//!
//! CLI 和 GUI 共用此 crate 的 API，各自负责渲染/格式化输出。

use std::cmp::Reverse;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;

/// CDN 清单地址列表（按优先级排序）
pub const MANIFEST_URLS: &[&str] = &["http://bj02.air32.cn:10888/files/files.json", "http://sh.air32.cn:10888/files/files.json"];

// ─── 数据结构 ──────────────────────────────────────────

/// 资源清单根结构
#[derive(Debug, Deserialize)]
pub struct ResourceManifest {
    #[allow(dead_code)]
    pub version: u32,
    pub mirrors: Vec<Mirror>,
    /// 注意：服务端 JSON 字段名是 "resouces"（拼写错误），保持兼容
    pub resouces: Vec<ResourceCategory>,
}

/// CDN 镜像
#[derive(Debug, Clone, Deserialize)]
pub struct Mirror {
    pub url: String,
    pub speed: Option<u32>,
}

/// 资源分类（顶层，如 "Air780E"）
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ResourceCategory {
    pub name: String,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub url: Option<String>,
    #[serde(default)]
    pub childrens: Vec<ResourceChild>,
}

/// 分类下的子项（如 "core" / "demo"）
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ResourceChild {
    pub name: String,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub versions: Vec<ResourceVersion>,
}

/// 版本（如 "V1008"）
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ResourceVersion {
    pub name: String,
    #[serde(default)]
    pub desc: Option<String>,
    /// 原始 JSON 数组，使用 [`parse_file_entry`] 解析
    #[serde(default)]
    files: Vec<serde_json::Value>,
}

impl ResourceVersion {
    /// 解析版本下的所有文件条目
    pub fn file_entries(&self) -> Vec<FileEntry> {
        self.files.iter().filter_map(parse_file_entry).collect()
    }
}

/// 文件条目（从 JSON 数组解析得到）
#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub desc: String,
    pub filename: String,
    pub sha256: String,
    pub size: u64,
    pub path: String,
}

/// 从原始 JSON 数组解析文件条目
/// 格式: `["描述", "文件名", "sha256", 大小, "路径"]`
pub fn parse_file_entry(val: &serde_json::Value) -> Option<FileEntry> {
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

// ─── 下载进度事件 ──────────────────────────────────────

/// 下载进度事件（调用方通过回调接收）
#[derive(Debug, Clone)]
pub enum DownloadEvent {
    /// 开始下载文件
    StartFile { filename: String, size: u64, index: usize, total: usize },
    /// 下载进度（字节数）
    Progress { filename: String, downloaded: u64, total: u64 },
    /// 文件 SHA256 校验通过
    Verified { filename: String, dest: String },
    /// SHA256 校验失败
    HashMismatch { filename: String, expected: String, actual: String },
    /// 单个镜像下载失败，尝试下一个
    MirrorFailed { mirror_url: String, filename: String, error: String },
    /// 文件下载完全失败（所有镜像都失败）
    FileFailed { filename: String },
    /// zip 文件已解压到目标目录
    Extracted { filename: String, dest_dir: String },
}

/// 下载进度回调类型
pub type DownloadCallback = Box<dyn Fn(&DownloadEvent) + Send>;

/// 下载结果报告（每个文件的结果）
#[derive(Debug, Clone, Serialize)]
pub struct FileResult {
    pub filename: String,
    pub path: String,
    pub success: bool,
    pub dest: Option<String>,
    pub error: Option<String>,
}

/// 批量下载报告
#[derive(Debug, Clone, Serialize)]
pub struct DownloadReport {
    pub module: String,
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub files: Vec<FileResult>,
}

// ─── API 函数 ──────────────────────────────────────────

/// manifest 缓存有效期（秒）。可在源码中修改此常量调整缓存时长。
pub const MANIFEST_CACHE_TTL_SECS: u64 = 300; // 5 分钟

/// 从 CDN 获取资源清单，自动尝试多个镜像
pub fn fetch_manifest() -> Result<ResourceManifest> {
    fetch_manifest_from(MANIFEST_URLS)
}

/// 从指定 URL 列表获取资源清单（便于测试）
pub fn fetch_manifest_from(urls: &[&str]) -> Result<ResourceManifest> {
    let body = fetch_manifest_raw_from(urls)?;
    serde_json::from_str(&body).context("解析资源清单 JSON 失败")
}

/// 获取资源清单（带本地缓存）。
///
/// 若 `cache_path` 文件存在且修改时间在 [`MANIFEST_CACHE_TTL_SECS`] 秒内，
/// 直接读取缓存；否则从 CDN 拉取并覆写缓存文件。
///
/// 推荐缓存路径：`~/.luatos/manifest_cache.json`。
pub fn fetch_manifest_with_cache(cache_path: &Path) -> Result<ResourceManifest> {
    if is_cache_fresh(cache_path, MANIFEST_CACHE_TTL_SECS) {
        if let Ok(content) = std::fs::read_to_string(cache_path) {
            match serde_json::from_str::<ResourceManifest>(&content) {
                Ok(m) => {
                    log::debug!("使用缓存资源清单: {}", cache_path.display());
                    return Ok(m);
                }
                Err(e) => log::warn!("缓存清单解析失败，重新拉取: {e}"),
            }
        }
    }

    let body = fetch_manifest_raw_from(MANIFEST_URLS)?;

    // 写入缓存（写入失败不影响本次使用）
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(cache_path, &body) {
        log::warn!("写入清单缓存失败: {e}");
    }

    serde_json::from_str(&body).context("解析资源清单 JSON 失败")
}

/// 从 CDN URL 列表获取清单原始 JSON 字符串（内部使用）
fn fetch_manifest_raw_from(urls: &[&str]) -> Result<String> {
    let mut last_err = None;
    for url in urls {
        match ureq::get(url).call() {
            Ok(resp) => return resp.into_string().context("读取清单响应体失败"),
            Err(e) => {
                log::warn!("获取清单失败 {url}: {e}");
                last_err = Some(e);
            }
        }
    }
    bail!("所有清单地址均失败: {}", last_err.map(|e| e.to_string()).unwrap_or_else(|| "无 URL".into()))
}

/// 判断缓存文件是否在 `ttl_secs` 秒内修改过
fn is_cache_fresh(path: &Path, ttl_secs: u64) -> bool {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|e| e.as_secs() < ttl_secs)
        .unwrap_or(false)
}

/// 查找指定模组的资源分类
pub fn find_category<'a>(manifest: &'a ResourceManifest, module: &str) -> Option<&'a ResourceCategory> {
    manifest.resouces.iter().find(|c| c.name.eq_ignore_ascii_case(module))
}

/// 查找分类下的指定子项（大小写不敏感）
pub fn find_child<'a>(category: &'a ResourceCategory, child_name: &str) -> Option<&'a ResourceChild> {
    category.childrens.iter().find(|c| c.name.eq_ignore_ascii_case(child_name))
}

/// 从单个子项收集待下载文件
///
/// - `version_filter` 为 None → 只取第一个（最新）版本
/// - `version_filter` 为 Some(v) → 取所有名称 contains v 的版本
pub fn collect_files_for_child(child: &ResourceChild, version_filter: Option<&str>) -> Vec<FileEntry> {
    let mut files = Vec::new();
    for ver in &child.versions {
        if let Some(filter) = version_filter {
            if !ver.name.contains(filter) {
                continue;
            }
        }
        files.extend(ver.file_entries());
        if version_filter.is_none() {
            break;
        }
    }
    files
}

/// 跨子项按版本名 + 文件名筛选文件
///
/// 遍历分类下所有子项，收集版本名中包含 `version_filter` 的版本文件。
/// 若指定了 `file_filter`，则只保留文件名中包含该字符串的文件。
pub fn collect_files_for_version(category: &ResourceCategory, version_filter: &str, file_filter: Option<&str>) -> Vec<FileEntry> {
    let mut files = Vec::new();
    for child in &category.childrens {
        for ver in &child.versions {
            if !ver.name.contains(version_filter) {
                continue;
            }
            for entry in ver.file_entries() {
                if let Some(ff) = file_filter {
                    if !entry.filename.contains(ff) {
                        continue;
                    }
                }
                files.push(entry);
            }
        }
    }
    files
}

/// 收集待下载的文件列表（兼容旧接口，内部使用）
///
/// 如果指定了 `version_filter`，只下载名称中包含该字符串的版本。
/// 如果未指定版本过滤，每个子分类只取第一个（最新）版本。
pub fn collect_files(category: &ResourceCategory, version_filter: Option<&str>) -> Vec<FileEntry> {
    let mut files = Vec::new();
    for child in &category.childrens {
        files.extend(collect_files_for_child(child, version_filter));
    }
    files
}

/// 下载文件到指定目录，支持 SHA256 校验和进度回调
///
/// `mirrors` 按优先级排序，依次尝试直到成功。
pub fn download_files(module: &str, files: &[FileEntry], mirrors: &[Mirror], output_dir: &Path, on_event: Option<&DownloadCallback>) -> Result<DownloadReport> {
    std::fs::create_dir_all(output_dir)?;

    let mut results = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    // 按速度降序排列镜像
    let mut sorted_mirrors = mirrors.to_vec();
    sorted_mirrors.sort_by_key(|b| Reverse(b.speed.unwrap_or(0)));

    for (idx, entry) in files.iter().enumerate() {
        let dest = output_dir.join(&entry.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if let Some(cb) = &on_event {
            cb(&DownloadEvent::StartFile {
                filename: entry.filename.clone(),
                size: entry.size,
                index: idx,
                total: files.len(),
            });
        }

        let mut success = false;
        for mirror in &sorted_mirrors {
            let url = format!("{}{}", mirror.url, entry.path);
            match download_single_file(&url, &dest, entry.size, &entry.filename, on_event) {
                Ok(()) => {
                    // SHA256 校验
                    let data = std::fs::read(&dest)?;
                    let hash = sha2::Sha256::digest(&data);
                    let hex = format!("{:X}", hash);
                    if hex.eq_ignore_ascii_case(&entry.sha256) {
                        if let Some(cb) = &on_event {
                            cb(&DownloadEvent::Verified {
                                filename: entry.filename.clone(),
                                dest: dest.display().to_string(),
                            });
                        }
                        // zip 文件自动解压到同级的 {stem}/ 子目录
                        if entry.filename.ends_with(".zip") {
                            match extract_zip_to_stem_dir(&dest) {
                                Ok(extract_dir) => {
                                    if let Some(cb) = &on_event {
                                        cb(&DownloadEvent::Extracted {
                                            filename: entry.filename.clone(),
                                            dest_dir: extract_dir.display().to_string(),
                                        });
                                    }
                                }
                                Err(e) => {
                                    log::warn!("解压 {} 失败: {e}", entry.filename);
                                }
                            }
                        }
                        results.push(FileResult {
                            filename: entry.filename.clone(),
                            path: entry.path.clone(),
                            success: true,
                            dest: Some(dest.display().to_string()),
                            error: None,
                        });
                        succeeded += 1;
                        success = true;
                        break;
                    } else {
                        if let Some(cb) = &on_event {
                            cb(&DownloadEvent::HashMismatch {
                                filename: entry.filename.clone(),
                                expected: entry.sha256.clone(),
                                actual: hex.clone(),
                            });
                        }
                        let _ = std::fs::remove_file(&dest);
                    }
                }
                Err(e) => {
                    if let Some(cb) = &on_event {
                        cb(&DownloadEvent::MirrorFailed {
                            mirror_url: mirror.url.clone(),
                            filename: entry.filename.clone(),
                            error: e.to_string(),
                        });
                    }
                }
            }
        }
        if !success {
            if let Some(cb) = &on_event {
                cb(&DownloadEvent::FileFailed { filename: entry.filename.clone() });
            }
            results.push(FileResult {
                filename: entry.filename.clone(),
                path: entry.path.clone(),
                success: false,
                dest: None,
                error: Some("所有镜像均失败".into()),
            });
            failed += 1;
        }
    }

    Ok(DownloadReport {
        module: module.to_string(),
        total: files.len(),
        succeeded,
        failed,
        files: results,
    })
}

/// 从单个 URL 下载文件（内部辅助函数）
fn download_single_file(url: &str, dest: &Path, total_size: u64, filename: &str, on_event: Option<&DownloadCallback>) -> Result<()> {
    let resp = ureq::get(url).call()?;
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(dest)?;
    let mut buf = [0u8; 8192];
    let mut downloaded = 0u64;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        downloaded += n as u64;

        if let Some(cb) = &on_event {
            cb(&DownloadEvent::Progress {
                filename: filename.to_string(),
                downloaded,
                total: total_size,
            });
        }
    }
    Ok(())
}

/// 将 zip 文件解压到同级的 `{stem}/` 子目录。
///
/// 例如：`public/soc_script/v2026.04.10.16.zip`
/// 解压到：`public/soc_script/v2026.04.10.16/`，内部结构保持不变。
///
/// 若目标目录已存在，会在其中覆盖写入（已有文件不删除）。
/// 返回解压目标目录路径。
fn extract_zip_to_stem_dir(zip_path: &Path) -> Result<std::path::PathBuf> {
    let stem = zip_path
        .file_stem()
        .and_then(|s| s.to_str())
        .with_context(|| format!("无法获取文件名 stem: {}", zip_path.display()))?;
    let parent = zip_path.parent().with_context(|| format!("无法获取父目录: {}", zip_path.display()))?;
    let dest_dir = parent.join(stem);
    std::fs::create_dir_all(&dest_dir)?;

    let file = std::fs::File::open(zip_path).with_context(|| format!("打开 zip 文件失败: {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).with_context(|| format!("读取 zip 失败: {}", zip_path.display()))?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let entry_name = entry.name().to_owned();
        // 跳过目录条目
        if entry_name.ends_with('/') {
            continue;
        }
        let out_path = dest_dir.join(&entry_name);
        if let Some(p) = out_path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut out_file = std::fs::File::create(&out_path).with_context(|| format!("创建文件失败: {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out_file)?;
    }

    log::debug!("zip 解压完成: {} → {}", zip_path.display(), dest_dir.display());
    Ok(dest_dir)
}

/// 校验本地文件的 SHA256 是否与预期一致
pub fn verify_sha256(path: &Path, expected: &str) -> Result<bool> {
    let data = std::fs::read(path).context("读取文件失败")?;
    let hash = sha2::Sha256::digest(&data);
    let hex = format!("{:X}", hash);
    Ok(hex.eq_ignore_ascii_case(expected))
}

/// 格式化文件大小为人类可读字符串
pub fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

// ─── 测试 ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file_entry_valid() {
        let val = serde_json::json!([
            "底层固件",
            "LuatOS-SoC_V2013_Air8101.soc",
            "ABCDEF1234567890ABCDEF1234567890ABCDEF1234567890ABCDEF1234567890",
            12345678,
            "soc/Air8101/LuatOS-SoC_V2013_Air8101.soc"
        ]);
        let entry = parse_file_entry(&val).expect("解析应成功");
        assert_eq!(entry.desc, "底层固件");
        assert_eq!(entry.filename, "LuatOS-SoC_V2013_Air8101.soc");
        assert_eq!(entry.size, 12345678);
        assert_eq!(entry.path, "soc/Air8101/LuatOS-SoC_V2013_Air8101.soc");
        assert_eq!(entry.sha256.len(), 64);
    }

    #[test]
    fn parse_file_entry_short_array() {
        let val = serde_json::json!(["desc", "filename", "sha256"]);
        assert!(parse_file_entry(&val).is_none());
    }

    #[test]
    fn parse_file_entry_not_array() {
        let val = serde_json::json!("not an array");
        assert!(parse_file_entry(&val).is_none());
    }

    #[test]
    fn parse_file_entry_wrong_types() {
        let val = serde_json::json!([1, 2, 3, "not a number", 5]);
        assert!(parse_file_entry(&val).is_none());
    }

    #[test]
    fn format_size_cases() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(1500), "1.5 KB");
        assert_eq!(format_size(2_500_000), "2.5 MB");
    }

    #[test]
    fn resource_version_file_entries() {
        let ver = ResourceVersion {
            name: "V1000".to_string(),
            desc: None,
            files: vec![
                serde_json::json!(["desc1", "f1.soc", "AAA", 100, "path/f1.soc"]),
                serde_json::json!(["bad"]),
                serde_json::json!(["desc2", "f2.soc", "BBB", 200, "path/f2.soc"]),
            ],
        };
        let entries = ver.file_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].filename, "f1.soc");
        assert_eq!(entries[1].filename, "f2.soc");
    }

    #[test]
    fn collect_files_no_filter() {
        let cat = ResourceCategory {
            name: "TestModule".to_string(),
            desc: None,
            url: None,
            childrens: vec![ResourceChild {
                name: "core".to_string(),
                desc: None,
                versions: vec![
                    ResourceVersion {
                        name: "V2".to_string(),
                        desc: None,
                        files: vec![serde_json::json!(["d", "v2.soc", "H2", 200, "p/v2.soc"])],
                    },
                    ResourceVersion {
                        name: "V1".to_string(),
                        desc: None,
                        files: vec![serde_json::json!(["d", "v1.soc", "H1", 100, "p/v1.soc"])],
                    },
                ],
            }],
        };
        // 无 filter 时只取第一个版本
        let files = collect_files(&cat, None);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "v2.soc");
    }

    #[test]
    fn collect_files_with_filter() {
        let cat = ResourceCategory {
            name: "TestModule".to_string(),
            desc: None,
            url: None,
            childrens: vec![ResourceChild {
                name: "core".to_string(),
                desc: None,
                versions: vec![
                    ResourceVersion {
                        name: "V2".to_string(),
                        desc: None,
                        files: vec![serde_json::json!(["d", "v2.soc", "H2", 200, "p/v2.soc"])],
                    },
                    ResourceVersion {
                        name: "V1".to_string(),
                        desc: None,
                        files: vec![serde_json::json!(["d", "v1.soc", "H1", 100, "p/v1.soc"])],
                    },
                ],
            }],
        };
        // 有 filter 时匹配所有版本
        let files = collect_files(&cat, Some("V1"));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "v1.soc");
    }

    #[test]
    fn verify_sha256_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"hello world").unwrap();
        // SHA256("hello world") = B94D27B9934D3E08A52E52D7DA7DABFAC484EFE37A5380EE9088F7ACE2EFCDE9
        assert!(verify_sha256(&path, "B94D27B9934D3E08A52E52D7DA7DABFAC484EFE37A5380EE9088F7ACE2EFCDE9").unwrap());
        assert!(!verify_sha256(&path, "0000000000000000000000000000000000000000000000000000000000000000").unwrap());
    }

    /// 验证 extract_zip_to_stem_dir 能正确解压到同名子目录
    #[test]
    fn extract_zip_to_stem_dir_basic() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("mylib.zip");

        // 创建一个包含 lib/test.lua 和 lib/sub/util.lua 的 zip
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        writer.start_file("lib/test.lua", opts).unwrap();
        writer.write_all(b"-- test").unwrap();
        writer.start_file("lib/sub/util.lua", opts).unwrap();
        writer.write_all(b"-- util").unwrap();
        writer.finish().unwrap();

        let dest = extract_zip_to_stem_dir(&zip_path).unwrap();
        assert_eq!(dest, dir.path().join("mylib"));
        assert!(dest.join("lib").join("test.lua").exists());
        assert!(dest.join("lib").join("sub").join("util.lua").exists());
    }

    /// 验证 soc_script zip 解压后 lib/ 目录结构正确（模拟 public/soc_script/v2026.zip）
    #[test]
    fn extract_zip_soc_script_structure() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let soc_script_dir = dir.path().join("public").join("soc_script");
        std::fs::create_dir_all(&soc_script_dir).unwrap();
        let zip_path = soc_script_dir.join("v2026.04.10.16.zip");

        let file = std::fs::File::create(&zip_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        writer.start_file("lib/airlbs.lua", opts).unwrap();
        writer.write_all(b"-- airlbs").unwrap();
        writer.finish().unwrap();

        let dest = extract_zip_to_stem_dir(&zip_path).unwrap();
        assert_eq!(dest, soc_script_dir.join("v2026.04.10.16"));
        assert!(dest.join("lib").join("airlbs.lua").exists());
    }
}
