//! squeeze: drop gameplay clips in, get Discord-sized MP4s out.
//!
//! Zero configuration beyond a size budget: files are written next to their
//! source with a `_discord` suffix. Encoding runs on a background worker so the
//! UI stays responsive; progress is pushed back over a channel.

// Release builds are a GUI app, so don't pop a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod theme;

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

/// Resolution + frame rate, for showing what a clip is and what it becomes.
#[derive(Clone, Copy, PartialEq)]
struct Shape {
    width: i32,
    height: i32,
    fps: f32,
}

impl std::fmt::Display for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}×{} · {:.0} fps", self.width, self.height, self.fps)
    }
}

#[derive(Clone)]
enum Status {
    Queued,
    Running {
        pass: u32,
        max_passes: u32,
        fraction: f32,
        encoder: String,
        out: Shape,
    },
    Done {
        bytes: u64,
        fits: bool,
        out: Shape,
    },
    Failed(String),
}

struct Job {
    path: PathBuf,
    source_bytes: u64,
    /// From probing the file when it was dropped; None if that failed.
    source: Option<Shape>,
    status: Status,
}

/// Queued unit of work handed to the encoder thread.
struct WorkItem {
    id: usize,
    path: PathBuf,
    max_bytes: u64,
    keep_fps: bool,
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
    /// Don't let the encoder halve high frame rates to buy quality.
    keep_fps: bool,
    logo: Option<egui::TextureHandle>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);

        // Reuse eframe's PNG decoder for the header mark rather than pulling in
        // an image crate of our own. This is the 96px asset, not the 256px one:
        // egui generates no mipmaps, so shrinking a 256px texture into a ~30pt
        // slot aliases badly.
        let logo = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon-96.png"))
            .ok()
            .map(|img| {
                cc.egui_ctx.load_texture(
                    "logo",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [img.width as usize, img.height as usize],
                        &img.rgba,
                    ),
                    egui::TextureOptions::LINEAR,
                )
            });

        let (work_tx, work_rx) = channel::<WorkItem>();
        let (msg_tx, msg_rx) = channel::<Msg>();
        let ctx = cc.egui_ctx.clone();

        // One worker: encodes run back-to-back. Sequential keeps GPU encoder
        // sessions (limited on consumer cards) and disk I/O predictable.
        std::thread::spawn(move || {
            while let Ok(item) = work_rx.recv() {
                let opts = CompressOptions {
                    max_bytes: item.max_bytes,
                    keep_fps: item.keep_fps,
                    ..Default::default()
                };
                let output = output_path(&item.path);

                let tx = msg_tx.clone();
                let repaint = ctx.clone();
                let mut last = -1.0f32;
                let result = compress_to_target(&item.path, &output, &opts, |p| {
                    // Fires per packet, so only forward meaningful changes.
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
                            out: Shape {
                                width: p.plan.width,
                                height: p.plan.height,
                                fps: p.plan.fps() as f32,
                            },
                        },
                    });
                    repaint.request_repaint();
                });

                let status = match result {
                    Ok(o) => Status::Done {
                        bytes: o.final_bytes,
                        fits: o.fits,
                        out: Shape {
                            width: o.last_plan.width,
                            height: o.last_plan.height,
                            fps: o.last_plan.fps() as f32,
                        },
                    },
                    Err(e) => Status::Failed(format!("{e:#}")),
                };
                let _ = msg_tx.send(Msg {
                    id: item.id,
                    status,
                });
                ctx.request_repaint();
            }
        });

        let mut app = Self {
            jobs: Vec::new(),
            updates: msg_rx,
            work: work_tx,
            budget: BUDGETS[0].1,
            keep_fps: false,
            logo,
        };

        // Files can also arrive as arguments: dropping them on the .exe icon in
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

        // Probing up front shows the clip's resolution while it waits, and
        // rejects anything unreadable immediately rather than at encode time.
        let probed = engine::probe(&path);
        let source = probed.as_ref().ok().map(|i| Shape {
            width: i.width,
            height: i.height,
            fps: i.fps() as f32,
        });
        let status = match &probed {
            Ok(_) => Status::Queued,
            Err(e) => Status::Failed(format!("{e:#}")),
        };
        let queued = matches!(status, Status::Queued);

        self.jobs.push(Job {
            path: path.clone(),
            source_bytes,
            source,
            status,
        });
        if queued {
            let _ = self.work.send(WorkItem {
                id,
                path,
                max_bytes: self.budget,
                keep_fps: self.keep_fps,
            });
        }
    }
}

