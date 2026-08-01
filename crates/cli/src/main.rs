//! squeeze-cli: the headless companion to the squeeze GUI.
//!
//! Usage:
//!   squeeze-cli [OPTIONS] <INPUT>...
//!
//! Compresses each input to an MP4 (H.264 + faststart) that fits under a size
//! ceiling, written next to the input with a suffix. The encode runs in-process
//! via libav* (NVENC by default); no ffmpeg.exe is invoked.

use anyhow::{bail, Context, Result};
use engine::{compress_to_target, CompressOptions, Encoder};
use std::path::{Path, PathBuf};

struct Args {
    inputs: Vec<PathBuf>,
    max_mb: f64,
    encoder: Encoder,
    passes: u32,
    suffix: String,
    outdir: Option<PathBuf>,
    keep_fps: bool,
    keep_resolution: bool,
    no_audio: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = parse_args()?;

    let opts = CompressOptions {
        max_bytes: (args.max_mb * 1_000_000.0) as u64,
        max_passes: args.passes,
        encoder: args.encoder,
        keep_fps: args.keep_fps,
        keep_resolution: args.keep_resolution,
        include_audio: !args.no_audio,
        ..Default::default()
    };

    let mut failures = 0;
    for input in &args.inputs {
        if let Err(e) = process_one(input, &args, &opts) {
            eprintln!("  failed: {e:#}");
            failures += 1;
        }
    }

    if failures > 0 {
        bail!("{failures} of {} file(s) failed", args.inputs.len());
    }
    Ok(())
}

fn process_one(input: &Path, args: &Args, opts: &CompressOptions) -> Result<()> {
    let output = output_path(input, &args.suffix, args.outdir.as_deref());
    if output == input {
        bail!(
            "refusing to overwrite the source file ({})",
            input.display()
        );
    }

    let source_bytes = std::fs::metadata(input)
        .with_context(|| format!("cannot read {}", input.display()))?
        .len();

    println!("==> {}", input.display());
    println!("    source: {}", human_mb(source_bytes));

    // The callback now fires continuously; only announce each new pass.
    let mut announced = 0u32;
    let outcome = compress_to_target(input, &output, opts, |p| {
        if p.pass != announced {
            announced = p.pass;
            println!(
                "    pass {}/{}: {}x{} @ {:.3} fps, video {} kbps [{}]",
                p.pass,
                p.max_passes,
                p.plan.width,
                p.plan.height,
                p.plan.fps(),
                p.plan.video_bitrate_bps / 1000,
                p.encoder,
            );
        }
    })?;

    let pct = 100.0 * outcome.final_bytes as f64 / source_bytes.max(1) as f64;
    let verdict = if outcome.fits {
        "✓ fits"
    } else {
        "✗ STILL OVER LIMIT"
    };
    println!(
        "    -> {} : {} ({pct:.1}% of source) in {} pass(es) {verdict}",
        outcome.output.display(),
        human_mb(outcome.final_bytes),
        outcome.passes,
    );
    Ok(())
}

fn output_path(input: &Path, suffix: &str, outdir: Option<&Path>) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let dir = outdir
        .map(Path::to_path_buf)
        .or_else(|| input.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join(format!("{stem}{suffix}.mp4"))
}

fn human_mb(bytes: u64) -> String {
    format!("{:.2} MB", bytes as f64 / 1_000_000.0)
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        inputs: Vec::new(),
        max_mb: 10.0,
        encoder: Encoder::Auto,
        passes: 3,
        suffix: "_discord".to_string(),
        outdir: None,
        keep_fps: false,
        keep_resolution: false,
        no_audio: false,
    };

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "--max-mb" => {
                args.max_mb = next_val(&mut it, "--max-mb")?
                    .parse()
                    .context("--max-mb must be a number")?;
            }
            "--encoder" => {
                let v = next_val(&mut it, "--encoder")?;
                args.encoder = match v.as_str() {
                    "auto" => Encoder::Auto,
                    "nvenc" => Encoder::Nvenc,
                    "x264" => Encoder::X264,
                    "openh264" => Encoder::OpenH264,
                    other => bail!("unknown --encoder '{other}' (auto|nvenc|x264|openh264)"),
                };
            }
            "--passes" => {
                args.passes = next_val(&mut it, "--passes")?
                    .parse()
                    .context("--passes must be an integer")?;
            }
            "--suffix" => args.suffix = next_val(&mut it, "--suffix")?,
            "-o" | "--outdir" => args.outdir = Some(PathBuf::from(next_val(&mut it, "--outdir")?)),
            "--keep-fps" => args.keep_fps = true,
            "--keep-resolution" => args.keep_resolution = true,
            "--no-audio" => args.no_audio = true,
            s if s.starts_with('-') && s != "-" => bail!("unknown option '{s}' (try --help)"),
            _ => args.inputs.push(PathBuf::from(arg)),
        }
    }

    if args.inputs.is_empty() {
        print_help();
        bail!("no input files given");
    }
    if args.passes == 0 {
        bail!("--passes must be >= 1");
    }
    Ok(args)
}

fn next_val(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    it.next().with_context(|| format!("{flag} needs a value"))
}

fn print_help() {
    eprintln!(
        "Squeeze: compress gameplay clips to fit under a size limit (for Discord)\n\
\n\
USAGE:\n\
    squeeze-cli [OPTIONS] <INPUT>...\n\
\n\
OPTIONS:\n\
    --max-mb <MB>        Size ceiling in MB (default: 10.0 = Discord free tier)\n\
    --encoder <E>        auto | nvenc | x264 | openh264 (default: auto -> NVENC)\n\
    --passes <N>         Max measure/re-encode passes (default: 3)\n\
    --suffix <S>         Output filename suffix (default: _discord)\n\
    -o, --outdir <DIR>   Output directory (default: alongside each input)\n\
    --keep-fps           Don't drop >45fps to 30fps when bits are tight\n\
    --keep-resolution    Don't scale the frame down when bits are tight\n\
    --no-audio           Drop audio instead of stream-copying it\n\
    -h, --help           Show this help\n\
\n\
Each <INPUT> is written to <stem><suffix>.mp4 (H.264 High, AAC copy, faststart)."
    );
}
