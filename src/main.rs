#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use eframe::egui;
use iroh_net::ticket::BlobTicket;

mod upload;

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 480.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Sendme", options, Box::new(|cc| Box::new(Sapp::new(cc))))
}

struct Sapp {
    selected_file: Option<PathBuf>,
    shared_state: Arc<Mutex<SharedState>>,
    worker: flume::Sender<WorkerMessage>,
}

#[derive(Debug, Default)]
struct SharedState {
    sharing_progress: Option<f32>,
    ticket: Option<BlobTicket>,
}

impl SharedState {
    fn reset(&mut self) {
        self.sharing_progress = None;
        self.ticket = None;
    }
}

#[derive(Debug)]
enum WorkerMessage {
    Share(PathBuf),
    Shutdown,
}

impl Sapp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
                                // TODO: show error
                                eprintln!("failed: {:?}", err);
                            }
                        }
                    }
                    WorkerMessage::Shutdown => {
                        println!("shutting down worker");
                        break;
                    }
                }
            }
        });

        Sapp {
            shared_state,
            worker: s,
            selected_file: None,
        }
    }
}

impl eframe::App for Sapp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Drop a file to upload!");

            if ui.button("Select file…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    self.selected_file.replace(path);
                }
            }

            if let Some(path) = &self.selected_file {
                ui.vertical(|ui| {
                    ui.add_space(50.);
                    ui.heading("Selected file:");
                    ui.horizontal_wrapped(|ui| {
                        ui.monospace(&path.display().to_string());
                    });

                    ui.add_space(30.);
                    if ui.button("Share").clicked() {
                        self.worker.send(WorkerMessage::Share(path.clone())).ok();
                    }
                    {
                        let state = self.shared_state.lock().unwrap();
                        if let Some(_progress) = state.sharing_progress {
                            ui.horizontal(|ui| {
                                ui.label("Sharing...");
                                ui.add_space(5.);
                                ui.add(egui::Spinner::new());
                            });
                        }

                        if let Some(ref ticket) = state.ticket {
                            // selectable text
                            let ticket_text = ticket.to_string();
                            let mut text: &str = &ticket_text;
                            ui.vertical(|ui| {
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

        preview_files_being_dropped(ctx);

        // Collect dropped files:
        ctx.input(|i| {
            if !i.raw.dropped_files.is_empty() {
                if let Some(ref path) = i.raw.dropped_files[0].path {
                    self.selected_file.replace(path.clone());
                    self.shared_state.lock().unwrap().reset();
                }
            }
        });
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
