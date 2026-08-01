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

/// Discord's upload ceilings, named by the tier they belong to: people know
/// which Discord plan they have far better than they know its byte limit.
/// (tier, limit shown when picked, bytes)
const BUDGETS: &[(&str, &str, u64)] = &[
    ("Free", "10 MB", 10_000_000),
    ("Nitro Basic", "50 MB", 50_000_000),
    ("Nitro", "500 MB", 500_000_000),
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
    /// The limit this job was queued against, for the over-limit message.
    budget: u64,
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
    no_audio: bool,
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
    /// Drop the audio track, spending its share of the budget on video.
    no_audio: bool,
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
                    include_audio: !item.no_audio,
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
            budget: BUDGETS[0].2,
            keep_fps: false,
            no_audio: false,
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
            budget: self.budget,
            source,
            status,
        });
        if queued {
            let _ = self.work.send(WorkItem {
                id,
                path,
                max_bytes: self.budget,
                keep_fps: self.keep_fps,
                no_audio: self.no_audio,
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

                // Plans sit opposite the title. One is always lit, so the group
                // reads as a choice without needing a label or button chrome.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (tier, size, bytes) in BUDGETS.iter().rev() {
                        if plan_option(ui, tier, size, self.budget == *bytes).clicked() {
                            self.budget = *bytes;
                        }
                    }
                });
            });

            // Switches, styled like the plans and lit when on. A row rather than
            // a stack of checkboxes, so more can be added without it becoming a
            // list. Rightmost is added first in a right-to-left layout.
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Cells butt together: -1 so the neighbours' strokes land on
                    // top of each other rather than reading as a 2px divider.
                    ui.spacing_mut().item_spacing.x = -1.0;
                    // Right-to-left, so the rightmost cell is added first.
                    if toggle(ui, "No audio", self.no_audio, Seg::Right, self.keep_fps).clicked() {
                        self.no_audio = !self.no_audio;
                    }
                    if toggle(ui, "Keep 60 fps", self.keep_fps, Seg::Left, self.no_audio).clicked()
                    {
                        self.keep_fps = !self.keep_fps;
                    }
                });
            });
            ui.add_space(12.0);

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

/// A plan: name on top, the limit it allows underneath. Sized to its own content
/// so "Free" doesn't carry the padding "Nitro Basic" needs.
fn plan_option(ui: &mut egui::Ui, tier: &str, size: &str, selected: bool) -> egui::Response {
    let (name_colour, size_colour) = if selected {
        (theme::CYAN, theme::CYAN.gamma_multiply(0.7))
    } else {
        (theme::TEXT_DIM, theme::TEXT_DIM.gamma_multiply(0.8))
    };

    let mut job = egui::text::LayoutJob::default();
    job.append(
        tier,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(13.5),
            color: name_colour,
            ..Default::default()
        },
    );
    job.append(
        &format!("\n{size}"),
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::monospace(10.5),
            color: size_colour,
            ..Default::default()
        },
    );
    pinned_button(ui, job, selected)
}

/// Where a switch sits in its group, which decides the corners it rounds and
/// whether it draws the seam to its neighbour.
#[derive(Clone, Copy, PartialEq)]
enum Seg {
    Left,
    Right,
}

/// One cell of a joined on/off group, painted by hand.
///
/// Hand-painting buys two things a plain `Button` cannot: a lit cell with no
/// outer border but a seam still dividing it from its neighbour, and knowledge
/// of the hover state at paint time, since `allocate_exact_size` hands back the
/// response before anything is drawn.
///
/// `neighbour_on` is only consulted by the left cell, to colour that seam.
fn toggle(
    ui: &mut egui::Ui,
    label: &str,
    on: bool,
    seg: Seg,
    neighbour_on: bool,
) -> egui::Response {
    let r = 8;
    let corners = match seg {
        Seg::Left => egui::CornerRadius {
            nw: r,
            sw: r,
            ne: 0,
            se: 0,
        },
        Seg::Right => egui::CornerRadius {
            ne: r,
            se: r,
            nw: 0,
            sw: 0,
        },
    };

    let text_colour = if on { theme::BG } else { theme::TEXT_DIM };
    let mut job = egui::text::LayoutJob::default();
    job.append(
        label,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(13.0),
            color: text_colour,
            ..Default::default()
        },
    );

    let galley = ui.painter().layout_job(job);
    let padding = ui.spacing().button_padding;
    let (rect, response) =
        ui.allocate_exact_size(galley.size() + padding * 2.0, egui::Sense::click());
    let hovered = response.hovered();

    // Lit cells carry no outline; the seam below is what separates them. Unlit
    // cells keep a border so the group reads as a control while it is all off.
    let (fill, stroke) = match (on, hovered) {
        (true, true) => (
            theme::mix(theme::CYAN, egui::Color32::WHITE, 0.18),
            egui::Stroke::NONE,
        ),
        (true, false) => (theme::CYAN, egui::Stroke::NONE),
        (false, true) => (
            theme::SURFACE_HI,
            egui::Stroke::new(1.0, theme::CYAN.gamma_multiply(0.6)),
        ),
        (false, false) => (theme::SURFACE, egui::Stroke::new(1.0, theme::BORDER)),
    };

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect(rect, corners, fill, stroke, egui::StrokeKind::Inside);
        painter.galley(rect.center() - galley.size() / 2.0, galley, text_colour);

        // The seam. Drawn by the left cell so it lands once, and tinted to stay
        // legible whether it divides two lit cells or a lit from an unlit one.
        if seg == Seg::Left {
            let colour = if on && neighbour_on {
                // Blended, not faded: a translucent line over a cyan cell
                // composites straight back to cyan and vanishes.
                theme::mix(theme::CYAN, theme::BG, 0.45)
            } else if on || neighbour_on {
                theme::BG
            } else {
                theme::BORDER
            };
            painter.vline(rect.right(), rect.y_range(), egui::Stroke::new(1.0, colour));
        }
    }
    response
}

/// A selectable button whose width is pinned to its own text.
///
/// egui derives a button's inner margin from the visuals of its *current state*
/// (`button_padding + expansion - bg_stroke.width`), so the same control can
/// allocate a different width once selected or hovered, shunting its neighbours
/// sideways. Measuring the text and demanding that width in every state stops
/// the row from twitching, while still letting each option size to its own
/// content.
fn pinned_button(ui: &mut egui::Ui, job: egui::text::LayoutJob, selected: bool) -> egui::Response {
    let galley = ui.painter().layout_job(job.clone());
    let padding = ui.spacing().button_padding;
    let min_size = galley.size() + padding * 2.0;
    ui.add(egui::Button::selectable(selected, job).min_size(min_size))
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
                                    egui::RichText::new(format!("{sizes}  too big"))
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
                // Landing over the limit is not an error, so say what happened
                // and what would help rather than leaving a bare warning colour.
                Status::Done { fits: false, .. } => {
                    ui.add_space(3.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Couldn't reach {} even at the lowest quality. \
                             The clip is too long for that limit; trim it to a \
                             shorter section and try again.",
                            budget_label(job.budget)
                        ))
                        .small()
                        .color(theme::WARN),
                    );
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

/// The limit as the buttons spell it ("10 MB"), not as a computed figure
/// ("10.0 MB"), so the message matches what was clicked.
fn budget_label(bytes: u64) -> String {
    BUDGETS
        .iter()
        .find(|(_, _, b)| *b == bytes)
        .map(|(_, size, _)| (*size).to_string())
        .unwrap_or_else(|| mb(bytes))
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
