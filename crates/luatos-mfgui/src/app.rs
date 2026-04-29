// egui App 实现 — UI 渲染与状态更新。

use eframe::egui;

use crate::state::{AppState, FlashStatus};
use crate::worker::{FlashWorker, WorkerMsg};

/// 应用主体，持有 UI 状态和后台刷机线程句柄。
pub struct MfguiApp {
    state: AppState,
    worker: Option<FlashWorker>,
}

impl MfguiApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        // 加载系统中文字体，确保汉字正确渲染
        load_cjk_font(&cc.egui_ctx);
        Self {
            state: AppState::new(),
            worker: None,
        }
    }

    /// 启动后台刷机线程。
    fn start_flash(&mut self) {
        self.state.status = FlashStatus::Flashing;
        self.state.progress = 0.0;
        self.state.stage = "准备中...".to_string();
        self.state.logs.clear();
        self.state.scroll_to_bottom = false;

        let soc_path = self.state.soc_path.clone();
        let port = if self.state.chip_type.as_deref().map(AppState::needs_uart_port).unwrap_or(true) {
            self.state.selected_port.clone()
        } else {
            None
        };

        self.worker = Some(FlashWorker::spawn(soc_path, port));
    }

    /// 轮询后台线程消息并更新状态。
    fn poll_worker(&mut self) {
        if let Some(worker) = &self.worker {
            while let Some(msg) = worker.poll() {
                match msg {
                    WorkerMsg::Progress(p) => {
                        if !p.message.is_empty() {
                            self.state.push_log(&p.message);
                        }
                        self.state.progress = p.percent / 100.0;
                        if !p.stage.is_empty() {
                            self.state.stage = p.stage.clone();
                        }
                        if p.done {
                            if p.error {
                                self.state.push_log("❌ 刷机失败");
                            } else {
                                self.state.push_log("✅ 刷机完成");
                            }
                            self.state.status = FlashStatus::Done { ok: !p.error };
                            self.worker = None;
                            return;
                        }
                    }
                    WorkerMsg::Done(result) => {
                        // 兜底：仅在 Progress.done 未触发时处理
                        if self.state.status == FlashStatus::Flashing {
                            match result {
                                Ok(()) => {
                                    self.state.push_log("✅ 刷机完成");
                                    self.state.status = FlashStatus::Done { ok: true };
                                }
                                Err(e) => {
                                    self.state.push_log(&format!("❌ 错误: {e}"));
                                    self.state.status = FlashStatus::Done { ok: false };
                                }
                            }
                        }
                        self.worker = None;
                        return;
                    }
                }
            }
        }
    }
}

