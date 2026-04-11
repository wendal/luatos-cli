#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

mod app;
mod config;
mod ui;

use log::LevelFilter;

// Initialize tokio runtime as global lazy if needed, but for our simple gui tokio runtime can be spawned on demand or kept in App.

fn setup_custom_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    
    // Attempt to find a CJK font from the system
    let font_paths = [
        "C:\\Windows\\Fonts\\msyh.ttc", // Windows YaHei
        "C:\\Windows\\Fonts\\simhei.ttf", // Windows SimHei
        "/System/Library/Fonts/PingFang.ttc", // macOS PingFang
        "/System/Library/Fonts/STHeiti Light.ttc", // macOS Heiti
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", // Linux WenQuanYi
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", // Linux Noto CJK
    ];

    let mut loaded_font_data = None;
    for path in font_paths.iter() {
        if let Ok(data) = std::fs::read(path) {
            loaded_font_data = Some(data);
            break;
        }
    }

    if let Some(font_data) = loaded_font_data {
        fonts.font_data.insert(
            "cjk_font".to_owned(),
            egui::FontData::from_owned(font_data),
        );
        
        // Put our font first for proportional and monospace
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "cjk_font".to_owned());
            
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push("cjk_font".to_owned()); // Fallback or push, insert at 0 can work too
    } else {
        log::warn!("No system CJK font found. Chinese characters may display as boxes.");
    }

    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result<()> {
    // Setup logging
    env_logger::builder()
        .filter_level(LevelFilter::Info)
        .init();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([600.0, 400.0])
            .with_title("LuatOS Flasher"),
        ..Default::default()
    };

    eframe::run_native(
        "LuatOS GUI",
        native_options,
        Box::new(|cc| {
            // Customize egui here with cc.egui_ctx.set_fonts and cc.egui_ctx.set_visuals
            egui_extras::install_image_loaders(&cc.egui_ctx);
            // Premium Visuals Theme
            let mut visuals = egui::Visuals::dark();
            visuals.window_rounding = egui::Rounding::same(8.0);
            visuals.widgets.noninteractive.rounding = egui::Rounding::same(4.0);
            visuals.widgets.inactive.rounding = egui::Rounding::same(4.0);
            visuals.widgets.hovered.rounding = egui::Rounding::same(4.0);
            visuals.widgets.active.rounding = egui::Rounding::same(4.0);
            
            // Adjust some accent colors 
            visuals.selection.bg_fill = egui::Color32::from_rgb(0, 122, 204); // A bright accent blue 
            visuals.panel_fill = egui::Color32::from_rgb(30, 30, 32); // Deep modern gray instead of pure black/default

            cc.egui_ctx.set_visuals(visuals);
            
            // Setup fonts
            setup_custom_fonts(&cc.egui_ctx);

            Box::new(app::LuatOsGui::new(cc))
        }),
    )
}

