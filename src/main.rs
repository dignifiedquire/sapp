#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use eframe::{
    egui::{self, Button, RichText, Style},
    emath::Align,
    epaint::{vec2, Color32, Stroke},
};
use iroh_bytes::get::db::DownloadProgress;
use iroh_net::ticket::BlobTicket;

mod upload;

const HEIGHT: f32 = 480.;

fn main() -> Result<(), eframe::Error> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([640.0, HEIGHT])
        .with_drag_and_drop(true);

    // try from different paths to account for dev and production
    let mut paths = vec![
        // dev
        PathBuf::from("./resources/icon.png"),
    ];
    if let Some(r) = macos_resource_path() {
        paths.push(r.join("resources/icon.png"));
    }

    for path in &paths {
        if let Ok(icon) = image::open(path) {
            let icon = icon.to_rgba8();
            let (icon_width, icon_height) = icon.dimensions();
            let rgba = icon.into_raw();
            let icon_data = egui::IconData {
                rgba,
                width: icon_width,
                height: icon_height,
            };
            viewport = viewport.with_icon(icon_data);
            break;
        }
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native("Sendme", options, Box::new(|cc| Box::new(Sapp::new(cc))))
}

struct Sapp {
    selected_file: Option<PathBuf>,
    input_text: String,
    download_target: Option<PathBuf>,
    shared_state: Arc<Mutex<SharedState>>,
    worker: flume::Sender<WorkerMessage>,
}

#[derive(Debug, Default)]
struct SharedState {
    sharing_progress: Option<f32>,
    ticket: Option<BlobTicket>,
    download_progress: Option<f32>,
    errors: Vec<anyhow::Error>,
}

impl SharedState {
    fn reset_download(&mut self) {
        self.sharing_progress = None;
        self.ticket = None;
    }
}

#[derive(Debug)]
enum WorkerMessage {
    Share(PathBuf),
    Get(String, PathBuf),
}

const DARK_BG: Color32 = Color32::from_rgb(26, 28, 32);
const MEDIUM_BG: Color32 = Color32::from_rgb(42, 46, 53);
const TEXT_COLOR: Color32 = Color32::from_rgb(117, 121, 131);
const BLUE_COLOR: Color32 = Color32::from_rgb(0, 138, 216);
const WHITE_COLOR: Color32 = Color32::WHITE;