impl eframe::App for MfguiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 轮询刷机线程消息
        self.poll_worker();

        // 刷机中持续请求刷新，保证进度实时显示
        if self.state.status == FlashStatus::Flashing {
            ctx.request_repaint();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.set_min_width(560.0);

            // ── SOC 文件选择 ──────────────────────────────────────────────────
            ui.group(|ui| {
                ui.label(egui::RichText::new("固件文件 (.soc)").strong());
                ui.horizontal(|ui| {
                    let path_text = if self.state.soc_path.is_empty() {
                        "未选择固件...".to_string()
                    } else {
                        self.state.soc_path.clone()
                    };
                    let text_edit = egui::TextEdit::singleline(&mut self.state.soc_path)
                        .hint_text("未选择固件...")
                        .desired_width(ui.available_width() - 60.0);
                    let _ = path_text; // suppress warning
                    ui.add(text_edit);
                    if ui.button("浏览").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("LuatOS 固件", &["soc"]).pick_file() {
                            self.state.soc_path = path.to_string_lossy().to_string();
                            self.state.load_soc_info();
                        }
                    }
                });

                // 芯片信息或错误提示
                if let Some(ref chip) = self.state.chip_type.clone() {
                    ui.horizontal(|ui| {
                        ui.label(format!("芯片: {chip}"));
                        if let Some(ref ver) = self.state.soc_version.clone() {
                            ui.label(format!("版本: {ver}"));
                        }
                    });
                }
                if let Some(ref err) = self.state.soc_error.clone() {
                    ui.colored_label(egui::Color32::RED, err);
                }
            });

            ui.add_space(4.0);

            // ── 串口选择（仅 UART 芯片显示）─────────────────────────────────
            let needs_uart = self.state.chip_type.as_deref().map(AppState::needs_uart_port).unwrap_or(true);

            if needs_uart {
                ui.group(|ui| {
                    ui.label(egui::RichText::new("串口").strong());
                    ui.horizontal(|ui| {
                        let selected_text = self.state.selected_port.clone().unwrap_or_else(|| "-- 请选择 --".to_string());
                        egui::ComboBox::from_label("").selected_text(&selected_text).width(280.0).show_ui(ui, |ui| {
                            let ports = self.state.ports.clone();
                            for port in &ports {
                                let label = match &port.product {
                                    Some(prod) => format!("{} ({})", port.port_name, prod),
                                    None => port.port_name.clone(),
                                };
                                ui.selectable_value(&mut self.state.selected_port, Some(port.port_name.clone()), label);
                            }
                        });
                        if ui.button("刷新").clicked() {
                            self.state.refresh_ports();
                        }
                    });
                });
            } else {
                ui.label(egui::RichText::new("ℹ EC718 / air780 系列：无需手动选择串口，将自动检测 USB 设备").color(egui::Color32::from_rgb(100, 180, 255)));
            }

            ui.add_space(4.0);

            // ── 操作按钮 ─────────────────────────────────────────────────────
            ui.horizontal(|ui| match self.state.status {
                FlashStatus::Flashing => {
                    if ui.button(egui::RichText::new("⬛ 停止").color(egui::Color32::RED)).clicked() {
                        if let Some(ref worker) = self.worker {
                            worker.cancel();
                        }
                        self.state.push_log("[用户取消]");
                    }
                }
                FlashStatus::Done { .. } => {
                    let can_start = self.state.can_start_flash();
                    if ui.add_enabled(can_start, egui::Button::new("▶ 开始刷机")).clicked() {
                        self.start_flash();
                    }
                    if ui.button("↺ 重置").clicked() {
                        self.state.reset();
                    }
                }
                FlashStatus::Idle => {
                    let can_start = self.state.can_start_flash();
                    if ui.add_enabled(can_start, egui::Button::new("▶ 开始刷机")).clicked() {
                        self.start_flash();
                    }
                }
            });

            ui.add_space(4.0);

            // ── 进度条 ───────────────────────────────────────────────────────
            let progress_text = match &self.state.status {
                FlashStatus::Idle => "等待开始".to_string(),
                FlashStatus::Flashing => format!("{} {:.0}%", self.state.stage, self.state.progress * 100.0),
                FlashStatus::Done { ok: true } => "✅ 刷机成功".to_string(),
                FlashStatus::Done { ok: false } => "❌ 刷机失败".to_string(),
            };

            let bar_color = match &self.state.status {
                FlashStatus::Done { ok: false } => egui::Color32::from_rgb(200, 60, 60),
                FlashStatus::Done { ok: true } => egui::Color32::from_rgb(60, 180, 60),
                _ => egui::Color32::from_rgb(70, 150, 220),
            };

            let progress_bar = egui::ProgressBar::new(self.state.progress).text(&progress_text).fill(bar_color);
            ui.add(progress_bar);

            ui.add_space(4.0);

            // ── 日志输出 ─────────────────────────────────────────────────────
            ui.label(egui::RichText::new("日志").strong());
            let scroll_area = egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(240.0)
                .stick_to_bottom(self.state.scroll_to_bottom);

            scroll_area.show(ui, |ui| {
                for line in &self.state.logs {
                    let color = if line.contains("❌") || line.contains("错误") || line.contains("失败") || line.contains("ERROR") {
                        egui::Color32::from_rgb(220, 80, 80)
                    } else if line.contains("✅") || line.contains("成功") || line.contains("完成") {
                        egui::Color32::from_rgb(80, 200, 80)
                    } else if line.contains("[用户取消]") || line.contains("WARN") {
                        egui::Color32::from_rgb(220, 180, 50)
                    } else {
                        egui::Color32::from_rgb(200, 200, 200)
                    };
                    ui.add(egui::Label::new(egui::RichText::new(line).monospace().color(color)));
                }
            });

            // scroll_to_bottom 仅触发一次，渲染后重置
            self.state.scroll_to_bottom = false;
        });
    }
}

// ─── 字体加载 ──────────────────────────────────────────────────────────────────

/// 从系统字体目录加载支持 CJK 的字体，避免中文显示乱码。
///
/// 优先顺序（Windows）：微软雅黑 → 宋体 → SimHei。
/// 加载失败时静默回退到 egui 默认字体（Latin 正常，中文无法显示）。
fn load_cjk_font(ctx: &egui::Context) {
    // 候选字体路径（Windows 系统字体目录）
    let candidates: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc", // 微软雅黑
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
        r"C:\Windows\Fonts\simhei.ttf", // 黑体
    ];

    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("cjk_font".to_owned(), egui::FontData::from_owned(bytes));
            // 插入到字体栈首位，使中文/英文均由该字体渲染
            fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "cjk_font".to_owned());
            fonts.families.entry(egui::FontFamily::Monospace).or_default().push("cjk_font".to_owned());
            ctx.set_fonts(fonts);
            return;
        }
    }
    // 无系统字体时不报错，UI 仍可使用（中文可能显示为方块）
}
