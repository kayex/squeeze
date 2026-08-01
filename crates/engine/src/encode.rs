//! One transcode pass: decode -> (scale + CFR + yuv420p) filter -> H.264
//! encoder (NVENC by default) -> MP4 mux with faststart. Audio is stream-copied.
//!
//! Structure follows rsmpeg's own `tests/ffmpeg_examples/transcode.rs`, adapted
//! to a single video stream (transcoded) plus an optional audio stream (copied).

use crate::plan::EncodePlan;
use crate::probe::MediaInfo;
use anyhow::{anyhow, bail, Context, Result};
use rsmpeg::{
    avcodec::{AVCodec, AVCodecContext},
    avfilter::{AVFilter, AVFilterContextMut, AVFilterGraph, AVFilterInOut},
    avformat::{AVFormatContextInput, AVFormatContextOutput},
    avutil::{av_rescale_q, ra, AVDictionary, AVFrame},
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
fn encoder_opens(name: &CStr, kind: EncoderKind) -> bool {
    let Some(codec) = AVCodec::find_encoder_by_name(name) else {
        return false;
    };
    let mut ctx = AVCodecContext::new(&codec);
    ctx.set_width(320);
    ctx.set_height(240);
    ctx.set_pix_fmt(ffi::AV_PIX_FMT_YUV420P);
    ctx.set_time_base(ra(1, 30));
    ctx.set_framerate(ra(30, 1));
    ctx.set_bit_rate(200_000);
    ctx.open(encoder_options(kind)).is_ok()
}

pub fn transcode(
    input: &CStr,
    output: &CStr,
    plan: &EncodePlan,
    info: &MediaInfo,
    encoder_name: &CStr,
    encoder_kind: EncoderKind,
    copy_audio: bool,
    on_progress: &mut dyn FnMut(f32),
) -> Result<()> {
    // ---- input + video decoder ----
    let mut ifmt = AVFormatContextInput::open(input).context("open input")?;

    let mut video_in: Option<usize> = None;
    let mut audio_in: Option<usize> = None;
    for (i, stream) in ifmt.streams().iter().enumerate() {
        let t = stream.codecpar().codec_type();
        if t.is_video() && video_in.is_none() {
            video_in = Some(i);
        } else if t.is_audio() && audio_in.is_none() {
            audio_in = Some(i);
        }
    }
    let video_in = video_in.context("input has no video stream")?;

    // Cache time bases / params before we start borrowing ifmt mutably for reads.
    let audio_in_tb = audio_in.map(|i| ifmt.streams()[i].time_base);
    let video_in_tb = ifmt.streams()[video_in].time_base;
    let audio_par = audio_in.map(|i| ifmt.streams()[i].codecpar().clone());

    let mut dec_ctx = {
        let stream = &ifmt.streams()[video_in];
        let par = stream.codecpar();
        let decoder =
            AVCodec::find_decoder(par.codec_id).context("no decoder for input video codec")?;
        let mut ctx = AVCodecContext::new(&decoder);
        ctx.apply_codecpar(&par).context("apply codecpar")?;
        ctx.set_pkt_timebase(stream.time_base);
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
    unsafe {
        let e = enc_ctx.as_mut_ptr();
        let d = dec_ctx.as_ptr();
        (*e).rc_max_rate = (plan.video_bitrate_bps as f64 * 1.15) as i64;
        (*e).rc_buffer_size = (plan.video_bitrate_bps as f64 * 2.0) as i32;
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

    // Optional audio output stream (stream-copied).
    let audio_out = if copy_audio {
        audio_par.as_ref().map(|par| {
            let mut stream = ofmt.new_stream();
            stream.set_codecpar(par.clone());
            stream.index as usize
        })
    } else {
        None
    };

    // ---- video filter graph: scale + CFR + yuv420p ----
    let mut graph = AVFilterGraph::new();
    let spec = video_filter_spec(plan);
    let (mut buffersrc_ctx, mut buffersink_ctx) =
        init_video_filter(&mut graph, &dec_ctx, &enc_ctx, &spec).context("init video filter")?;

    // ---- write header with faststart, then cache muxer-assigned time bases ----
    let mut header_opts = Some(AVDictionary::new(c"movflags", c"+faststart", 0));
    ofmt.write_header(&mut header_opts).context("write header")?;
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
            dec_ctx.send_packet(Some(&packet)).context("decode submit")?;
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
        } else if Some(idx) == audio_in {
            if let (Some(out), Some(in_tb), Some(out_tb)) = (audio_out, audio_in_tb, audio_out_tb) {
                packet.rescale_ts(in_tb, out_tb);
                packet.set_stream_index(out as i32);
                packet.set_pos(-1);
                ofmt.interleaved_write_frame(&mut packet)
                    .context("write audio packet")?;
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
        EncoderKind::OpenH264 => None,
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
    enc_ctx.send_frame(frame.as_ref()).context("encode submit")?;
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
