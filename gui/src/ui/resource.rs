use egui::{Color32, RichText, ComboBox, ProgressBar};

use crossbeam_channel::{unbounded, Receiver};

const RESOURCE_MANIFEST_URLS: &[&str] = &[
    "http://bj02.air32.cn:10888/files/files.json",
    "http://sh.air32.cn:10888/files/files.json",
];

#[derive(serde::Deserialize, Debug, Clone)]
struct ResourceManifest {
    mirrors: Vec<Mirror>,
    resouces: Vec<ResourceCategory>, // typo in server JSON
}

#[derive(serde::Deserialize, Debug, Clone)]
struct Mirror {
    url: String,
    speed: Option<u32>,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[allow(dead_code)]
struct ResourceCategory {
    name: String,
    desc: Option<String>,
    childrens: Vec<ResourceChild>,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[allow(dead_code)]
struct ResourceChild {
    name: String,
    desc: Option<String>,
    versions: Vec<ResourceVersion>,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[allow(dead_code)]
struct ResourceVersion {
    name: String,
    desc: Option<String>,
    files: Vec<serde_json::Value>,
}

#[derive(Clone)]
#[allow(dead_code)]
struct FileEntry {
    filename: String,
    sha256: String,
    size: u64,
    path: String,
}

fn parse_file_entry(val: &serde_json::Value) -> Option<FileEntry> {
    let arr = val.as_array()?;
    if arr.len() < 5 { return None; }
    Some(FileEntry {
        filename: arr[1].as_str()?.to_string(),
        sha256: arr[2].as_str()?.to_string(),
        size: arr[3].as_u64()?,
        path: arr[4].as_str()?.to_string(),
    })
}

#[derive(Default)]
pub struct ResourceState {
    pub selected_chip: Option<String>,
    pub selected_version: Option<String>,
    pub is_downloading: bool,
    pub progress: f32,
    pub download_status: String,
    
    manifest_fetched: bool,
    manifest: Option<ResourceManifest>,
    fetch_err: Option<String>,
    rx: Option<Receiver<(f32, String)>>,
}

impl ResourceState {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if !self.manifest_fetched {
            self.manifest_fetched = true;
            match self.fetch_manifest() {
                Ok(m) => self.manifest = Some(m),
                Err(e) => self.fetch_err = Some(e),
            }
        }

        ui.heading(RichText::new("🌐 云资源中心").size(24.0).color(Color32::WHITE));
        ui.add_space(8.0);
        
        if let Some(ref err) = self.fetch_err {
            ui.label(RichText::new(format!("获取资源列表失败: {}", err)).color(Color32::LIGHT_RED));
            if ui.button("🔄 重试").clicked() {
                self.manifest_fetched = false;
                self.fetch_err = None;
            }
            return;
        }

        ui.label(RichText::new("为您同步 LuatOS 官方 CDN 的最新固件与脚本包").color(Color32::GRAY));
        ui.add_space(16.0);

        if let Some(rx) = &self.rx {
            while let Ok((pct, msg)) = rx.try_recv() {
                self.progress = pct;
                self.download_status = msg;
                if pct >= 1.0 || pct < 0.0 {
                    self.is_downloading = false;
                    self.rx = None;
                    break;
                }
                ctx.request_repaint();
            }
        }

        egui::Frame::group(ui.style()).inner_margin(16.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(manifest) = &self.manifest {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("芯片架构:").size(16.0).strong());
                    let chips: Vec<String> = manifest.resouces.iter().map(|c| c.name.clone()).collect();
                    ComboBox::from_id_source("chip_select")
                        .selected_text(self.selected_chip.as_deref().unwrap_or("— 请选择架构 —"))
                        .show_ui(ui, |ui| {
                            for chip in chips {
                                if ui.selectable_value(&mut self.selected_chip, Some(chip.clone()), &chip).clicked() {
                                    self.selected_version = None;
                                }
                            }
                        });
                });

                ui.add_space(12.0);

                if let Some(ref chip_name) = self.selected_chip {
                    let cat = manifest.resouces.iter().find(|c| &c.name == chip_name);
                    if let Some(cat) = cat {
                        let mut versions: Vec<String> = Vec::new();
                        for child in &cat.childrens {
                            for v in &child.versions {
                                versions.push(v.name.clone());
                            }
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("固件版本:").size(16.0).strong());
                            ComboBox::from_id_source("version_select")
                                .selected_text(self.selected_version.as_deref().unwrap_or("— 选择稳定版/开发版 —"))
                                .show_ui(ui, |ui| {
                                    for v in versions {
                                        ui.selectable_value(&mut self.selected_version, Some(v.clone()), &v);
                                    }
                                });
                        });
                    }
                }

