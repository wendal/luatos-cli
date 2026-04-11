use egui::{Color32, RichText, ScrollArea, ComboBox};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crossbeam_channel::{unbounded, Receiver};

pub struct LogState {
    pub baud_rate: u32,
    pub is_connected: bool,
    pub logs: Vec<(Color32, String)>,
    pub rx: Option<Receiver<String>>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl LogState {
    pub fn new() -> Self {
        Self {
            baud_rate: 921600,
            is_connected: false,
            logs: vec![(Color32::GRAY, "Terminal ready.".to_string())],
            rx: None,
            cancel: None,
        }
    }

    pub fn add_log(&mut self, color: Color32, msg: String) {
        self.logs.push((color, msg));
        if self.logs.len() > 5000 {
            self.logs.drain(0..500);
        }
    }

    pub fn start_logging(&mut self, port: String, baud: u32) {
        if self.is_connected {
            self.stop_logging();
        }
        self.is_connected = true;
        self.add_log(Color32::LIGHT_BLUE, format!("▶ Connecting to {} at {}...", port, baud));
        
        let (tx, rx) = unbounded();
        self.rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());

        let tx_err = tx.clone();
        std::thread::spawn(move || {
            let on_line: luatos_serial::LineCallback = Box::new(move |line| {
                let _ = tx.send(line.to_string());
            });
            if let Err(e) = luatos_serial::stream_log_lines(&port, baud, cancel, on_line) {
                let _ = tx_err.send(format!("ERR: Serial error: {}", e));
            }
        });
    }

    pub fn stop_logging(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.is_connected = false;
        self.rx = None;
        self.cancel = None;
        self.add_log(Color32::GOLD, "⏹ Disconnected.".into());
    }

    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, ports: &[String], selected_port: &mut Option<String>) {
        // Drain channel safely
        let mut new_lines = Vec::new();
        if let Some(rx) = &self.rx {
            while let Ok(line) = rx.try_recv() {
                new_lines.push(line);
            }
        }

        for line in new_lines {
            let color = if line.contains("ERR") || line.contains("Error") {
                Color32::LIGHT_RED
            } else if line.contains("WARN") {
                Color32::KHAKI
            } else if line.contains("INFO") {
                Color32::LIGHT_GREEN
            } else {
                Color32::GRAY
            };
            self.add_log(color, line);
            ctx.request_repaint();
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("串口控制:").strong().size(16.0));
            
            ComboBox::from_id_source("global_port_select")
                .selected_text(selected_port.as_deref().unwrap_or("—选择串口—"))
                .show_ui(ui, |ui| {
                    for p in ports {
                        ui.selectable_value(selected_port, Some(p.clone()), p);
                    }
                });

            ui.label("波特率:");
            ui.add(egui::DragValue::new(&mut self.baud_rate).speed(100));

            ui.add_space(10.0);

            if self.is_connected {
                if ui.button(RichText::new("⏹ 断开").color(Color32::LIGHT_RED)).clicked() {
                    self.stop_logging();
                }
            } else {
                let can_connect = selected_port.is_some();
                if ui.add_enabled(can_connect, egui::Button::new(RichText::new("▶ 连接").color(Color32::LIGHT_GREEN))).clicked() {
                    if let Some(port) = selected_port {
                        self.start_logging(port.clone(), self.baud_rate);
                    }
                }
            }
            
            if ui.button("🗑 清空").clicked() {
                self.logs.clear();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.is_connected {
                    ui.label(RichText::new("● 正在通讯").color(Color32::LIGHT_GREEN));
                }
            });
        });

        ui.add_space(4.0);

        egui::Frame::canvas(ui.style()).show(ui, |ui| {
            ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if self.logs.is_empty() {
                        ui.label(RichText::new("Waiting for data...").color(Color32::DARK_GRAY));
                    } else {
                        for (color, log) in &self.logs {
                            ui.label(RichText::new(log).color(*color).monospace());
                        }
                    }
                });
        });
    }
}
