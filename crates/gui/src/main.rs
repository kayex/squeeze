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
        bitrate_bps: i64,
        out: Shape,
    },
    Done {
        bytes: u64,
        fits: bool,
        encoder: String,
        bitrate_bps: i64,
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
    /// When encoding began, so the header can count up while it runs.
    started: Option<std::time::Instant>,
    /// How long encoding took, frozen once it finishes.
    took: Option<std::time::Duration>,
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
                // compress_to_target doesn't report the encoder in its outcome,
                // so keep the last one it named for the finished row.
                let mut used_encoder = String::new();
                let result = compress_to_target(&item.path, &output, &opts, |p| {
                    used_encoder.clone_from(&p.encoder);
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
                            bitrate_bps: p.plan.video_bitrate_bps,
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
                        encoder: used_encoder,
                        bitrate_bps: o.last_plan.video_bitrate_bps,
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
            started: None,
            took: None,
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
        let mut running = false;
        while let Ok(msg) = self.updates.try_recv() {
            if let Some(job) = self.jobs.get_mut(msg.id) {
                match &msg.status {
                    Status::Running { .. } => {
                        job.started.get_or_insert_with(std::time::Instant::now);
                    }
                    Status::Done { .. } | Status::Failed(_) => {
                        job.took = Some(job.started.map(|s| s.elapsed()).unwrap_or_default());
                    }
                    Status::Queued => {}
                }
                job.status = msg.status;
            }
        }
        for job in &self.jobs {
            running |= matches!(job.status, Status::Running { .. });
        }
        // Progress messages alone would leave the counter stalling between
        // updates, so tick regardless while anything is encoding.
        if running {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
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
                // Fix the row's height before anything goes in it. A horizontal
                // layout centres each item against the height known at the time
                // it is added, so the wordmark and mark, which are added first,
                // would otherwise sit high while the taller plan chips grow the
                // row beneath them.
                ui.set_min_height(plan_row_height(ui));
                if let Some(logo) = &self.logo {
                    // Sits slightly above the row's centre line. The eye reads
                    // the mark as one solid block and weighs it against the
                    // letters' cap band, which is itself above the text box's
                    // centre because that box reserves room for the descender of
                    // the "q". Centring the two boxes therefore looks bottom
                    // heavy, however even it measures.
                    const OPTICAL_LIFT: f32 = 0.5;
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
                    egui::Image::new(logo)
                        .paint_at(ui, rect.translate(egui::vec2(0.0, OPTICAL_LIFT)));
                    ui.add_space(2.0);
                }
                ui.heading("Squeeze");

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
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                // Centred by hand. Wrapping the row in vertical_centered does
                // nothing, because a horizontal layout claims the full width
                // and so is already "centred"; the group's own width has to
                // be measured and the slack split.
                let group = toggle_width(ui, "Keep 60 fps") + toggle_width(ui, "No audio") - 1.0;
                ui.add_space(((ui.available_width() - group) / 2.0).max(0.0));
                // Cells butt together: -1 so the neighbours' strokes land on
                // top of each other rather than reading as a 2px divider.
                ui.spacing_mut().item_spacing.x = -1.0;
                // Left cell first, so the right one can be told whether its
                // neighbour is hovered before it paints the shared seam.
                let left = toggle(
                    ui,
                    "Keep 60 fps",
                    self.keep_fps,
                    Seg::Left,
                    self.no_audio,
                    false,
                );
                let right = toggle(
                    ui,
                    "No audio",
                    self.no_audio,
                    Seg::Right,
                    self.keep_fps,
                    left.hovered(),
                );
                if left.clicked() {
                    self.keep_fps = !self.keep_fps;
                }
                if right.clicked() {
                    self.no_audio = !self.no_audio;
                }
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

/// How tall a plan chip comes out, built the same way [`plan_option`] builds one
/// so the header row can reserve its height before laying anything out.
fn plan_row_height(ui: &egui::Ui) -> f32 {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        "X",
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(13.5),
            ..Default::default()
        },
    );
    job.append(
        "\nX",
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::monospace(10.5),
            ..Default::default()
        },
    );
    ui.painter().layout_job(job).size().y + ui.spacing().button_padding.y * 2.0
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

/// How wide [`toggle`] will come out for a given label, so a row of them can be
/// centred before any of them is laid out.
fn toggle_width(ui: &egui::Ui, label: &str) -> f32 {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        label,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(13.0),
            ..Default::default()
        },
    );
    ui.painter().layout_job(job).size().x + ui.spacing().button_padding.x * 2.0
}

