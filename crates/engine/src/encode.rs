//! One transcode pass: decode -> (scale + CFR + yuv420p) filter -> H.264
//! encoder (NVENC by default) -> MP4 mux with faststart. Audio is copied when
//! that is affordable and re-encoded to AAC when it is not.
//!
//! Structure follows rsmpeg's own `tests/ffmpeg_examples/transcode.rs`, adapted
//! to a single video stream plus an optional audio stream.

use crate::plan::{AudioAction, EncodePlan};
use crate::probe::MediaInfo;
use anyhow::{anyhow, bail, Context, Result};
use rsmpeg::{
    avcodec::{AVCodec, AVCodecContext, AVCodecParameters},
    avfilter::{AVFilter, AVFilterContextMut, AVFilterGraph, AVFilterInOut},
    avformat::{AVFormatContextInput, AVFormatContextOutput},
    avutil::{av_rescale_q, ra, AVChannelLayout, AVDictionary, AVFrame},
    error::RsmpegError,
    ffi,
};
use std::ffi::{CStr, CString};

#[derive(Clone, Copy, Debug)]
pub enum EncoderKind {
    Nvenc,
    X264,
    OpenH264,
}

/// Pick the first H.264 encoder available in this FFmpeg build for the requested
/// preference.
///
/// For `Auto` each candidate is *opened*, not merely looked up by name: a
/// hardware encoder like `h264_nvenc` is compiled into the build whether or not
/// the machine has an NVIDIA GPU, and only fails when opened. Probing here is
/// what makes the fallback to a software encoder actually work.
///
/// An explicitly requested encoder is only checked for presence, so the caller
/// gets the encoder's real open error rather than a silent substitution.
pub fn resolve_encoder(choice: crate::Encoder) -> Result<(&'static CStr, EncoderKind)> {
    use crate::Encoder::*;
    let candidates: &[(&'static CStr, EncoderKind)] = match choice {
        Auto => &[
            (c"h264_nvenc", EncoderKind::Nvenc),
            (c"libx264", EncoderKind::X264),
            (c"libopenh264", EncoderKind::OpenH264),
        ],
        Nvenc => &[(c"h264_nvenc", EncoderKind::Nvenc)],
        X264 => &[(c"libx264", EncoderKind::X264)],
        OpenH264 => &[(c"libopenh264", EncoderKind::OpenH264)],
    };

    if !matches!(choice, Auto) {
        let (name, kind) = candidates[0];
        if AVCodec::find_encoder_by_name(name).is_some() {
            return Ok((name, kind));
        }
        bail!("encoder {:?} is not present in this FFmpeg build", name);
    }

    for (name, kind) in candidates {
        if encoder_opens(name, *kind) {
            return Ok((name, *kind));
        }
    }
    bail!(
        "no usable H.264 encoder on this machine. Tried h264_nvenc (needs an \
         NVIDIA GPU with a recent driver), libx264, and libopenh264."
    )
}

/// Try to actually open `name` with a minimal configuration. This is the only
/// reliable way to tell "encoder exists in the build" from "encoder works here".
/// Whether the NVIDIA runtime can be loaded at all.
///
/// Opening `h264_nvenc` on a machine with no driver crashed the process with an
/// access violation rather than failing cleanly, which killed the default
/// encoder selection for every user without an NVIDIA card. Loading the library
/// ourselves is a question FFmpeg is never asked, so it cannot answer it badly.
#[cfg(target_os = "windows")]
fn nvidia_runtime_present() -> bool {
    extern "system" {
        fn LoadLibraryA(name: *const i8) -> *mut std::ffi::c_void;
        fn FreeLibrary(handle: *mut std::ffi::c_void) -> i32;
    }
    // nvcuda.dll ships with the display driver; NVENC needs it before it needs
    // anything of its own.
    unsafe {
        let handle = LoadLibraryA(c"nvcuda.dll".as_ptr());
        if handle.is_null() {
            return false;
        }
        FreeLibrary(handle);
        true
    }
}

#[cfg(target_os = "linux")]
fn nvidia_runtime_present() -> bool {
    extern "C" {
        fn dlopen(file: *const i8, mode: i32) -> *mut std::ffi::c_void;
        fn dlclose(handle: *mut std::ffi::c_void) -> i32;
    }
    const RTLD_LAZY: i32 = 1;
    unsafe {
        let handle = dlopen(c"libcuda.so.1".as_ptr(), RTLD_LAZY);
        if handle.is_null() {
            return false;
        }
        dlclose(handle);
        true
    }
}

/// No NVENC on macOS, so nothing to guard.
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn nvidia_runtime_present() -> bool {
    false
}

fn encoder_opens(name: &CStr, kind: EncoderKind) -> bool {
    // Checked before the codec is even looked up: the point is to never hand
    // nvenc to avcodec_open2 on a machine that cannot support it.
    if matches!(kind, EncoderKind::Nvenc) && !nvidia_runtime_present() {
        trace_probe(name, "skipped, no NVIDIA runtime");
        return false;
    }
    let Some(codec) = AVCodec::find_encoder_by_name(name) else {
        trace_probe(name, "not in this build");
        return false;
    };
    let mut ctx = AVCodecContext::new(&codec);
    ctx.set_width(320);
    ctx.set_height(240);
    ctx.set_pix_fmt(ffi::AV_PIX_FMT_YUV420P);
    ctx.set_time_base(ra(1, 30));
    ctx.set_framerate(ra(30, 1));
    ctx.set_bit_rate(200_000);
    let opened = ctx.open(encoder_options(kind)).is_ok();
    trace_probe(name, if opened { "opened" } else { "would not open" });
    opened
}

/// Why a candidate was passed over. Silent unless `SQUEEZE_TRACE_ENCODER` is
/// set: selection happens before any progress is reported, so when it goes
/// wrong there is otherwise nothing at all to look at.
fn trace_probe(name: &CStr, outcome: &str) {
    if std::env::var_os("SQUEEZE_TRACE_ENCODER").is_some() {
        eprintln!("encoder probe: {} {}", name.to_string_lossy(), outcome);
    }
}

pub fn transcode(
    input: &CStr,
    output: &CStr,
    plan: &EncodePlan,
    info: &MediaInfo,
    encoder_name: &CStr,
    encoder_kind: EncoderKind,
    on_progress: &mut dyn FnMut(f32),
) -> Result<()> {
    // ---- input + video decoder ----
    let mut ifmt = AVFormatContextInput::open(input).context("open input")?;

    let mut video_in: Option<usize> = None;
    // Every audio stream, not just the first: a capture that keeps game sound
    // and microphone apart carries two, and dropping one loses half the sound.
    let mut audio_ins: Vec<usize> = Vec::new();
    for (i, stream) in ifmt.streams().iter().enumerate() {
        let t = stream.codecpar().codec_type();
        if t.is_video() && video_in.is_none() {
            video_in = Some(i);
        } else if t.is_audio() {
            audio_ins.push(i);
        }
    }
    let video_in = video_in.context("input has no video stream")?;

    // Cache time bases / params before we start borrowing ifmt mutably for reads.
    let audio_in = audio_ins.first().copied();
    let audio_in_tb = audio_in.map(|i| ifmt.streams()[i].time_base);
    let video_in_tb = ifmt.streams()[video_in].time_base;
    let audio_par = audio_in.map(|i| ifmt.streams()[i].codecpar().clone());
    let audio_tracks: Vec<(usize, AVCodecParameters, ffi::AVRational)> = audio_ins
        .iter()
        .map(|&i| {
            (
                i,
                ifmt.streams()[i].codecpar().clone(),
                ifmt.streams()[i].time_base,
            )
        })
        .collect();

    let mut dec_ctx = {
        let stream = &ifmt.streams()[video_in];
        let par = stream.codecpar();
        let decoder =
            AVCodec::find_decoder(par.codec_id).context("no decoder for input video codec")?;
        let mut ctx = AVCodecContext::new(&decoder);
        ctx.apply_codecpar(&par).context("apply codecpar")?;
        ctx.set_pkt_timebase(stream.time_base);
        // libavcodec decodes on ONE thread unless told otherwise, and one core
        // cannot keep pace with a high-bitrate ShadowPlay capture: a 98 Mbit/s
        // 1440p clip decoded ~4x slower than realtime and stalled NVENC behind
        // it. Zero sizes the pool from the CPU count. No typed setter in
        // rsmpeg, hence the raw pointer.
        unsafe {
            (*ctx.as_mut_ptr()).thread_count = 0;
        }
        if let Some(fr) = stream.guess_framerate() {
            ctx.set_framerate(fr);
        }
        ctx.open(None).context("open video decoder")?;
        ctx
    };

    // ---- output muxer + video encoder ----
    let mut ofmt = AVFormatContextOutput::create(output).context("create output")?;

    let encoder = AVCodec::find_encoder_by_name(encoder_name)
        .with_context(|| anyhow!("encoder {:?} not found in build", encoder_name))?;
    let mut enc_ctx = AVCodecContext::new(&encoder);
    enc_ctx.set_width(plan.width);
    enc_ctx.set_height(plan.height);
    enc_ctx.set_pix_fmt(ffi::AV_PIX_FMT_YUV420P);
    enc_ctx.set_sample_aspect_ratio(dec_ctx.sample_aspect_ratio);
    // time_base = 1/fps, framerate = fps  => true CFR output.
    enc_ctx.set_time_base(ra(plan.fps_den.max(1), plan.fps_num));
    enc_ctx.set_framerate(ra(plan.fps_num, plan.fps_den.max(1)));
    enc_ctx.set_bit_rate(plan.video_bitrate_bps);
    let gop = (plan.fps() * 2.0).round() as i32;
    enc_ctx.set_gop_size(gop.max(1));
    enc_ctx.set_max_b_frames(match encoder_kind {
        EncoderKind::OpenH264 => 0, // Constrained Baseline: no B-frames
        _ => 3,
    });

    // Rate-control ceiling + carry source color metadata. These fields have no
    // typed setter in rsmpeg, so write them through the raw pointer.
    //
    // The ceiling is deliberately loose. A tight one (1.15x average over a 2s
    // buffer) made x264 log a stream of "VBV underflow" warnings at the low
    // bitrates targeted here, where a single keyframe can outweigh the whole
    // buffer. Nothing downstream cares: x264 only writes HRD parameters into
    // the SPS when nal-hrd is asked for, which it is not, so the buffer is
    // never signalled to a decoder. It exists only to stop one pass
    // overshooting the size target badly enough to cost an extra pass, and a
    // loose bound does that just as well while leaving room to spend bits on
    // a hard scene.
    unsafe {
        let e = enc_ctx.as_mut_ptr();
        let d = dec_ctx.as_ptr();
        (*e).rc_max_rate = (plan.video_bitrate_bps as f64 * 2.0) as i64;
        (*e).rc_buffer_size = (plan.video_bitrate_bps as f64 * 4.0) as i32;
        (*e).color_range = (*d).color_range;
        (*e).colorspace = (*d).colorspace;
        (*e).color_primaries = (*d).color_primaries;
        (*e).color_trc = (*d).color_trc;
    }

    if ofmt.oformat().flags & ffi::AVFMT_GLOBALHEADER as i32 != 0 {
        enc_ctx.set_flags(enc_ctx.flags | ffi::AV_CODEC_FLAG_GLOBAL_HEADER as i32);
    }

    let enc_opts = encoder_options(encoder_kind);
    let leftover = enc_ctx.open(enc_opts).with_context(|| {
        anyhow!(
            "failed to open encoder {:?}. If h264_nvenc: confirm an NVIDIA GPU and a recent \
             driver are present, or retry with --encoder x264.",
            encoder_name
        )
    })?;
    if let Some(left) = leftover {
        if let Ok(s) = left.get_string(b'=', b',') {
            let s = s.to_string_lossy();
            if !s.is_empty() {
                eprintln!("warning: encoder ignored options: {s}");
            }
        }
    }

    // Video output stream (index 0).
    let video_out;
    {
        let mut stream = ofmt.new_stream();
        stream.set_codecpar(enc_ctx.extract_codecpar());
        stream.set_time_base(enc_ctx.time_base);
        video_out = stream.index as usize;
    }

    // Optional audio output stream: either the source parameters copied across
    // verbatim, or an AAC encoder the source is fed through.
    let mut audio_enc: Option<AudioEncode> = None;
    let audio_out = match plan.audio {
        AudioAction::Drop => None,
        AudioAction::Copy => audio_par.as_ref().map(|par| {
            let mut stream = ofmt.new_stream();
            stream.set_codecpar(par.clone());
            stream.index as usize
        }),
        AudioAction::Reencode { bps } => match audio_tracks.is_empty() {
            false => {
                let mut a = AudioEncode::new(&audio_tracks, bps)?;
                let mut stream = ofmt.new_stream();
                stream.set_codecpar(a.enc_ctx.extract_codecpar());
                stream.set_time_base(a.enc_ctx.time_base);
                a.out_index = stream.index as usize;
                let out = a.out_index;
                audio_enc = Some(a);
                Some(out)
            }
            true => None,
        },
    };

    // ---- video filter graph: scale + CFR + yuv420p ----
    let mut graph = AVFilterGraph::new();
    let spec = video_filter_spec(plan);
    let (mut buffersrc_ctx, mut buffersink_ctx) =
        init_video_filter(&mut graph, &dec_ctx, &enc_ctx, &spec).context("init video filter")?;

    // ---- write header with faststart, then cache muxer-assigned time bases ----
    let mut header_opts = Some(AVDictionary::new(c"movflags", c"+faststart", 0));
    ofmt.write_header(&mut header_opts)
        .context("write header")?;
    let audio_out_tb = audio_out.map(|i| ofmt.streams()[i].time_base);

    // ---- main packet loop ----
    while let Some(mut packet) = ifmt.read_packet().context("read packet")? {
        let idx = packet.stream_index as usize;
        if idx == video_in {
            // Read position through the source is a good proxy for pass progress.
            if packet.pts != ffi::AV_NOPTS_VALUE && info.duration_s > 0.0 {
                let secs =
                    packet.pts as f64 * video_in_tb.num as f64 / video_in_tb.den.max(1) as f64;
                on_progress((secs / info.duration_s).clamp(0.0, 1.0) as f32);
            }
            dec_ctx
                .send_packet(Some(&packet))
                .context("decode submit")?;
            loop {
                let mut frame = match dec_ctx.receive_frame() {
                    Ok(f) => f,
                    Err(RsmpegError::DecoderDrainError) | Err(RsmpegError::DecoderFlushedError) => {
                        break
                    }
                    Err(e) => bail!(e),
                };
                frame.set_pts(frame.best_effort_timestamp);
                filter_encode_write(
                    Some(frame),
                    &mut buffersrc_ctx,
                    &mut buffersink_ctx,
                    &mut enc_ctx,
                    &mut ofmt,
                    video_out,
                )?;
            }
        } else if audio_ins.contains(&idx) {
            if let Some(a) = audio_enc.as_mut() {
                a.push_packet(idx, &packet, &mut ofmt)?;
            } else if Some(idx) == audio_in {
                if let (Some(out), Some(in_tb), Some(out_tb)) =
                    (audio_out, audio_in_tb, audio_out_tb)
                {
                    packet.rescale_ts(in_tb, out_tb);
                    packet.set_stream_index(out as i32);
                    packet.set_pos(-1);
                    ofmt.interleaved_write_frame(&mut packet)
                        .context("write audio packet")?;
                }
            }
        }
    }

    // ---- flush: decoder -> filter -> encoder ----
    dec_ctx.send_packet(None).context("decoder flush submit")?;
    loop {
        let mut frame = match dec_ctx.receive_frame() {
            Ok(f) => f,
            Err(RsmpegError::DecoderDrainError) | Err(RsmpegError::DecoderFlushedError) => break,
            Err(e) => bail!(e),
        };
        frame.set_pts(frame.best_effort_timestamp);
        filter_encode_write(
            Some(frame),
            &mut buffersrc_ctx,
            &mut buffersink_ctx,
            &mut enc_ctx,
            &mut ofmt,
            video_out,
        )?;
    }
    // EOF into the filter graph, then drain it.
    filter_encode_write(
        None,
        &mut buffersrc_ctx,
        &mut buffersink_ctx,
        &mut enc_ctx,
        &mut ofmt,
        video_out,
    )?;
    flush_encoder(&mut enc_ctx, &mut ofmt, video_out)?;
    if let Some(a) = audio_enc.as_mut() {
        a.finish(&mut ofmt)?;
    }

    ofmt.write_trailer().context("write trailer")?;
    Ok(())
}

fn encoder_options(kind: EncoderKind) -> Option<AVDictionary> {
    match kind {
        EncoderKind::Nvenc => Some(
            AVDictionary::new(c"preset", c"p5", 0)
                .set(c"tune", c"hq", 0)
                .set(c"rc", c"vbr", 0)
                .set(c"multipass", c"fullres", 0)
                .set(c"profile", c"high", 0)
                .set(c"spatial-aq", c"1", 0)
                .set(c"temporal-aq", c"1", 0)
                .set(c"b_ref_mode", c"middle", 0)
                .set(c"rc-lookahead", c"20", 0),
        ),
        EncoderKind::X264 => {
            Some(AVDictionary::new(c"preset", c"medium", 0).set(c"profile", c"high", 0))
        }
        // openh264 defaults to a quality-targeted mode that ignores the
        // bitrate entirely, and says so: "bitrate can't be controlled ...
        // without enabling skip frame". Left at the default it overshoots the
        // ceiling on every pass and the size loop cannot converge, which is
        // exactly what a size-targeted encoder must not do. Asking for the
        // bitrate mode and letting it drop frames when it must is the trade
        // this tool wants: a skipped frame beats a file too big to send.
        EncoderKind::OpenH264 => {
            Some(AVDictionary::new(c"rc_mode", c"bitrate", 0).set(c"allow_skip_frames", c"1", 0))
        }
    }
}

/// `scale=W:H` (exact, matches encoder dims) + `fps` (VFR -> CFR) + force yuv420p.
fn video_filter_spec(plan: &EncodePlan) -> CString {
    let spec = format!(
        "scale={w}:{h}:flags=bicubic,fps={fn}/{fd},format=pix_fmts=yuv420p",
        w = plan.width,
        h = plan.height,
        fn = plan.fps_num,
        fd = plan.fps_den.max(1),
    );
    CString::new(spec).expect("filter spec has no interior NUL")
}

/// Copy the streams into an MP4 without touching the pixels.
///
/// Used when the source already fits the ceiling: re-encoding it would spend a
/// generation of quality and, because the target bitrate is clamped to the
/// source's own, come out about the same size regardless. Also upgrades an MKV
/// wrapper to MP4 and adds faststart on the way past.
pub fn remux(input: &CStr, output: &CStr, include_audio: bool, mix_bps: Option<i64>) -> Result<()> {
    let mut ifmt = AVFormatContextInput::open(input).context("open input")?;
    let mut ofmt = AVFormatContextOutput::create(output).context("create output")?;

    let n = ifmt.streams().len();
    let mut in_tb: Vec<ffi::AVRational> = Vec::with_capacity(n);
    let mut video_in: Option<usize> = None;
    let mut audio_ins: Vec<usize> = Vec::new();
    for i in 0..n {
        let stream = &ifmt.streams()[i];
        in_tb.push(stream.time_base);
        let kind = stream.codecpar().codec_type();
        if kind.is_video() && video_in.is_none() {
            video_in = Some(i);
        } else if kind.is_audio() && include_audio {
            audio_ins.push(i);
        }
    }
    let video_in = video_in.context("input has no video stream")?;

    // Video is always copied here: that is the whole point of this path.
    let video_out = {
        let (par, tb) = {
            let stream = &ifmt.streams()[video_in];
            (stream.codecpar().clone(), stream.time_base)
        };
        let mut out = ofmt.new_stream();
        out.set_codecpar(par);
        out.set_time_base(tb);
        // The source tag belongs to the source container and can be
        // meaningless in MP4; zero lets the muxer choose the right one.
        unsafe {
            (*(*out.as_mut_ptr()).codecpar).codec_tag = 0;
        }
        out.index as usize
    };

    // Audio can only be copied when there is a single track. Several have to be
    // mixed, and mixing means encoding even though the picture is untouched.
    let mut audio_enc: Option<AudioEncode> = None;
    let mut audio_copy_from: Option<usize> = None;
    let audio_out = match (audio_ins.len(), mix_bps) {
        (0, _) => None,
        (1, _) => {
            let (par, tb) = {
                let stream = &ifmt.streams()[audio_ins[0]];
                (stream.codecpar().clone(), stream.time_base)
            };
            let mut out = ofmt.new_stream();
            out.set_codecpar(par);
            out.set_time_base(tb);
            unsafe {
                (*(*out.as_mut_ptr()).codecpar).codec_tag = 0;
            }
            audio_copy_from = Some(audio_ins[0]);
            Some(out.index as usize)
        }
        (_, Some(bps)) => {
            let tracks: Vec<(usize, AVCodecParameters, ffi::AVRational)> = audio_ins
                .iter()
                .map(|&i| {
                    (
                        i,
                        ifmt.streams()[i].codecpar().clone(),
                        ifmt.streams()[i].time_base,
                    )
                })
                .collect();
            let mut a = AudioEncode::new(&tracks, bps)?;
            let mut stream = ofmt.new_stream();
            stream.set_codecpar(a.enc_ctx.extract_codecpar());
            stream.set_time_base(a.enc_ctx.time_base);
            a.out_index = stream.index as usize;
            let out = a.out_index;
            audio_enc = Some(a);
            Some(out)
        }
        // Several tracks but no mix rate asked for: keep the first rather than
        // inventing one.
        (_, None) => {
            let (par, tb) = {
                let stream = &ifmt.streams()[audio_ins[0]];
                (stream.codecpar().clone(), stream.time_base)
            };
            let mut out = ofmt.new_stream();
            out.set_codecpar(par);
            out.set_time_base(tb);
            unsafe {
                (*(*out.as_mut_ptr()).codecpar).codec_tag = 0;
            }
            audio_copy_from = Some(audio_ins[0]);
            Some(out.index as usize)
        }
    };

    let mut header_opts = Some(AVDictionary::new(c"movflags", c"+faststart", 0));
    ofmt.write_header(&mut header_opts)
        .context("write header")?;
    let out_tb: Vec<ffi::AVRational> = ofmt.streams().iter().map(|s| s.time_base).collect();

    while let Some(mut pkt) = ifmt.read_packet().context("read packet")? {
        let i = pkt.stream_index as usize;
        if i == video_in {
            pkt.rescale_ts(in_tb[i], out_tb[video_out]);
            pkt.set_stream_index(video_out as i32);
            pkt.set_pos(-1);
            ofmt.interleaved_write_frame(&mut pkt)
                .context("write video packet")?;
        } else if audio_ins.contains(&i) {
            if let Some(a) = audio_enc.as_mut() {
                a.push_packet(i, &pkt, &mut ofmt)?;
            } else if Some(i) == audio_copy_from {
                if let Some(out) = audio_out {
                    pkt.rescale_ts(in_tb[i], out_tb[out]);
                    pkt.set_stream_index(out as i32);
                    pkt.set_pos(-1);
                    ofmt.interleaved_write_frame(&mut pkt)
                        .context("write audio packet")?;
                }
            }
        }
    }
    if let Some(a) = audio_enc.as_mut() {
        a.finish(&mut ofmt)?;
    }
    ofmt.write_trailer().context("write trailer")?;
    Ok(())
}

/// The audio re-encode path: source track -> decoder -> sample-format and rate
/// conversion -> AAC at a chosen bitrate.
///
/// Conversion runs through a filter graph rather than a resampler plus a
/// hand-written sample buffer, because AAC will only accept frames of exactly
/// `frame_size` samples and `buffersink_set_frame_size` already chunks them
/// that way. The graph is stored and its two endpoints looked up by name on
/// each call: holding the endpoint handles next to the graph they borrow from
/// would make this struct self-referential.
struct AudioInput {
    stream_index: usize,
    dec_ctx: AVCodecContext,
}

struct AudioEncode {
    inputs: Vec<AudioInput>,
    enc_ctx: AVCodecContext,
    graph: AVFilterGraph,
    out_index: usize,
}

impl AudioEncode {
    /// One decoder per source track, all meeting at an `amix` when there is
    /// more than one.
    fn new(tracks: &[(usize, AVCodecParameters, ffi::AVRational)], bps: i64) -> Result<Self> {
        let mut inputs = Vec::with_capacity(tracks.len());
        for (stream_index, par, tb) in tracks {
            let decoder =
                AVCodec::find_decoder(par.codec_id).context("no decoder for source audio")?;
            let mut dec_ctx = AVCodecContext::new(&decoder);
            dec_ctx
                .apply_codecpar(par)
                .context("apply audio codecpar")?;
            dec_ctx.set_pkt_timebase(*tb);
            dec_ctx.open(None).context("open audio decoder")?;
            inputs.push(AudioInput {
                stream_index: *stream_index,
                dec_ctx,
            });
        }
        if inputs.is_empty() {
            bail!("no audio tracks to encode");
        }

        let encoder = AVCodec::find_encoder_by_name(c"aac").context("no AAC encoder in build")?;
        let mut enc_ctx = AVCodecContext::new(&encoder);
        let rate = if inputs[0].dec_ctx.sample_rate > 0 {
            inputs[0].dec_ctx.sample_rate
        } else {
            48_000
        };
        // Rebuilt from the channel count rather than copied: a source layout may
        // own a heap allocation that must not be aliased by a second context.
        let channels = inputs[0].dec_ctx.ch_layout().nb_channels.clamp(1, 8);
        let layout = AVChannelLayout::from_nb_channels(channels);
        enc_ctx.set_ch_layout(layout.into_inner());
        enc_ctx.set_sample_rate(rate);
        enc_ctx.set_sample_fmt(ffi::AV_SAMPLE_FMT_FLTP);
        enc_ctx.set_bit_rate(bps);
        enc_ctx.set_time_base(ra(1, rate));
        enc_ctx.open(None).context("open AAC encoder")?;

        let mut graph = AVFilterGraph::new();
        Self::init_filter(&mut graph, &inputs, &enc_ctx, rate, channels)?;
        Ok(Self {
            inputs,
            enc_ctx,
            graph,
            out_index: 0,
        })
    }

    fn init_filter(
        graph: &mut AVFilterGraph,
        inputs: &[AudioInput],
        enc_ctx: &AVCodecContext,
        rate: i32,
        channels: i32,
    ) -> Result<()> {
        let abuffer = AVFilter::get_by_name(c"abuffer").context("abuffer filter missing")?;
        let abuffersink =
            AVFilter::get_by_name(c"abuffersink").context("abuffersink filter missing")?;

        let mut srcs = Vec::with_capacity(inputs.len());
        let mut labels = String::new();
        for (i, input) in inputs.iter().enumerate() {
            let dec_ctx = &input.dec_ctx;
            let in_layout = dec_ctx
                .ch_layout()
                .describe()
                .unwrap_or_else(|_| CString::new("stereo").unwrap());
            let fmt_ptr = unsafe { ffi::av_get_sample_fmt_name(dec_ctx.sample_fmt) };
            let fmt_name = if fmt_ptr.is_null() {
                CString::new("fltp").unwrap()
            } else {
                unsafe { CStr::from_ptr(fmt_ptr) }.to_owned()
            };
            let args = CString::new(format!(
                "time_base={}/{}:sample_rate={}:sample_fmt={}:channel_layout={}",
                dec_ctx.pkt_timebase.num.max(1),
                dec_ctx.pkt_timebase.den.max(1),
                dec_ctx.sample_rate.max(1),
                fmt_name.to_string_lossy(),
                in_layout.to_string_lossy(),
            ))
            .unwrap();
            let name = CString::new(format!("ain{i}")).unwrap();
            srcs.push(
                graph
                    .create_filter_context(&abuffer, &name, Some(&args))
                    .context("create abuffer source")?,
            );
            labels.push_str(&format!("[ain{i}]"));
        }

        let mut sink = graph
            .alloc_filter_context(&abuffersink, c"aout")
            .context("alloc abuffer sink")?;
        sink.init_dict(&mut None).context("init abuffer sink")?;

        let out_layout = AVChannelLayout::from_nb_channels(channels)
            .describe()
            .unwrap_or_else(|_| CString::new("stereo").unwrap());
        // amix's default normalisation divides by the number of live inputs.
        // Summing without it measurably clips real game audio, which already
        // peaks near full scale, so the quieter-but-clean mix is the right one.
        let mix = if inputs.len() > 1 {
            format!("amix=inputs={}:duration=longest,", inputs.len())
        } else {
            String::new()
        };
        // Endpoints are labelled rather than left to positional matching, so
        // the graph is linked by name and cannot silently come out dangling.
        let spec = CString::new(format!(
            "{labels}{mix}aformat=sample_fmts=fltp:sample_rates={rate}:channel_layouts={}[aout]",
            out_layout.to_string_lossy()
        ))
        .unwrap();

        // One entry per source, chained head-first. rsmpeg exposes no list
        // builder, so the `next` links are written directly; ownership passes
        // into the chain, whose head frees the whole thing on drop.
        let mut outputs: Option<AVFilterInOut> = None;
        for (i, src) in srcs.iter_mut().enumerate().rev() {
            let name = CString::new(format!("ain{i}")).unwrap();
            let mut inout = AVFilterInOut::new(&name, src, 0);
            if let Some(rest) = outputs.take() {
                unsafe {
                    (*inout.as_mut_ptr()).next = rest.into_raw().as_ptr();
                }
            }
            outputs = Some(inout);
        }
        let inputs_io = AVFilterInOut::new(c"aout", &mut sink, 0);
        graph
            .parse_ptr(&spec, Some(inputs_io), outputs)
            .context("parse audio filter spec")?;
        graph.config().context("configure audio filter graph")?;

        // AAC takes a fixed number of samples per frame; let the sink do the
        // chunking rather than buffering partial frames by hand.
        if enc_ctx.frame_size > 0 {
            if let Some(mut sink) = graph.get_filter(c"aout") {
                sink.buffersink_set_frame_size(enc_ctx.frame_size as u32);
            }
        }
        Ok(())
    }

    /// Decode one packet from `stream_index` and push what comes out into that
    /// track's branch of the graph.
    fn push_packet(
        &mut self,
        stream_index: usize,
        packet: &rsmpeg::avcodec::AVPacket,
        ofmt: &mut AVFormatContextOutput,
    ) -> Result<()> {
        let Some(pos) = self
            .inputs
            .iter()
            .position(|i| i.stream_index == stream_index)
        else {
            return Ok(());
        };
        self.inputs[pos]
            .dec_ctx
            .send_packet(Some(packet))
            .context("audio decode submit")?;
        self.drain_decoder(pos, ofmt)
    }

    fn drain_decoder(&mut self, pos: usize, ofmt: &mut AVFormatContextOutput) -> Result<()> {
        loop {
            let frame = match self.inputs[pos].dec_ctx.receive_frame() {
                Ok(f) => f,
                Err(RsmpegError::DecoderDrainError) | Err(RsmpegError::DecoderFlushedError) => {
                    break
                }
                Err(e) => bail!(e),
            };
            self.feed(pos, Some(frame), ofmt)?;
        }
        Ok(())
    }

    /// Hand a frame (or EOF) to one branch, then take whatever the mix yields.
    fn feed(
        &mut self,
        pos: usize,
        frame: Option<AVFrame>,
        ofmt: &mut AVFormatContextOutput,
    ) -> Result<()> {
        {
            let name = CString::new(format!("ain{pos}")).unwrap();
            let mut src = self
                .graph
                .get_filter(&name)
                .ok_or_else(|| anyhow!("audio filter source {pos} vanished"))?;
            src.buffersrc_add_frame(frame, None)
                .context("feed audio filter")?;
        }
        self.pull(ofmt)
    }

    fn pull(&mut self, ofmt: &mut AVFormatContextOutput) -> Result<()> {
        loop {
            let filtered = {
                let mut sink = self
                    .graph
                    .get_filter(c"aout")
                    .ok_or_else(|| anyhow!("audio filter sink vanished"))?;
                match sink.buffersink_get_frame(None) {
                    Ok(f) => f,
                    Err(RsmpegError::BufferSinkDrainError)
                    | Err(RsmpegError::BufferSinkEofError) => break,
                    Err(e) => bail!(e),
                }
            };
            self.encode_write(Some(filtered), ofmt)?;
        }
        Ok(())
    }

    fn encode_write(
        &mut self,
        frame: Option<AVFrame>,
        ofmt: &mut AVFormatContextOutput,
    ) -> Result<()> {
        self.enc_ctx
            .send_frame(frame.as_ref())
            .context("audio encode submit")?;
        loop {
            let mut pkt = match self.enc_ctx.receive_packet() {
                Ok(p) => p,
                Err(RsmpegError::EncoderDrainError) | Err(RsmpegError::EncoderFlushedError) => {
                    break
                }
                Err(e) => bail!(e),
            };
            let out_tb = ofmt.streams()[self.out_index].time_base;
            pkt.rescale_ts(self.enc_ctx.time_base, out_tb);
            pkt.set_stream_index(self.out_index as i32);
            pkt.set_pos(-1);
            ofmt.interleaved_write_frame(&mut pkt)
                .context("write audio packet")?;
        }
        Ok(())
    }

    /// Drain every decoder, close every branch, then flush the encoder. All
    /// branches must reach EOF or the mix will keep waiting on one of them.
    fn finish(&mut self, ofmt: &mut AVFormatContextOutput) -> Result<()> {
        for pos in 0..self.inputs.len() {
            self.inputs[pos]
                .dec_ctx
                .send_packet(None)
                .context("audio decoder flush")?;
            self.drain_decoder(pos, ofmt)?;
            self.feed(pos, None, ofmt)?;
        }
        self.pull(ofmt)?;
        self.encode_write(None, ofmt)?;
        Ok(())
    }
}

/// Build buffer -> [spec] -> buffersink for the video stream.
fn init_video_filter<'g>(
    graph: &'g mut AVFilterGraph,
    dec_ctx: &AVCodecContext,
    enc_ctx: &AVCodecContext,
    filter_spec: &CStr,
) -> Result<(AVFilterContextMut<'g>, AVFilterContextMut<'g>)> {
    let buffersrc = AVFilter::get_by_name(c"buffer").context("buffer filter missing")?;
    let buffersink = AVFilter::get_by_name(c"buffersink").context("buffersink filter missing")?;

    let args = CString::new(format!(
        "video_size={}x{}:pix_fmt={}:time_base={}/{}:pixel_aspect={}/{}",
        dec_ctx.width,
        dec_ctx.height,
        dec_ctx.pix_fmt,
        dec_ctx.pkt_timebase.num,
        dec_ctx.pkt_timebase.den,
        dec_ctx.sample_aspect_ratio.num,
        dec_ctx.sample_aspect_ratio.den,
    ))
    .unwrap();

    let mut buffersrc_ctx = graph
        .create_filter_context(&buffersrc, c"in", Some(&args))
        .context("create buffer source")?;

    let mut buffersink_ctx = graph
        .alloc_filter_context(&buffersink, c"out")
        .context("alloc buffer sink")?;
    buffersink_ctx
        .opt_set_bin(c"pix_fmts", &enc_ctx.pix_fmt)
        .context("set buffersink pix_fmt")?;
    buffersink_ctx
        .init_dict(&mut None)
        .context("init buffer sink")?;

    // Endpoint naming mirrors the FFmpeg example: graph outputs feed "in",
    // graph inputs come from "out".
    let outputs = AVFilterInOut::new(c"in", &mut buffersrc_ctx, 0);
    let inputs = AVFilterInOut::new(c"out", &mut buffersink_ctx, 0);
    graph
        .parse_ptr(filter_spec, Some(inputs), Some(outputs))
        .context("parse filter spec")?;
    graph.config().context("configure filter graph")?;

    Ok((buffersrc_ctx, buffersink_ctx))
}

