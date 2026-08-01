# squeeze2

A drag-and-drop tool that compresses NVIDIA ShadowPlay gameplay captures small
enough to share on Discord — H.264 MP4, faststart, sized to fit under a byte
limit, with **zero configuration**.

This is the **Rust rewrite** of the original Go app. Decode/encode/mux run
**in-process** via FFmpeg's `libav*` libraries (the `rsmpeg` crate) using
**NVENC** for hardware encoding — there is **no shelled-out `ffmpeg.exe`**.

See [docs/rewrite-plan.md](docs/rewrite-plan.md) for the full architecture
rationale and roadmap.

---

## Status: Phase 1 — drag-and-drop GUI 🚧

Two binaries ship side by side:

- **`squeeze-gui.exe`** — the app: drop clips on the window, pick a size budget
  (10 / 50 / 500 MB), watch per-file progress. Encoding runs on a background
  worker. Files dropped onto the `.exe` icon are queued too. Native egui — no
  Electron, no webview.
- **`squeeze.exe`** — the headless CLI, for scripting and debugging.

Phase 0's engine is **proven on real hardware**: a single self-contained
executable (no DLLs, no user-installed FFmpeg) reliably produces a `< N` MB
H.264 MP4 via NVENC.

| validated | result |
|---|---|
| Windows static `.exe` + NVENC (Tesla T4, driver 596.36) | 42.8 MB → 9.32 MB, **one pass**, 9.6 s, zero DLLs |
| Linux + NVENC (Tesla T4) | 166 MB → 9.33 MB, one pass, 22.7 s (~⅓ the CPU of x264) |
| macOS + x264 | 166 MB → 9.25 MB, one pass |
| Output correctness | H.264 High, CFR, AAC stream-copied, **faststart** confirmed |

Software fallback: builds include **openh264** (BSD), and `--encoder auto` probes
each candidate by *actually opening* it — so a machine with no NVIDIA GPU, or a
driver older than FFmpeg 8's NVENC floor (**≈570**), degrades to software instead
of failing.

What it does per input file:

1. Probe duration / resolution / fps / audio.
2. Compute a target video bitrate from the size budget and duration.
3. Transcode: decode → **scale + VFR→CFR + yuv420p** filter → **h264_nvenc**
   (VBR, full-resolution multipass, High profile) → MP4 with `+faststart`.
   Audio is **stream-copied** (no re-encode).
4. **Measure the result; if it overshot the ceiling, re-encode at a lower
   bitrate** (up to `--passes` times). This loop is the whole point — neither
   NVENC nor any single-pass encoder guarantees a hard size cap.

Output is written next to the input as `<name>_discord.mp4`.

---

## Building on Windows 11 (primary target)

You need the FFmpeg **development** libraries built **with NVENC**
(`--enable-nvenc` / nv-codec-headers). Two paths; pick one.

### Option A — prebuilt FFmpeg (fastest to get linking)

