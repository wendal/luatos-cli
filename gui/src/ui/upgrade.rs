use egui::{Color32, RichText};

#[derive(Default)]
pub struct UpgradeState {
    pub has_checked: bool,
    pub is_checking: bool,
}

impl UpgradeState {
    pub fn show(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.heading(RichText::new("⚙ 系统设置与升级").size(24.0).color(Color32::WHITE));
        ui.add_space(8.0);
        
        ui.label(RichText::new("管理 LuatOS CLI GUI 的工具版本与全局偏好。").color(Color32::GRAY));
        ui.add_space(16.0);

        egui::Frame::group(ui.style()).inner_margin(16.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.heading(RichText::new("版本信息").size(18.0).strong());
            ui.add_space(8.0);
            
            ui.horizontal(|ui| {
               ui.label("当前版本:");
               ui.label(RichText::new("v1.1.0-RC").color(Color32::LIGHT_BLUE).strong());
            });
            ui.label("构建标识: luatos-cli-gui-win32-x64");
            
            ui.add_space(20.0);

            if self.is_checking {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在从 GitHub 检查更新...");
                });
            } else {
                if ui.button(RichText::new("🔄 检查更新").strong()).clicked() {
                    self.is_checking = true;
                    self.has_checked = false;
                }
            }

            if self.has_checked && !self.is_checking {
                ui.add_space(12.0);
                ui.label(RichText::new("✨ 您当前使用的是最新版本").color(Color32::LIGHT_GREEN).italics());
            }
        });

        // Mock completion after click
        if self.is_checking {
            self.is_checking = false;
            self.has_checked = true;
        }

        ui.add_space(20.0);
        ui.heading(RichText::new("关于 LuatOS CLI").size(18.0).strong());
        ui.label("LuatOS CLI 是一个用于管理和烧录 LuatOS 设备的跨平台命令行工具与 GUI 封装。");
        ui.hyperlink_to("访问项目主页", "https://github.com/wendal/luatos-cli");
    }
}