/// One cell of a joined on/off group, painted by hand.
///
/// Hand-painting buys two things a plain `Button` cannot: a lit cell with no
/// outer border but a seam still dividing it from its neighbour, and knowledge
/// of the hover state at paint time, since `allocate_exact_size` hands back the
/// response before anything is drawn.
///
/// `neighbour_on` and `neighbour_hovered` are only consulted by the left cell,
/// which owns the seam and so has to colour it on behalf of both.
fn toggle(
    ui: &mut egui::Ui,
    label: &str,
    on: bool,
    seg: Seg,
    neighbour_on: bool,
    neighbour_hovered: bool,
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
        (false, true) => (theme::SURFACE_HI, egui::Stroke::new(1.0, theme::CYAN)),
        (false, false) => (theme::SURFACE, egui::Stroke::new(1.0, theme::BORDER)),
    };

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect(rect, corners, fill, stroke, egui::StrokeKind::Inside);
        painter.galley(rect.center() - galley.size() / 2.0, galley, text_colour);

        // The seam, drawn by the cell added second so it lands on top of its
        // neighbour's edge, and tinted to stay legible whether it divides two
        // lit cells or a lit from an unlit one.
        if seg == Seg::Right {
            // An unlit cell being hovered draws a cyan border, and this seam is
            // part of that border. Painted after both cells, it would otherwise
            // overwrite the right cell's edge and leave its outline broken.
            let outlined = (hovered && !on) || (neighbour_hovered && !neighbour_on);
            let colour = if outlined {
                theme::CYAN
            } else if on && neighbour_on {
                // Blended, not faded: a translucent line over a cyan cell
                // composites straight back to cyan and vanishes.
                theme::mix(theme::CYAN, theme::BG, 0.45)
            } else if on || neighbour_on {
                theme::BG
            } else {
                theme::BORDER
            };
            painter.vline(rect.left(), rect.y_range(), egui::Stroke::new(1.0, colour));
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

            // Header: the file, how long it runs, and how it ended up.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(name).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match &job.status {
                        Status::Done { fits: true, .. } => {
                            ui.label(egui::RichText::new("✔").color(theme::OK));
                        }
                        Status::Done { fits: false, .. } => {
                            ui.label(egui::RichText::new("too big").color(theme::WARN));
                        }
                        Status::Failed(_) => {
                            ui.label(egui::RichText::new("failed").color(theme::ERR));
                        }
                        _ => {}
                    }
                    let elapsed = job
                        .took
                        .or_else(|| job.started.map(|s| s.elapsed()))
                        .map(|d| d.as_secs_f64());
                    if let Some(secs) = elapsed {
                        ui.label(
                            egui::RichText::new(duration(secs))
                                .monospace()
                                .size(11.5)
                                .color(theme::TEXT_DIM),
                        );
                    }
                });
            });

            // Before over after, so the columns stack into a table and a field
            // that changed stands out when scanning a long queue.
            ui.add_space(4.0);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                if let Some(src) = job.source {
                    stat_line(
                        ui,
                        Some(src),
                        Some((mb(job.source_bytes), theme::TEXT_DIM)),
                        None,
                        "",
                    );
                }
                match &job.status {
                    Status::Queued => stat_line(
                        ui,
                        None,
                        Some((mb(job.budget), theme::TEXT_DIM)),
                        None,
                        "waiting",
                    ),
                    Status::Running {
                        out,
                        encoder,
                        bitrate_bps,
                        pass,
                        max_passes,
                        ..
                    } => {
                        let rate = bitrate(*bitrate_bps);
                        let note = if *pass > 1 {
                            format!("{encoder} · {rate} · pass {pass}/{max_passes}")
                        } else {
                            format!("{encoder} · {rate}")
                        };
                        // The chosen limit stands in until the real size is
                        // known, so it is obvious which setting the job started
                        // with rather than being taken on trust.
                        stat_line(
                            ui,
                            Some(*out),
                            Some((mb(job.budget), theme::TEXT_DIM)),
                            job.source,
                            &note,
                        );
                    }
                    Status::Done {
                        bytes,
                        fits,
                        out,
                        encoder,
                        bitrate_bps,
                    } => stat_line(
                        ui,
                        Some(*out),
                        // Echoes the verdict in the header, so the number that
                        // matters is the one the eye lands on.
                        Some((mb(*bytes), if *fits { theme::OK } else { theme::WARN })),
                        job.source,
                        &format!("{encoder} · {}", bitrate(*bitrate_bps)),
                    ),
                    Status::Failed(_) => {}
                }
            });

            match &job.status {
                Status::Queued => {
                    ui.add_space(6.0);
                    theme::gradient_bar(ui, 0.0);
                }
                Status::Running { fraction, .. } => {
                    ui.add_space(6.0);
                    theme::gradient_bar(ui, *fraction);
                }
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

/// One "before" or "after" line. Fields sit in fixed monospace columns so the
/// two lines stack into a table, and anything `compared_to` shows has changed is
/// picked out in the accent colour.
fn stat_line(
    ui: &mut egui::Ui,
    shape: Option<Shape>,
    size: Option<(String, egui::Color32)>,
    compared_to: Option<Shape>,
    note: &str,
) {
    let changed = |f: fn(&Shape) -> String| match (shape, compared_to) {
        (Some(a), Some(b)) => f(&a) != f(&b),
        _ => false,
    };
    let res_changed = changed(|s| format!("{}x{}", s.width, s.height));
    let fps_changed = changed(|s| format!("{:.0}", s.fps));

    let cell = |t: String, accent: bool| {
        egui::RichText::new(t)
            .monospace()
            .size(11.5)
            .color(if accent { theme::CYAN } else { theme::TEXT_DIM })
    };
    let tinted = |t: String, colour: egui::Color32| {
        egui::RichText::new(t).monospace().size(11.5).color(colour)
    };

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let (res, fps) = match shape {
            Some(s) => (
                format!("{}×{}", s.width, s.height),
                format!("{:.0} fps", s.fps),
            ),
            None => (String::new(), String::new()),
        };
        ui.label(cell(format!("{res:<9}"), res_changed));
        ui.label(cell(format!("{fps:>7}"), fps_changed));
        let (size_text, size_colour) = size.unwrap_or((String::new(), theme::TEXT_DIM));
        ui.label(tinted(format!("{size_text:>9}"), size_colour));
        if !note.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(cell(note.to_string(), false));
            });
        }
    });
}

/// `1m 22s`, or `47s` for anything under a minute.
fn duration(secs: f64) -> String {
    let s = secs.round() as i64;
    if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

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

/// The video bitrate the encode is aiming at. This comes from the plan, so it
/// means the same thing whichever encoder is in use.
fn bitrate(bps: i64) -> String {
    if bps >= 1_000_000 {
        format!("{:.1} Mbit/s", bps as f64 / 1_000_000.0)
    } else {
        format!("{} kbit/s", bps / 1000)
    }
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
        .with_title("Squeeze");

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
        "Squeeze",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