1. Download a **shared dev** build from
   [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds/releases) —
   pick a `ffmpeg-master-latest-win64-lgpl-shared.zip` (LGPL keeps licensing
   clean; NVENC is included). Unzip to e.g. `C:\ffmpeg`.
   It contains `bin\` (DLLs), `lib\` (import `.lib`s) and `include\`.
2. Point rsmpeg at it (PowerShell):
   ```powershell
   $env:FFMPEG_INCLUDE_DIR = "C:\ffmpeg\include"
   $env:FFMPEG_DLL_PATH    = "C:\ffmpeg\bin"
   $env:FFMPEG_LIBS_DIR    = "C:\ffmpeg\lib"
   ```
3. Build and run:
   ```powershell
   cargo run -p cli --release -- "C:\path\to\clip.mp4"
   ```
4. **Runtime:** the `.dll`s in `C:\ffmpeg\bin` must sit next to the built
   `squeeze.exe` (or be on `PATH`). `nvEncodeAPI64.dll` / `nvcuda.dll` come from
   your NVIDIA driver — do not bundle those.

### Option B — vcpkg **static** (the shipping path: one self-contained `.exe`)

This is how we ship: a single `squeeze.exe` with **no FFmpeg DLLs** and **no
user-installed FFmpeg**. NVENC still works because FFmpeg loads it at *runtime*
from the NVIDIA driver (`nvEncodeAPI64.dll`) — it is never linked into the
binary — so a fully static FFmpeg build keeps hardware encoding.

```powershell
# static-md = FFmpeg + C deps static, MSVC CRT dynamic (matches Rust's default /MD).
vcpkg install "ffmpeg[avcodec,avformat,avfilter,swresample,swscale,nvcodec,zlib,dav1d]:x64-windows-static-md"
$env:VCPKG_ROOT = "C:\vcpkg"
cargo build -p cli --release --features vcpkg
```

- **Do NOT** add the `gpl` feature (pulls x264/x265 → forces GPL on the whole
  binary) or `nonfree` (makes the binary legally non-distributable). Native
  FFmpeg AAC + the built-in H.264/HEVC/AV1 *decoders* + `h264_nvenc` are all in
  the default LGPL set. `dav1d` is included for fast AV1 decode (RTX 40-series
  ShadowPlay records AV1).
- The vcpkg link integration auto-emits the FFmpeg + transitive deps link flags;
  if the MSVC linker still complains about system libs, add them in
  `crates/engine/build.rs` (e.g. `bcrypt`, `secur32`, `ws2_32`, `user32`).
- The only runtime dependency is the VC++ runtime already present on every
  Windows 11 box. `nvEncodeAPI64.dll` / `nvcuda.dll` come from the driver.
- **Driver:** FFmpeg 8 needs NVIDIA driver **≈570+** for NVENC; older/absent
  drivers fail cleanly (`ENOSYS` / "cannot load nvEncodeAPI64.dll") and the app
  falls back to `--encoder x264`.

> Don't chase `x64-windows-static` (static CRT) or `+crt-static` for v1 — mixing
> `/MD` and `/MT` causes `LNK2038` and the static-CRT FFmpeg triplet is far more
> failure-prone. `static-md` already gives you the single-file win.

> Toolchain: Rust **1.81+**, MSVC. `libclang` (LLVM) is only needed if rsmpeg
> regenerates bindings; the pinned `ffmpeg8` bindings usually avoid that.

> **LGPL note:** statically linking LGPL-2.1 FFmpeg into one `.exe` is fine for a
> closed binary only if you let users relink (ship object files or the FFmpeg
> source+config). Keeping squeeze2's source public satisfies this for free —
> just bundle FFmpeg's `LICENSE`/copyright notice in the release.

## Building on macOS (development / typecheck only — no NVENC)

```bash
brew install pkg-config ffmpeg
cargo run -p cli --features system -- ~/clip.mp4 --encoder x264
```

macOS has no NVENC; use `--encoder x264` (or `openh264`). This path exists so the
engine can be compiled and exercised off-Windows; VideoToolbox is a later phase.

---

## Usage

```
squeeze [OPTIONS] <INPUT>...

  --max-mb <MB>      Size ceiling (default 10.0 = Discord free tier)
  --encoder <E>      auto | nvenc | x264 | openh264   (default auto -> NVENC)
  --passes <N>       Max measure/re-encode passes (default 3)
  --suffix <S>       Output suffix (default _discord)
  -o, --outdir <DIR> Output directory (default: alongside input)
  --keep-fps         Don't cap 60fps -> 30fps when bits are tight
  --no-audio         Drop audio instead of stream-copying
```

Example:
```powershell
squeeze --max-mb 10 "C:\Videos\Shadowplay\clip1.mp4" "C:\Videos\clip2.mkv"
```

---

## What to measure in this spike (the go/no-go criteria)

- **Size accuracy:** does the output land under `--max-mb`, and in how many
  passes? (1 pass ideal; the loop is the safety net.)
- **Quality** vs. the original Go app's `libx264 -crf` output at a similar size.
- **NVENC works** end-to-end on your hardware/driver, and `--encoder x264`
  falls back cleanly when it doesn't.
- **Build effort** — Option A vs B — since "drop the `ffmpeg.exe` binary but
  link `libav*`" trades a runtime dependency for a build-time one. That trade is
  exactly what Phase 0 is here to evaluate.