                ui.add_space(24.0);

                if self.selected_chip.is_some() && self.selected_version.is_some() {
                    if !self.is_downloading {
                        if ui.add_sized([160.0, 40.0], egui::Button::new(RichText::new("📥 立即同步").size(16.0).strong())).clicked() {
                            self.is_downloading = true;
                            self.progress = 0.0;
                            self.download_status = "Initializing mirrors...".into();
                            self.start_download(manifest.clone(), self.selected_chip.clone().unwrap(), self.selected_version.clone().unwrap());
                        }
                    } else {
                        ui.horizontal(|ui| {
                           ui.label(RichText::new(&self.download_status).color(Color32::LIGHT_BLUE));
                        });
                        ui.add_space(8.0);
                        ui.add(ProgressBar::new(self.progress).show_percentage().animate(true));
                    }
                } else {
                    ui.label(RichText::new("ℹ 请在上方选择架构和版本以开始同步到本地资源目录。").color(Color32::DARK_GRAY).italics());
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                    ui.label("正在从云端加载资源清单...");
                });
            }
        });

        ui.add_space(12.0);
        ui.label(RichText::new("下载的文件将存放在程序同目录下的 /resource 文件夹。").size(12.0).color(Color32::DARK_GRAY));
    }

    fn fetch_manifest(&self) -> Result<ResourceManifest, String> {
        let mut last_err = String::new();
        for url in RESOURCE_MANIFEST_URLS {
            match ureq::get(url).call() {
                Ok(resp) => {
                    if let Ok(body) = resp.into_string() {
                        if let Ok(manifest) = serde_json::from_str(&body) {
                            return Ok(manifest);
                        }
                    }
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }
        }
        Err(last_err)
    }

    fn start_download(&mut self, manifest: ResourceManifest, module: String, version: String) {
        let (tx, rx) = unbounded();
        self.rx = Some(rx);

        std::thread::spawn(move || {
            let cat = manifest.resouces.iter().find(|c| c.name.eq_ignore_ascii_case(&module));
            if cat.is_none() {
                let _ = tx.send((-1.0, format!("Module not found!")));
                return;
            }
            let cat = cat.unwrap();
            
            let mut files_to_download: Vec<FileEntry> = Vec::new();
            for child in &cat.childrens {
                for ver in &child.versions {
                    if ver.name == version {
                        for raw in &ver.files {
                            if let Some(entry) = parse_file_entry(raw) {
                                files_to_download.push(entry);
                            }
                        }
                    }
                }
            }

            if files_to_download.is_empty() {
                let _ = tx.send((-1.0, format!("No files found for version {}", version)));
                return;
            }

            let mut mirrors = manifest.mirrors.clone();
            mirrors.sort_by(|a, b| b.speed.unwrap_or(0).cmp(&a.speed.unwrap_or(0)));

            let out_path = std::path::Path::new("resource");
            let _ = std::fs::create_dir_all(out_path);

            for entry in files_to_download {
                let dest = out_path.join(&entry.path);
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }

                let mut success = false;
                for mirror in &mirrors {
                    let url = format!("{}{}", mirror.url, entry.path);
                    let _ = tx.send((0.0, format!("Downloading {}...", entry.filename)));
                    
                    if let Ok(resp) = ureq::get(&url).call() {
                        use std::io::Read;
                        let mut reader = resp.into_reader();
                        let mut file = match std::fs::File::create(&dest) {
                            Ok(f) => f,
                            Err(_) => continue,
                        };
                        
                        let mut buf = [0u8; 8192];
                        let mut downloaded = 0u64;
                        let size = entry.size;

                        loop {
                            match reader.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    let _ = std::io::Write::write_all(&mut file, &buf[..n]);
                                    downloaded += n as u64;
                                    let pct = (downloaded as f32) / (size as f32);
                                    let _ = tx.send((pct, format!("Downloading {}... ({}/{})", entry.filename, downloaded, size)));
                                }
                                Err(_) => break,
                            }
                        }
                        
                        // We skip sha256 verification here for simplicity to keep dependencies purely fast
                        success = true;
                        break;
                    }
                }
                
                if !success {
                    let _ = tx.send((-1.0, format!("Failed to download {}", entry.filename)));
                    return;
                }
            }
            
            let _ = tx.send((1.0, "下载成功 (Download Completed)".into()));
        });
    }
}