/// filter -> encode -> write for one decoded frame (or `None` to flush the graph).
fn filter_encode_write(
    frame: Option<AVFrame>,
    buffersrc_ctx: &mut AVFilterContextMut,
    buffersink_ctx: &mut AVFilterContextMut,
    enc_ctx: &mut AVCodecContext,
    ofmt: &mut AVFormatContextOutput,
    stream_index: usize,
) -> Result<()> {
    buffersrc_ctx
        .buffersrc_add_frame(frame, None)
        .context("submit frame to filtergraph")?;
    loop {
        let mut filtered = match buffersink_ctx.buffersink_get_frame(None) {
            Ok(f) => f,
            Err(RsmpegError::BufferSinkDrainError) | Err(RsmpegError::BufferSinkEofError) => break,
            Err(e) => bail!(e),
        };
        filtered.set_time_base(buffersink_ctx.get_time_base());
        filtered.set_pict_type(ffi::AV_PICTURE_TYPE_NONE);
        encode_write(Some(filtered), enc_ctx, ofmt, stream_index)?;
    }
    Ok(())
}

/// encode -> write for one filtered frame (or `None` to flush the encoder).
fn encode_write(
    mut frame: Option<AVFrame>,
    enc_ctx: &mut AVCodecContext,
    ofmt: &mut AVFormatContextOutput,
    stream_index: usize,
) -> Result<()> {
    if let Some(f) = frame.as_mut() {
        if f.pts != ffi::AV_NOPTS_VALUE {
            f.set_pts(av_rescale_q(f.pts, f.time_base, enc_ctx.time_base));
        }
    }
    enc_ctx
        .send_frame(frame.as_ref())
        .context("encode submit")?;
    loop {
        let mut pkt = match enc_ctx.receive_packet() {
            Ok(p) => p,
            Err(RsmpegError::EncoderDrainError) | Err(RsmpegError::EncoderFlushedError) => break,
            Err(e) => bail!(e),
        };
        pkt.set_stream_index(stream_index as i32);
        pkt.rescale_ts(enc_ctx.time_base, ofmt.streams()[stream_index].time_base);
        ofmt.interleaved_write_frame(&mut pkt)
            .context("write video packet")?;
    }
    Ok(())
}

fn flush_encoder(
    enc_ctx: &mut AVCodecContext,
    ofmt: &mut AVFormatContextOutput,
    stream_index: usize,
) -> Result<()> {
    if enc_ctx.codec().capabilities & ffi::AV_CODEC_CAP_DELAY as i32 == 0 {
        return Ok(());
    }
    encode_write(None, enc_ctx, ofmt, stream_index)
}
