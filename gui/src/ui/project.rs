use crate::config::GuiConfig;
use egui::{Color32, RichText, ProgressBar};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crossbeam_channel::{unbounded, Receiver};
use luatos_flash::FlashProgress;

pub struct ProjectState {
    pub selected_port: Option<String>,
    pub logs: Vec<String>, // Now only used as a buffer to pass logs to app.rs
    
    // Project loaded data
    pub loaded_project: Option<luatos_project::Project>,
    pub current_project_path: Option<String>,

    pub flash_rx: Option<Receiver<FlashProgress>>,
    pub flash_cancel: Option<Arc<AtomicBool>>,
    pub is_flashing: bool,
    pub flash_percent: f32,
    pub flash_stage: String,
}

impl ProjectState {
    pub fn new(config: &GuiConfig) -> Self {
        let mut s = Self {
            selected_port: config.last_port.clone(),
            logs: Vec::new(),
            loaded_project: None,
            current_project_path: None,
            flash_rx: None,
            flash_cancel: None,
            is_flashing: false,
            flash_percent: 0.0,
            flash_stage: String::new(),
        };
        // Load the currently selected project if any
        if let Some(ref path) = config.current_project {
            s.load_project(path.clone());
        }
        s
    }

    fn load_project(&mut self, path: String) {
        let pb = std::path::PathBuf::from(&path);
        match luatos_project::Project::load(&pb) {
            Ok(p) => {
                self.loaded_project = Some(p);
                self.current_project_path = Some(path);
                self.logs.push(format!("Project loaded: {:?}", pb.display()));
            }
            Err(e) => {
                self.logs.push(format!("Failed to load project at {:?}: {}", pb.display(), e));
                self.loaded_project = None;
                self.current_project_path = None;
            }
        }
    }

