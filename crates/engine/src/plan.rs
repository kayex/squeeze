//! Turn a [`MediaInfo`] + size budget into concrete encode settings, and
//! shrink the plan when a pass overshoots the ceiling.
//!
//! This module is pure arithmetic, with no FFmpeg, so the heuristics are easy to
//! reason about and tweak.

use crate::probe::MediaInfo;
use crate::CompressOptions;

/// Floor on video bitrate; below this, quality is so bad that shrinking further
/// is pointless (better to drop resolution, which `choose_resolution` does).
const MIN_VIDEO_BPS: i64 = 120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioAction {
    /// Stream-copy the source audio (no re-encode, no generation loss).
    Copy,
    /// Drop audio entirely.
    Drop,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncodePlan {
    pub width: i32,
    pub height: i32,
    /// Output frame rate as a rational; the encode is always normalized to this
    /// constant rate (ShadowPlay captures are VFR).
    pub fps_num: i32,
    pub fps_den: i32,
    pub video_bitrate_bps: i64,
    pub audio: AudioAction,
}

impl EncodePlan {
    pub fn fps(&self) -> f64 {
        self.fps_num as f64 / self.fps_den.max(1) as f64
    }
}

/// First-pass plan: aim for `margin * max_bytes` so we usually land under the
/// ceiling without a correction pass.
pub fn plan_initial(info: &MediaInfo, opts: &CompressOptions) -> EncodePlan {
    let audio = if opts.include_audio && info.has_audio {
        AudioAction::Copy
    } else {
        AudioAction::Drop
    };

    let video_bps = target_video_bps(info, opts, audio);
    let (fps_num, fps_den) = choose_fps(info, opts, video_bps);
    let (width, height) = choose_resolution(info, opts, video_bps);

    EncodePlan {
        width,
        height,
        fps_num,
        fps_den,
        video_bitrate_bps: video_bps,
        audio,
    }
}

impl EncodePlan {
    /// Produce a tighter plan after an overshoot. Scales video bitrate by the
    /// measured overshoot ratio (accounting for the fixed audio bytes), and may
    /// step resolution/fps down further if the new bitrate is low.
    pub fn shrink(
        &self,
        actual_bytes: u64,
        info: &MediaInfo,
        opts: &CompressOptions,
    ) -> EncodePlan {
        let audio_bytes = audio_bytes(info, self.audio);
        let target_video_bytes = (opts.max_bytes as f64 * opts.margin - audio_bytes).max(1.0);
        let actual_video_bytes = (actual_bytes as f64 - audio_bytes).max(1.0);

        // Always reduce by at least a little, even if the estimate says we're close.
        let ratio = (target_video_bytes / actual_video_bytes).min(0.97);
        let new_bps = ((self.video_bitrate_bps as f64 * ratio).round() as i64).max(MIN_VIDEO_BPS);

        let (fps_num, fps_den) = choose_fps(info, opts, new_bps);
        let (width, height) = choose_resolution(info, opts, new_bps);

        EncodePlan {
            width,
            height,
            fps_num,
            fps_den,
            video_bitrate_bps: new_bps,
            audio: self.audio,
        }
    }
}

fn audio_bytes(info: &MediaInfo, audio: AudioAction) -> f64 {
    match audio {
        AudioAction::Copy => assumed_audio_bps(info) as f64 * info.duration_s / 8.0,
        AudioAction::Drop => 0.0,
    }
}

fn assumed_audio_bps(info: &MediaInfo) -> i64 {
    if info.audio_bitrate_bps > 0 {
        info.audio_bitrate_bps
    } else {
        160_000 // typical ShadowPlay AAC when the container omits the figure
    }
}

fn target_video_bps(info: &MediaInfo, opts: &CompressOptions, audio: AudioAction) -> i64 {
    let usable_bits = opts.max_bytes as f64 * 8.0 * opts.margin;
    let audio_bits = audio_bytes(info, audio) * 8.0;
    let video_bits = (usable_bits - audio_bits).max(0.0);
    let mut bps = (video_bits / info.duration_s).round() as i64;

    // Never spend more than the source itself did: a clip that already fits the
    // budget would otherwise be re-encoded at a *higher* rate and come out
    // bigger than it went in. Re-encoding can't recover detail that was never
    // there, so the source rate is the sensible ceiling.
    if info.video_bitrate_bps > 0 {
        bps = bps.min(info.video_bitrate_bps);
    }
    bps.max(MIN_VIDEO_BPS)
}

/// Step the resolution down only as far as the bitrate warrants. Above ~10 Mbit/s
/// there's enough to do a 1440p/4K source justice, so it's kept; below that,
/// spending the bits on fewer, better pixels wins. Never upscales.
///
/// The budget is a **pixel count**, not a height. Bitrate has to cover every
/// pixel in the frame, so an ultrawide (3440x1440, 21:9) held to "1080p" would be
/// 2580x1080 and carry 35% more pixels than a 16:9 clip at the same rate, looking
/// correspondingly worse. Capping total pixels treats both alike; for 16:9 the
/// result is identical to a height cap.
fn choose_resolution(info: &MediaInfo, opts: &CompressOptions, video_bps: i64) -> (i32, i32) {
    // 16:9 reference frames: 1920x1080, 1280x720, 854x480.
    let max_pixels: i64 = if opts.keep_resolution || video_bps >= 10_000_000 {
        i64::MAX
    } else if video_bps >= 1_600_000 {
        1920 * 1080
    } else if video_bps >= 700_000 {
        1280 * 720
    } else {
        854 * 480
    };

    let (w, h) = (info.width.max(2), info.height.max(2));
    let pixels = w as i64 * h as i64;
    if pixels <= max_pixels {
        // Already within budget; keep source dimensions.
        return (make_even(w), make_even(h));
    }

    // Shrink both axes by the same factor, so the aspect ratio is preserved.
    let scale = (max_pixels as f64 / pixels as f64).sqrt();
    (
        make_even((w as f64 * scale).round() as i32),
        make_even((h as f64 * scale).round() as i32),
    )
}

/// Always normalize to CFR. Cap 60→30 when bits are tight; otherwise keep the
/// source nominal rate.
fn choose_fps(info: &MediaInfo, opts: &CompressOptions, video_bps: i64) -> (i32, i32) {
    let src_fps = info.fps();
    if !opts.keep_fps && src_fps > 45.0 && video_bps < 3_000_000 {
        return (30, 1);
    }
    (info.fps_num, info.fps_den.max(1))
}

fn make_even(x: i32) -> i32 {
    let x = x.max(2);
    x - (x % 2)
}