impl Sapp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Configure basic style

        let mut style = Style::default();

        style.spacing.button_padding = vec2(10., 5.);

        let visuals = &mut style.visuals;
        visuals.dark_mode = true;
        visuals.panel_fill = DARK_BG;
        visuals.code_bg_color = MEDIUM_BG;
        visuals.selection.bg_fill = BLUE_COLOR;
        visuals.selection.stroke = Stroke::new(1., WHITE_COLOR);

        let widgets = &mut visuals.widgets;
        widgets.active.bg_fill = DARK_BG;
        widgets.active.weak_bg_fill = BLUE_COLOR;
        widgets.active.fg_stroke = Stroke::new(1., WHITE_COLOR);
        widgets.active.bg_stroke = Stroke::new(1., WHITE_COLOR);

        widgets.open.bg_fill = MEDIUM_BG;
        widgets.open.weak_bg_fill = MEDIUM_BG;
        widgets.open.fg_stroke = Stroke::new(1., TEXT_COLOR);
        widgets.open.bg_stroke = Stroke::new(1., TEXT_COLOR);

        widgets.inactive.bg_fill = MEDIUM_BG;
        widgets.inactive.weak_bg_fill = BLUE_COLOR;
        widgets.inactive.fg_stroke = Stroke::new(1., WHITE_COLOR);
        widgets.inactive.bg_stroke = Stroke::new(1., WHITE_COLOR);

        widgets.hovered.bg_fill = MEDIUM_BG;
        widgets.hovered.weak_bg_fill = BLUE_COLOR;
        widgets.hovered.fg_stroke = Stroke::new(1., WHITE_COLOR);
        widgets.hovered.bg_stroke = Stroke::new(1., WHITE_COLOR);

        widgets.noninteractive.bg_fill = MEDIUM_BG;
        widgets.noninteractive.weak_bg_fill = MEDIUM_BG;
        widgets.noninteractive.fg_stroke = Stroke::new(1., TEXT_COLOR);
        widgets.noninteractive.bg_stroke = Stroke::new(1., TEXT_COLOR);

        cc.egui_ctx.set_style(style);

        let ctx = cc.egui_ctx.clone();
        let shared_state = Arc::new(Mutex::new(SharedState::default()));
        let ss1 = shared_state.clone();
        let (s, r) = flume::unbounded();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();

            while let Ok(msg) = r.recv() {
                match msg {
                    WorkerMessage::Share(path) => {
                        println!("sharing: {}", path.display());

                        // import progress
                        let (send, recv) = flume::bounded(32);

                        let ctx2 = ctx.clone();
                        let ss2 = ss1.clone();
                        let res =
                            rt.block_on(async move {
                                tokio::task::spawn(async move {
                                    let mut total_size = 0;
                                    let mut imported_size = 0;
                                    while let Ok(ev) = recv.recv_async().await {
                                        match ev {
                                        iroh_bytes::store::ImportProgress::Size { size, .. } => {
                                            total_size += size;
                                            let p = total_size as f32 / imported_size as f32;
                                            ss2.lock().unwrap().sharing_progress.replace(p);
                                            ctx2.request_repaint();
                                        }
                                        iroh_bytes::store::ImportProgress::OutboardProgress {
                                            offset,
                                            ..
                                        } => {
                                            imported_size += offset;
                                            let p = total_size as f32 / imported_size as f32;
                                            ss2.lock().unwrap().sharing_progress.replace(p);
                                            ctx2.request_repaint();
                                        }
                                        _ => {}
                                    }
                                    }
                                });
                                let (ticket, _handle) = upload::provide(path, send).await?;
                                anyhow::Ok(ticket)
                            });
                        match res {
                            Ok(ticket) => {
                                let mut state = ss1.lock().unwrap();
                                state.sharing_progress = None;
                                state.ticket = Some(ticket);

                                ctx.request_repaint();
                            }
                            Err(err) => {
                                eprintln!("failed: {:?}", err);
                                ss1.lock().unwrap().errors.push(err.context("sharing"));
                            }
                        }
                    }
                    WorkerMessage::Get(ticket, target) => {
                        match ticket.parse::<BlobTicket>() {
                            Ok(ticket) => {
                                println!("getting: {}", ticket);
                                // import progress
                                let (send, recv) = flume::bounded(32);

                                let ctx2 = ctx.clone();
                                let ss2 = ss1.clone();
                                ss1.lock().unwrap().download_progress = Some(0.0);
                                ctx.request_repaint();

                                let res = rt.block_on(async move {
                                    tokio::task::spawn(async move {
                                        while let Ok(ev) = recv.recv_async().await {
                                            // TODO: propper progress
                                            match ev {
                                                DownloadProgress::AllDone => {}
                                                _ => {}
                                            }
                                        }
                                    });

                                    upload::get(ticket, target, send).await
                                });

                                if let Err(err) = res {
                                    eprintln!("failed: {:?}", err);
                                    ss1.lock().unwrap().errors.push(err.context("get"));
                                } else {
                                    ss1.lock().unwrap().download_progress = None;
                                    ctx.request_repaint();
                                }
                            }
                            Err(err) => {
                                eprintln!("invalid ticket: {:?}", err);
                                ss1.lock()
                                    .unwrap()
                                    .errors
                                    .push(anyhow::anyhow!(err).context("parsing ticket"));
                            }
                        }
                    }
                }
            }
        });

        Sapp {
            shared_state,
            input_text: String::new(),
            download_target: None,
            worker: s,
            selected_file: None,
        }
    }

    fn show_errors(&mut self, ctx: &egui::Context) {
        let err = self
            .shared_state
            .lock()
            .unwrap()
            .errors
            .last()
            .map(|e| format!("{:#}", e));

        if let Some(err) = err {
            let center = ctx.screen_rect().center();
            let width = 200.;
            let height = 100.;

            let pos = egui::Pos2 {
                x: center.x - width / 2.,
                y: center.y - height / 2.,
            };
            egui::Window::new("⚠ Error")
                .fixed_size(vec2(width, height))
                .collapsible(false)
                .fixed_pos(pos)
                .show(ctx, |ui| {
                    ui.set_min_height(height);
                    ui.set_min_width(width);

                    ui.add_space(5.);
                    ui.label(err);
                    ui.add_space(5.);

                    ui.with_layout(egui::Layout::bottom_up(Align::RIGHT), |ui| {
                        if ui.button("Ok").clicked() {
                            self.shared_state.lock().unwrap().errors.pop();
                        }
                        ui.add_space(5.);
                        ui.separator();
                    });
                });
        }
    }
}