impl eframe::App for App {
    /// eframe's default is a near-black (12,12,12); anything the UI leaves
    /// unpainted would show as a dark band. Match the window background instead.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.window_fill().to_normalized_gamma_f32()
    }

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

        // The Ui handed to us has no margin/background of its own, and anything
        // it doesn't paint falls through to the clear colour. Expand the frame
        // to the full viewport so the background covers the window.
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                if let Some(logo) = &self.logo {
                    ui.add(egui::Image::new(logo).fit_to_exact_size(egui::vec2(30.0, 30.0)));
                    ui.add_space(2.0);
                }
                ui.heading("squeeze");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (label, bytes) in BUDGETS.iter().rev() {
                        let selected = self.budget == *bytes;
                        let text = if selected {
                            egui::RichText::new(*label).color(theme::CYAN)
                        } else {
                            egui::RichText::new(*label).color(theme::TEXT_DIM)
                        };
                        if ui
                            .selectable_label(selected, text)
                            .on_hover_text("Target upload limit")
                            .clicked()
                        {
                            self.budget = *bytes;
                        }
                    }
                    ui.label(egui::RichText::new("Fit under").color(theme::TEXT_DIM));
                });
            });
            // Keep every control in the same right-hand column rather than
            // stranding this one under the title.
            // The horizontal wrapper matters: with_layout on its own claims all
            // the remaining vertical space and starves everything below it.
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.checkbox(&mut self.keep_fps, "Keep original frame rate")
                        .on_hover_text(
                            "By default 60 fps clips drop to 30 when there aren't enough \
                             bits to go round, which usually looks better. Tick this to \
                             keep the frame rate and accept softer frames.\n\n\
                             Applies to clips added from now on.",
                        );
                });
            });
            ui.add_space(10.0);

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
    let (stroke, fill) = if hovering {
        (
            egui::Stroke::new(1.5, theme::CYAN),
            theme::CYAN.gamma_multiply(0.10),
        )
    } else {
        (egui::Stroke::new(1.0, theme::BORDER), theme::SURFACE)
    };

    egui::Frame::default()
        .stroke(stroke)
        .fill(fill)
        .corner_radius(10.0)
        .inner_margin(22.0)
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(if hovering {
                        "Release to add"
                    } else {
                        "Drop video files here"
                    })
                    .color(if hovering { theme::CYAN } else { theme::TEXT }),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("saved next to the original as _discord.mp4")
                        .small()
                        .color(theme::TEXT_DIM),
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

    egui::Frame::default()
        .fill(theme::SURFACE)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(14, 11))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(name).strong());
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| match &job.status {
                        Status::Done { bytes, fits, .. } => {
                            let sizes = format!("{} → {}", mb(job.source_bytes), mb(*bytes));
                            if *fits {
                                ui.label(
                                    egui::RichText::new(format!("{sizes}  ✔"))
                                        .monospace()
                                        .color(theme::OK),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new(format!("{sizes}  over limit"))
                                        .monospace()
                                        .color(theme::WARN),
                                );
                            }
                        }
                        Status::Failed(_) => {
                            ui.label(egui::RichText::new("failed").color(theme::ERR));
                        }
                        _ => {
                            ui.label(
                                egui::RichText::new(mb(job.source_bytes))
                                    .monospace()
                                    .color(theme::TEXT_DIM),
                            );
                        }
                    },
                );
            });

            // What the clip is and what it's becoming, so a dropped resolution or
            // a halved frame rate is visible rather than a surprise in the output.
            let shape = match &job.status {
                Status::Running { out, .. } | Status::Done { out, .. } => match job.source {
                    Some(src) if src != *out => format!("{src} → {out}"),
                    _ => out.to_string(),
                },
                _ => job.source.map(|s| s.to_string()).unwrap_or_default(),
            };
            let note = match &job.status {
                Status::Queued => "waiting".to_string(),
                Status::Running {
                    pass,
                    max_passes,
                    encoder,
                    ..
                } => {
                    if *pass > 1 {
                        format!("{encoder} · pass {pass}/{max_passes}")
                    } else {
                        encoder.clone()
                    }
                }
                _ => String::new(),
            };
            let detail = [shape, note]
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("  ·  ");
            if !detail.is_empty() {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(detail)
                        .monospace()
                        .size(11.5)
                        .color(theme::TEXT_DIM),
                );
            }

            match &job.status {
                Status::Queued => {
                    ui.add_space(6.0);
                    theme::gradient_bar(ui, 0.0);
                }
                Status::Running { fraction, .. } => {
                    ui.add_space(6.0);
                    theme::gradient_bar(ui, *fraction);
                }
                Status::Done { .. } => {}
                Status::Failed(err) => {
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(err).small().color(theme::ERR));
                }
            }
        });
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
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([560.0, 460.0])
        .with_min_inner_size([420.0, 320.0])
        // Required for file drops on Windows.
        .with_drag_and_drop(true)
        .with_title("squeeze");

    // The icon embedded in the .exe covers Explorer, but winit doesn't reuse it
    // for the title bar / Alt-Tab, so that needs setting here as well.
    //
    // macOS gets the padded variant: the Dock expects artwork within a ~80% safe
    // area, so a full-bleed icon sits noticeably larger than every other app.
    // Windows wants full-bleed.
    #[cfg(target_os = "macos")]
    let icon_png: &[u8] = include_bytes!("../../../assets/icon-256-macos.png");
    #[cfg(not(target_os = "macos"))]
    let icon_png: &[u8] = include_bytes!("../../../assets/icon-256.png");

    if let Ok(icon) = eframe::icon_data::from_png_bytes(icon_png) {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "squeeze",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