    pub fn show_sidebar(&mut self, ui: &mut egui::Ui, config: &mut GuiConfig) {
        if config.projects.is_empty() {
            ui.label(RichText::new("暂无项目 (No projects)").color(Color32::GRAY));
        } else {
            let mut selected_to_load = None;
            for proj_path in &config.projects {
                let folder_name = std::path::Path::new(proj_path)
                    .file_name().unwrap_or_default().to_string_lossy();
                let is_selected = config.current_project.as_ref() == Some(proj_path);
                
                // Use a larger button-like selectable label
                ui.add_space(4.0);
                if ui.selectable_label(is_selected, RichText::new(format!("📁 {}", folder_name)).size(15.0))
                    .on_hover_text(proj_path).clicked() {
                    selected_to_load = Some(proj_path.clone());
                }
            }
            if let Some(path) = selected_to_load {
                config.current_project = Some(path.clone());
                self.load_project(path);
            }
        }
        ui.add_space(15.0);
        if ui.button(RichText::new("➕ 导入已有项目").strong()).clicked() {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                let path_str = folder.to_string_lossy().to_string();
                if luatos_project::Project::config_file(&folder).exists() {
                    if !config.projects.contains(&path_str) {
                        config.projects.push(path_str.clone());
                    }
                    config.current_project = Some(path_str.clone());
                    self.load_project(path_str);
                } else {
                    self.logs.push(format!("No luatos-project.toml found in {:?}", folder));
                }
            }
        }
    }

    pub fn show_main(&mut self, ui: &mut egui::Ui, _config: &mut GuiConfig, ctx: &egui::Context) {
        // Drain flash progress channel
        if let Some(rx) = &self.flash_rx {
            while let Ok(msg) = rx.try_recv() {
                self.flash_percent = msg.percent;
                self.flash_stage = msg.stage.clone();
                if !msg.message.is_empty() {
                    self.logs.push(msg.message);
                }
                if msg.done {
                    self.is_flashing = false;
                    self.flash_cancel = None;
                    self.flash_rx = None;
                    break;
                }
                ctx.request_repaint();
            }
        }

        if let Some(proj) = self.loaded_project.clone() {
            ui.heading(RichText::new(format!("⚙ {}", proj.project.name)).size(24.0).color(Color32::WHITE));
            ui.add_space(8.0);
            
            ui.horizontal(|ui| {
                ui.label(RichText::new("架构:").color(Color32::GRAY));
                ui.label(RichText::new(&proj.project.chip).color(Color32::LIGHT_BLUE));
                ui.add_space(10.0);
                ui.label(RichText::new("版本:").color(Color32::GRAY));
                ui.label(&proj.project.version);
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("脚本目录:").color(Color32::GRAY));
                ui.label(RichText::new(format!("{:?}", proj.build.script_dirs)).monospace());
            });
            ui.add_space(16.0);
            
            egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.heading(RichText::new("⚡ 烧录控制").size(18.0).color(Color32::LIGHT_GREEN));
                ui.add_space(8.0);
                
                if self.is_flashing {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("正在烧录 ({})...", self.flash_stage)).color(Color32::YELLOW).strong());
                        if ui.button("⏹ 取消任务").clicked() {
                            if let Some(cancel) = &self.flash_cancel {
                                cancel.store(true, Ordering::Relaxed);
                                self.logs.push("Cancelling...".to_string());
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.add(ProgressBar::new(self.flash_percent / 100.0).show_percentage().animate(true));
                } else {
                    let can_flash = self.selected_port.is_some() && proj.flash.soc_file.is_some();
                    
                    if !can_flash {
                        ui.label(RichText::new("⚠ 请先在下方终端选择串口，并确保项目已配置 SOC 固件路径。").color(Color32::KHAKI));
                        ui.add_space(8.0);
                    }
                    
                    ui.horizontal(|ui| {
                        if ui.add_sized([180.0, 45.0], egui::Button::new(RichText::new("🚀 刷入全量 (SOC+脚本)").size(16.0).strong()))
                             .on_hover_text("同时也刷入 Bootloader 和底层固件")
                             .clicked() {
                            if can_flash {
                                self.start_flash(&proj.project.chip, proj.flash.soc_file.as_deref().unwrap(), proj.flash.baud, &proj.build.script_dirs, "full");
                            }
                        }
                        ui.add_space(12.0);
                        if ui.add_sized([180.0, 45.0], egui::Button::new(RichText::new("📄 仅更新脚本").size(16.0).strong()))
                             .on_hover_text("只下载 Lua 文件，速度极快")
                             .clicked() {
                            if can_flash {
                                self.start_flash(&proj.project.chip, proj.flash.soc_file.as_deref().unwrap(), proj.flash.baud, &proj.build.script_dirs, "script");
                            }
                        }
                    });
                }
            });
            ui.add_space(10.0);
            ui.label(RichText::new("提示: 刷机过程中的详细进度将在下方终端中显示。").color(Color32::DARK_GRAY).italics());
        } else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("👈 暂无选中项目，请在左侧点击或导入").size(20.0).color(Color32::DARK_GRAY));
            });
        }
    }

    fn start_flash(&mut self, chip: &str, soc_file: &str, baud: Option<u32>, script_dirs: &[String], mode: &str) {
        self.logs.push(format!("Starting {} flash on {}...", mode, self.selected_port.as_ref().unwrap()));
        self.is_flashing = true;
        self.flash_percent = 0.0;
        self.flash_stage = "Init".into();
        
        let (tx, rx) = unbounded();
        self.flash_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.flash_cancel = Some(cancel.clone());
        
        let port = self.selected_port.clone().unwrap();
        let soc_file = soc_file.to_string();
        let proj_root = self.current_project_path.clone().unwrap();
        let folders: Vec<String> = script_dirs.iter()
            .map(|d| std::path::Path::new(&proj_root).join(d).to_string_lossy().to_string())
            .collect();
            
        let chip = chip.to_string();
        let mode = mode.to_string();

        std::thread::spawn(move || {
            let tx_clone = tx.clone();
            let on_progress: luatos_flash::ProgressCallback = Box::new(move |p| {
                let _ = tx_clone.send(p.clone());
            });

            let folder_refs: Vec<&str> = folders.iter().map(|s| s.as_str()).collect();

            let res = match chip.as_str() {
                "bk72xx" | "air8101" | "air8000" => {
                    if mode == "full" {
                        luatos_flash::bk7258::flash_bk7258(&soc_file, Some(&folder_refs), &port, baud, cancel.clone(), on_progress).map(|_| ())
                    } else {
                        luatos_flash::bk7258::flash_script_only(&soc_file, &folder_refs, &port, cancel.clone(), on_progress)
                    }
                }
                "air6208" | "air101" | "air103" | "air601" => {
                    if mode == "full" {
                        luatos_flash::xt804::flash_xt804(&soc_file, &port, on_progress, cancel.clone())
                    } else {
                        on_progress(&FlashProgress::done_err("Script direct update requires files logic porting."));
                        Ok(())
                    }
                }
                _ => {
                    on_progress(&FlashProgress::done_err(&format!("Unsupported chip {}", chip)));
                    Ok(())
                }
            };
            
            if let Err(e) = res {
                let _ = tx.send(FlashProgress::done_err(&e.to_string()));
            } else {
                let _ = tx.send(FlashProgress::done_ok("Flash completed successfully."));
            }
        });
    }
}
