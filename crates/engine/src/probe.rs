//! Inspect an input file enough to plan a size-targeted encode.

use crate::path_to_cstring;
use anyhow::{anyhow, Context, Result};
use rsmpeg::{avcodec::AVCodec, avformat::AVFormatContextInput, ffi};
use std::path::Path;

/// The handful of facts we need about an input to plan an encode.
#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub duration_s: f64,
    pub width: i32,
    pub height: i32,
    /// Nominal frame rate as a rational (e.g. 60000/1001 for 59.94).
    pub fps_num: i32,
    pub fps_den: i32,
    pub has_audio: bool,
    /// How many audio streams the container carries. ShadowPlay writes two when
    /// asked to keep game sound and microphone apart.
    pub audio_tracks: usize,
    /// Audio bitrate in bits/s, or 0 if the container didn't record it.
    pub audio_bitrate_bps: i64,
    /// Video bitrate in bits/s, or 0 if it couldn't be determined. Used as a
    /// ceiling so a generous budget never re-encodes a clip *larger* than it was.
    pub video_bitrate_bps: i64,
    pub video_codec: String,
}

impl MediaInfo {
    pub fn fps(&self) -> f64 {
        self.fps_num as f64 / self.fps_den.max(1) as f64
    }
}

pub fn probe(path: &Path) -> Result<MediaInfo> {
    let url = path_to_cstring(path)?;
    let ifmt = AVFormatContextInput::open(&url)
        .with_context(|| format!("failed to open input {}", path.display()))?;

    let mut info = MediaInfo {
        duration_s: 0.0,
        width: 0,
        height: 0,
        fps_num: 0,
        fps_den: 1,
        has_audio: false,
        audio_tracks: 0,
        audio_bitrate_bps: 0,
        video_bitrate_bps: 0,
        video_codec: String::new(),
    };

    // Container duration is in AV_TIME_BASE (microsecond) units.
    if ifmt.duration > 0 {
        info.duration_s = ifmt.duration as f64 / ffi::AV_TIME_BASE as f64;
    }

    let mut found_video = false;
    for stream in ifmt.streams() {
        let par = stream.codecpar();
        let codec_type = par.codec_type();
        if codec_type.is_video() && !found_video {
            found_video = true;
            info.width = par.width;
            info.height = par.height;
            if let Some(fr) = stream.guess_framerate() {
                if fr.num > 0 && fr.den > 0 {
                    info.fps_num = fr.num;
                    info.fps_den = fr.den;
                }
            }
            if info.fps_num == 0 {
                let afr = stream.avg_frame_rate;
                if afr.num > 0 && afr.den > 0 {
                    info.fps_num = afr.num;
                    info.fps_den = afr.den;
                }
            }
            if par.bit_rate > 0 {
                info.video_bitrate_bps = par.bit_rate;
            }
            if let Some(codec) = AVCodec::find_decoder(par.codec_id) {
                info.video_codec = codec.name().to_string_lossy().into_owned();
            }
        } else if codec_type.is_audio() {
            info.audio_tracks += 1;
            if !info.has_audio {
                info.has_audio = true;
                if par.bit_rate > 0 {
                    info.audio_bitrate_bps = par.bit_rate;
                }
            }
        }
    }

    if !found_video {
        return Err(anyhow!("no video stream found in {}", path.display()));
    }
    if info.duration_s <= 0.0 {
        return Err(anyhow!(
            "could not determine duration of {} (required for size targeting)",
            path.display()
        ));
    }
    if info.fps_num == 0 {
        // Last-resort fallback; CFR normalization still needs a target rate.
        info.fps_num = 30;
        info.fps_den = 1;
    }

    // MP4 frequently leaves the per-stream bitrate at zero. Fall back to the
    // container's overall rate, or failing that the file size, minus audio.
    if info.video_bitrate_bps == 0 {
        let total_bps = if ifmt.bit_rate > 0 {
            ifmt.bit_rate
        } else {
            std::fs::metadata(path)
                .map(|m| ((m.len() as f64 * 8.0) / info.duration_s) as i64)
                .unwrap_or(0)
        };
        if total_bps > 0 {
            info.video_bitrate_bps = (total_bps - info.audio_bitrate_bps).max(0);
        }
    }

    Ok(info)
}
