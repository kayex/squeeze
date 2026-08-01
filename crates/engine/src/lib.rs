//! engine: size-targeted H.264 compression for sharing clips on Discord.
//!
//! Decode/encode/mux happen IN-PROCESS via FFmpeg's libav* (the `rsmpeg` crate);
//! there is no shelled-out `ffmpeg.exe`. The default encoder is NVENC.
//!
//! The public entry point is [`compress_to_target`], which runs a
//! measure-then-re-encode loop until the output fits under a byte ceiling.

mod encode;
mod plan;
mod probe;

pub use plan::{AudioAction, EncodePlan};
pub use probe::{probe, MediaInfo};

use anyhow::{Context, Result};
use std::ffi::CString;
use std::path::{Path, PathBuf};

/// Which H.264 encoder to use. `Auto` prefers NVENC, then libx264, then
/// libopenh264, whichever the FFmpeg build provides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encoder {
    #[default]
    Auto,
    Nvenc,
    X264,
    OpenH264,
}

#[derive(Clone, Debug)]
pub struct CompressOptions {
    /// Hard ceiling: the output must end up at or below this many bytes.
    pub max_bytes: u64,
    /// First-pass aim factor (0..1) below the ceiling, leaving headroom so we
    /// usually avoid a correction pass.
    pub margin: f64,
    /// Max encode passes before giving up (returns best-effort).
    pub max_passes: u32,
    pub encoder: Encoder,
    /// Keep the source frame rate instead of capping 60→30 when bits are tight.
    pub keep_fps: bool,
    /// Stream-copy the source audio (vs. drop it).
    pub include_audio: bool,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            // Discord free-tier upload limit (mid-2026) is 10 MB. Treat it as a
            // hard ceiling; `margin` aims a bit under it.
            max_bytes: 10_000_000,
            margin: 0.92,
            max_passes: 3,
            encoder: Encoder::Auto,
            keep_fps: false,
            include_audio: true,
        }
    }
}

/// Reported once per encode pass, for progress UIs / logging.
#[derive(Clone, Debug)]
pub struct PassInfo {
    pub pass: u32,
    pub max_passes: u32,
    pub plan: EncodePlan,
    pub encoder: String,
    /// How far through the current pass, 0.0..=1.0.
    pub fraction: f32,
}

#[derive(Clone, Debug)]
pub struct CompressOutcome {
    pub output: PathBuf,
    pub final_bytes: u64,
    pub passes: u32,
    /// Whether the final file is within `max_bytes`.
    pub fits: bool,
    pub info: MediaInfo,
    pub last_plan: EncodePlan,
}

/// Compress `input` to `output`, re-encoding at a lower bitrate until it fits
/// under `opts.max_bytes` (or `opts.max_passes` is exhausted).
///
/// `on_progress` fires at the start of every pass and repeatedly as that pass
/// advances, so a UI can drive a progress bar. It may be called very frequently;
/// throttle inside the callback if that matters.
pub fn compress_to_target(
    input: &Path,
    output: &Path,
    opts: &CompressOptions,
    mut on_progress: impl FnMut(&PassInfo),
) -> Result<CompressOutcome> {
    let info = probe(input)?;
    let (encoder_name, encoder_kind) = encode::resolve_encoder(opts.encoder)?;

    let input_c = path_to_cstring(input)?;
    let output_c = path_to_cstring(output)?;

    let mut plan = plan::plan_initial(&info, opts);
    let mut passes = 0u32;
    let mut final_bytes: u64;
    let mut fits = false;

    loop {
        passes += 1;
        let encoder = encoder_name.to_string_lossy().into_owned();
        let mut report = |fraction: f32| {
            on_progress(&PassInfo {
                pass: passes,
                max_passes: opts.max_passes,
                plan: plan.clone(),
                encoder: encoder.clone(),
                fraction,
            });
        };
        report(0.0);

        encode::transcode(
            &input_c,
            &output_c,
            &plan,
            &info,
            encoder_name,
            encoder_kind,
            matches!(plan.audio, AudioAction::Copy),
            &mut report,
        )
        .with_context(|| format!("encode pass {passes} failed"))?;

        final_bytes = std::fs::metadata(output)
            .with_context(|| format!("stat output {}", output.display()))?
            .len();

        if final_bytes <= opts.max_bytes {
            fits = true;
            break;
        }
        if passes >= opts.max_passes {
            break;
        }
        // Once the plan stops changing, the bitrate floor and resolution ladder
        // have nothing left to give; repeating the encode would only burn time
        // and produce the same file.
        let next = plan.shrink(final_bytes, &info, opts);
        if next == plan {
            break;
        }
        plan = next;
    }

    Ok(CompressOutcome {
        output: output.to_path_buf(),
        final_bytes,
        passes,
        fits,
        info,
        last_plan: plan,
    })
}

pub(crate) fn path_to_cstring(p: &Path) -> Result<CString> {
    // FFmpeg expects UTF-8 paths on Windows and converts to wide internally.
    CString::new(p.to_string_lossy().into_owned().into_bytes())
        .with_context(|| format!("path contains an interior NUL: {}", p.display()))
}
