//! squeeze — drop gameplay clips in, get Discord-sized MP4s out.
//!
//! Zero configuration beyond a size budget: files are written next to their
//! source with a `_discord` suffix. Encoding runs on a background worker so the
//! UI stays responsive; progress is pushed back over a channel.

// Release builds are a GUI app — don't pop a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use engine::{compress_to_target, CompressOptions};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "avi", "webm", "flv", "m4v", "ts"];

/// Discord's upload ceilings (free / Nitro Basic / Nitro).
const BUDGETS: &[(&str, u64)] = &[
    ("10 MB", 10_000_000),
    ("50 MB", 50_000_000),
    ("500 MB", 500_000_000),
];

#[derive(Clone)]
enum Status {
    Queued,
    Running {
        pass: u32,
        max_passes: u32,
        fraction: f32,
        encoder: String,
    },
    Done {
        bytes: u64,
        fits: bool,
    },
    Failed(String),
}

struct Job {
    path: PathBuf,
    source_bytes: u64,
    status: Status,
}

/// Queued unit of work handed to the encoder thread.
struct WorkItem {
    id: usize,
    path: PathBuf,
    max_bytes: u64,
}

struct Msg {
    id: usize,
    status: Status,
}

struct App {
    jobs: Vec<Job>,
    updates: Receiver<Msg>,
    work: Sender<WorkItem>,
    budget: u64,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (work_tx, work_rx) = channel::<WorkItem>();
        let (msg_tx, msg_rx) = channel::<Msg>();
        let ctx = cc.egui_ctx.clone();

        // One worker: encodes run back-to-back. Sequential keeps GPU encoder
        // sessions (limited on consumer cards) and disk I/O predictable.
        std::thread::spawn(move || {
            while let Ok(item) = work_rx.recv() {
                let opts = CompressOptions {
                    max_bytes: item.max_bytes,
                    ..Default::default()
                };
                let output = output_path(&item.path);

                let tx = msg_tx.clone();
                let repaint = ctx.clone();
                let mut last = -1.0f32;
                let result = compress_to_target(&item.path, &output, &opts, |p| {
                    // Fires per packet — only forward meaningful changes.
                    if (p.fraction - last).abs() < 0.01 && p.fraction > 0.0 {
                        return;
                    }
                    last = p.fraction;
                    let _ = tx.send(Msg {
                        id: item.id,
                        status: Status::Running {
                            pass: p.pass,
                            max_passes: p.max_passes,
                            fraction: p.fraction,
                            encoder: p.encoder.clone(),
                        },
                    });
                    repaint.request_repaint();
                });

                let status = match result {
                    Ok(o) => Status::Done {
                        bytes: o.final_bytes,
                        fits: o.fits,
                    },
                    Err(e) => Status::Failed(format!("{e:#}")),
                };
                let _ = msg_tx.send(Msg { id: item.id, status });
                ctx.request_repaint();
            }
        });

        let mut app = Self {
            jobs: Vec::new(),
            updates: msg_rx,
            work: work_tx,
            budget: BUDGETS[0].1,
        };

        // Files can also arrive as arguments — dropping them on the .exe icon in
        // Explorer, or "Open with", passes them this way rather than as a drop.
        for arg in std::env::args_os().skip(1) {
            app.enqueue(PathBuf::from(arg));
        }
        app
    }

    fn enqueue(&mut self, path: PathBuf) {
        let is_video = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false);
        if !is_video {
            return;
        }
        let source_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let id = self.jobs.len();
        self.jobs.push(Job {
            path: path.clone(),
            source_bytes,
            status: Status::Queued,
        });
        let _ = self.work.send(WorkItem {
            id,
            path,
            max_bytes: self.budget,
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.updates.try_recv() {
            if let Some(job) = self.jobs.get_mut(msg.id) {
                job.status = msg.status;
            }
        }

        let (dropped, hovering) = ui.ctx().input(|i| {
            (
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect::<Vec<PathBuf>>(),
                !i.raw.hovered_files.is_empty(),
            )
        });
        for path in dropped {
            self.enqueue(path);
        }

        // The Ui handed to us has no margin/background of its own.
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("squeeze");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (label, bytes) in BUDGETS.iter().rev() {
                        if ui
                            .selectable_label(self.budget == *bytes, *label)
                            .on_hover_text("Target upload limit")
                            .clicked()
                        {
                            self.budget = *bytes;
                        }
                    }
                    ui.label("Fit under:");
                });
            });
            ui.add_space(8.0);

            drop_zone(ui, hovering);
            ui.add_space(8.0);

            if self.jobs.is_empty() {
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                for job in &self.jobs {
                    job_row(ui, job);
                    ui.add_space(6.0);
                }
            });
        });
    }
}

fn drop_zone(ui: &mut egui::Ui, hovering: bool) {
    let visuals = ui.visuals();
    let (stroke, fill) = if hovering {
        (
            egui::Stroke::new(2.0, visuals.selection.stroke.color),
            visuals.selection.bg_fill.linear_multiply(0.25),
        )
    } else {
        (
            egui::Stroke::new(1.0, visuals.weak_text_color()),
            egui::Color32::TRANSPARENT,
        )
    };

    egui::Frame::default()
        .stroke(stroke)
        .fill(fill)
        .corner_radius(8.0)
        .inner_margin(18.0)
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(if hovering {
                    "Release to add"
                } else {
                    "Drop video files here"
                });
                ui.label(
                    egui::RichText::new("saved next to the original as _discord.mp4")
                        .small()
                        .weak(),
                );
            });
            ui.set_min_width(ui.available_width());
        });
}

fn job_row(ui: &mut egui::Ui, job: &Job) {
    let name = job
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(name).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            match &job.status {
                Status::Done { bytes, fits } => {
                    let text = format!("{} → {}", mb(job.source_bytes), mb(*bytes));
                    if *fits {
                        ui.label(egui::RichText::new(format!("{text}  ✔")).color(
                            egui::Color32::from_rgb(0x4c, 0xaf, 0x50),
                        ));
                    } else {
                        ui.label(
                            egui::RichText::new(format!("{text}  over limit"))
                                .color(egui::Color32::from_rgb(0xe5, 0x8b, 0x2c)),
                        );
                    }
                }
                Status::Failed(_) => {
                    ui.label(egui::RichText::new("failed").color(egui::Color32::from_rgb(
                        0xe5, 0x4b, 0x4b,
                    )));
                }
                _ => {
                    ui.label(egui::RichText::new(mb(job.source_bytes)).weak());
                }
            }
        });
    });

    match &job.status {
        Status::Queued => {
            ui.add(egui::ProgressBar::new(0.0).text("queued"));
        }
        Status::Running {
            pass,
            max_passes,
            fraction,
            encoder,
        } => {
            let label = if *pass > 1 {
                format!("pass {pass}/{max_passes} · {encoder}")
            } else {
                encoder.clone()
            };
            ui.add(egui::ProgressBar::new(*fraction).text(label));
        }
        Status::Done { .. } => {}
        Status::Failed(err) => {
            ui.label(
                egui::RichText::new(err)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        }
    }
}

/// `clip.mp4` -> `clip_discord.mp4`, alongside the source.
fn output_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let dir = input
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join(format!("{stem}_discord.mp4"))
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 460.0])
            .with_min_inner_size([420.0, 320.0])
            // Required for file drops on Windows.
            .with_drag_and_drop(true)
            .with_title("squeeze"),
        ..Default::default()
    };
    eframe::run_native(
        "squeeze",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
