//! Turn a [`MediaInfo`] + size budget into concrete encode settings, and
//! shrink the plan when a pass overshoots the ceiling.
//!
//! This module is pure arithmetic — no FFmpeg — so the heuristics are easy to
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

#[derive(Clone, Debug)]
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
    let (width, height) = choose_resolution(info, video_bps);

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
    pub fn shrink(&self, actual_bytes: u64, info: &MediaInfo, opts: &CompressOptions) -> EncodePlan {
        let audio_bytes = audio_bytes(info, self.audio);
        let target_video_bytes =
            (opts.max_bytes as f64 * opts.margin - audio_bytes).max(1.0);
        let actual_video_bytes = (actual_bytes as f64 - audio_bytes).max(1.0);

        // Always reduce by at least a little, even if the estimate says we're close.
        let ratio = (target_video_bytes / actual_video_bytes).min(0.97);
        let new_bps =
            ((self.video_bitrate_bps as f64 * ratio).round() as i64).max(MIN_VIDEO_BPS);

        let (fps_num, fps_den) = choose_fps(info, opts, new_bps);
        let (width, height) = choose_resolution(info, new_bps);

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
    ((video_bits / info.duration_s).round() as i64).max(MIN_VIDEO_BPS)
}

/// Step the resolution down only as far as the bitrate warrants. Above ~10 Mbit/s
/// there's enough to do a 1440p/4K source justice, so it's kept; below that,
/// spending the bits on fewer, better pixels wins. Never upscales. Dimensions are
/// forced even (yuv420p).
fn choose_resolution(info: &MediaInfo, video_bps: i64) -> (i32, i32) {
    let mut target_h = info.height;
    if video_bps < 10_000_000 {
        target_h = target_h.min(1080);
    }
    if video_bps < 1_600_000 {
        target_h = target_h.min(720);
    }
    if video_bps < 700_000 {
        target_h = target_h.min(480);
    }

    if target_h >= info.height {
        // No downscale needed; keep source dimensions (already even from capture).
        return (make_even(info.width), make_even(info.height));
    }

    let width = (info.width as f64 * target_h as f64 / info.height as f64).round() as i32;
    (make_even(width), make_even(target_h))
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
