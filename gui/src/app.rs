use crate::config::GuiConfig;
use crate::ui;
use egui::Color32;
use std::time::{Instant, Duration};

#[derive(PartialEq)]
pub enum Tab {
    Project,
    Resource,
    Upgrade,
}

pub struct LuatOsGui {
    pub config: GuiConfig,
    pub active_tab: Tab,
    pub project_state: ui::project::ProjectState,
    pub log_state: ui::log_viewer::LogState,
    pub resource_state: ui::resource::ResourceState,
    pub upgrade_state: ui::upgrade::UpgradeState,

    // Global Serial Port Cache
    pub ports: Vec<String>,
    pub last_port_refresh: Instant,
    pub was_flashing: bool,
}

impl LuatOsGui {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let config = GuiConfig::load();
        Self {
            project_state: ui::project::ProjectState::new(&config),
            log_state: ui::log_viewer::LogState::new(),
            resource_state: ui::resource::ResourceState::default(),
            upgrade_state: ui::upgrade::UpgradeState::default(),
            active_tab: Tab::Project,
            config,
            ports: Vec::new(),
            last_port_refresh: Instant::now() - Duration::from_secs(10),
            was_flashing: false,
        }
    }

    fn refresh_ports(&mut self) {
        if self.last_port_refresh.elapsed() > Duration::from_secs(2) {
            let info_ports = luatos_serial::list_ports();
            self.ports = info_ports.into_iter().map(|p| p.port_name).collect();
            self.last_port_refresh = Instant::now();
        }
    }
}

impl eframe::App for LuatOsGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh_ports();

        // Handle Auto-Connect / Auto-Disconnect Logic
        let is_flashing = self.project_state.is_flashing;
        if is_flashing && !self.was_flashing {
            // Flash started
            self.log_state.stop_logging();
            self.log_state.add_log(Color32::LIGHT_BLUE, "⚡ 刷机任务启动，已暂时断开串口日志连。".into());
        }
        
        // Sync logs from project_state (flashing) to log_state (terminal)
        // Drain project_state.logs into log_state.logs
        while !self.project_state.logs.is_empty() {
            let log = self.project_state.logs.remove(0);
            self.log_state.add_log(Color32::WHITE, format!("FLASH: {}", log));
        }

        if !is_flashing && self.was_flashing {
            // Flash ended, try auto-reconnect
            self.log_state.add_log(Color32::LIGHT_GREEN, "✅ 刷机任务完成。".into());
            if let Some(port) = &self.config.last_port {
                 self.log_state.start_logging(port.clone(), self.log_state.baud_rate);
            }
        }
        self.was_flashing = is_flashing;

        // Sync Global Port selection
        // project_state.selected_port should track config.last_port
        self.project_state.selected_port = self.config.last_port.clone();

        // Left Sidebar
        egui::SidePanel::left("left_sidebar")
            .resizable(true)
            .min_width(220.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.heading(egui::RichText::new("📦 LuatOS 工具箱").size(22.0).color(Color32::WHITE));
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                
                ui.vertical_centered_justified(|ui| {
                    ui.selectable_value(&mut self.active_tab, Tab::Project, egui::RichText::new("📁 我的项目").size(16.0));
                    ui.selectable_value(&mut self.active_tab, Tab::Resource, egui::RichText::new("🌐 云资源库").size(16.0));
                    ui.selectable_value(&mut self.active_tab, Tab::Upgrade, egui::RichText::new("🔧 全局设置").size(16.0));
                });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                match self.active_tab {
                    Tab::Project => self.project_state.show_sidebar(ui, &mut self.config),
                    Tab::Resource => {
                        ui.label(egui::RichText::new("从云端获取最新的固件程序和脚本包。").color(Color32::GRAY));
                    }
                    _ => {}
                }
            });

        // Bottom Global Terminal
        egui::TopBottomPanel::bottom("log_terminal")
            .resizable(true)
            .min_height(250.0)
            .show(ctx, |ui| {
                self.log_state.show(ui, ctx, &self.ports, &mut self.config.last_port);
            });

        // Main Area
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                Tab::Project => self.project_state.show_main(ui, &mut self.config, ctx),
                Tab::Resource => self.resource_state.show(ui, ctx),
                Tab::Upgrade => self.upgrade_state.show(ui, ctx),
            }
        });
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.config.save();
    }
}