impl eframe::App for Sapp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Receive").heading().color(WHITE_COLOR));
                ui.add_space(10.);

                let state = self.shared_state.lock().unwrap();
                if state.download_progress.is_some() {
                    let mut text: &str = &self.input_text;
                    ui.add(egui::TextEdit::multiline(&mut text).font(egui::FontId::monospace(12.)));
                    ui.add_space(10.);
                } else {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.input_text)
                            .font(egui::FontId::monospace(12.))
                            .hint_text("Paste ticket"),
                    );

                    ui.add_space(10.);
                    let button = egui::Button::new("Save to...");

                    if ui.add(button).clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.download_target.replace(path);
                        }
                    }
                }
                if let Some(ref target) = self.download_target {
                    ui.add_space(5.);
                    ui.label(format!("Target: {}", target.display()));
                    ui.add_space(5.);

                    if let Some(_progress) = state.download_progress {
                        ui.add_space(5.);
                        ui.add(egui::Spinner::new());
                    } else {
                        if ui.button("Download").clicked() {
                            self.worker
                                .send(WorkerMessage::Get(self.input_text.clone(), target.clone()))
                                .ok();
                        }
                    }
                }
            });
            ui.add_space(20.);
            ui.separator();
            ui.add_space(20.);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Send").heading().color(WHITE_COLOR));
                ui.add_space(10.);

                egui::ScrollArea::vertical().show(ui, |ui| {
                    let button = Button::new("Drag and drop or browse your files…")
                        .rounding(10.)
                        .fill(MEDIUM_BG)
                        .min_size(vec2(250., 150.));

                    let button_res = ui.add(button);
                    if button_res.clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            self.selected_file.replace(path);
                        }
                    }
                    preview_files_being_dropped(&button_res.ctx);

                    if let Some(path) = &self.selected_file {
                        ui.vertical_centered(|ui| {
                            ui.add_space(25.);
                            ui.heading("Selected file:");
                            let name = path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| path.display().to_string());
                            ui.monospace(&name);

                            ui.add_space(15.);
                            {
                                let state = self.shared_state.lock().unwrap();
                                if state.ticket.is_none() {
                                    if ui.button("Share").clicked() {
                                        self.worker.send(WorkerMessage::Share(path.clone())).ok();
                                    }
                                }
                                if let Some(_progress) = state.sharing_progress {
                                    ui.add_space(5.);
                                    ui.add(egui::Spinner::new());
                                }

                                if let Some(ref ticket) = state.ticket {
                                    // selectable text
                                    let ticket_text = ticket.to_string();
                                    let mut text: &str = &ticket_text;
                                    ui.vertical_centered(|ui| {
                                        ui.heading("Ready to share:");
                                        ui.add_space(10.);
                                        ui.add(
                                            egui::TextEdit::multiline(&mut text)
                                                .font(egui::FontId::monospace(12.)),
                                        );
                                    });
                                }
                            }
                        });
                    }
                });
            });
        });

        // Collect dropped files:
        ctx.input(|i| {
            if !i.raw.dropped_files.is_empty() {
                if let Some(ref path) = i.raw.dropped_files[0].path {
                    self.selected_file.replace(path.clone());
                    self.shared_state.lock().unwrap().reset_download();
                }
            }
        });

        // Show potential errors
        self.show_errors(&ctx);
    }
}

/// Preview hovering files:
fn preview_files_being_dropped(ctx: &egui::Context) {
    use egui::*;

    if !ctx.input(|i| i.raw.hovered_files.is_empty()) {
        let text = ctx.input(|i| {
            if i.raw.hovered_files.len() == 1 {
                let file = &i.raw.hovered_files[0];
                if let Some(ref path) = file.path {
                    format!("Dropping file:\n{}", path.display())
                } else {
                    "Invalid file".into()
                }
            } else {
                "Only one file is allowed".into()
            }
        });

        let painter =
            ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("file_drop_target")));

        let screen_rect = ctx.screen_rect();
        painter.rect_filled(screen_rect, 0.0, Color32::from_black_alpha(192));
        let galley = painter.layout(
            text,
            TextStyle::Heading.resolve(&ctx.style()),
            Color32::WHITE,
            400.,
        );
        let center = screen_rect.center();
        let x_t = galley.rect.width() / 2.;
        let y_t = galley.rect.height() / 2.;
        let pos = Pos2 {
            x: center.x - x_t,
            y: center.y - y_t,
        };
        painter.galley(pos, galley);
    }
}

#[cfg(target_os = "macos")]
fn macos_resource_path() -> Option<PathBuf> {
    let bundle = core_foundation::bundle::CFBundle::main_bundle();
    let bundle_path = bundle.path()?;
    let resources_path = bundle.resources_path()?;
    let absolute_resources_root = bundle_path.join(resources_path);
    Some(absolute_resources_root)
}

#[cfg(not(target_os = "macos"))]
fn macos_resource_path() -> Option<PathBuf> {
    None
}
