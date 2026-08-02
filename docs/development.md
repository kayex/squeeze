# Development

Rust workspace, three crates:

| crate | produces | what it is |
|---|---|---|
| `crates/engine` | lib | the encode engine: probe, plan, transcode, size loop |
| `crates/gui` | `squeeze.exe` | the app (egui/eframe) |
| `crates/cli` | `squeeze-cli.exe` | headless CLI |

Decode/encode/mux run **in-process** via FFmpeg's `libav*` libraries (the
[`rsmpeg`](https://crates.io/crates/rsmpeg) crate). There is no shelled-out
`ffmpeg.exe`. Hardware encoding uses `h264_nvenc`.

Dev tasks go through [`just`](https://github.com/casey/just): run `just` to list
them. Related docs: [rewrite-plan.md](rewrite-plan.md) for the architecture
rationale, [deploy-and-test.md](deploy-and-test.md) for building and GPU-testing
without a Windows machine.

## How a file gets compressed

1. **Probe** duration, resolution, fps, audio.
2. **Plan** a target video bitrate from the size budget and duration, choosing a
   downscale and fps cap if the bitrate would be too low to look reasonable.
3. **Transcode**: decode → filter (`scale` + `fps` for VFR→CFR + `format=yuv420p`)
   → `h264_nvenc` (VBR, full-res multipass, High profile) → MP4 with
   `+faststart`. Audio is stream-copied.
4. **Measure and retry**: if the output overshot the ceiling, re-encode at a
   proportionally lower bitrate (up to `--passes`).

Step 4 is the load-bearing one: neither NVENC nor any single-pass encoder
guarantees a hard size cap, so the only way to promise "under N MB" is to check.

`--encoder auto` picks an encoder by **actually opening** each candidate, not by
name: `h264_nvenc` is compiled into the binary whether or not the machine has an
NVIDIA GPU, and only fails at open time. Probing is what makes the fallback to
software encoding work.

## Building on macOS (dev only, no NVENC)

macOS has no NVENC, so use `--encoder x264`. This path exists so the engine can
be compiled and exercised off-Windows; VideoToolbox is a later phase.

```bash
brew install just pkg-config ffmpeg
just build
just run ~/clip.mp4          # compresses via x264
just gui                     # launch the app
just sample /tmp/test.mp4    # generate a ShadowPlay-like test clip
```

The `system` cargo feature (`rsmpeg/link_system_ffmpeg`) links the Homebrew
FFmpeg via pkg-config. Homebrew ships FFmpeg 8.x, which matches the pinned
`ffmpeg8` bindings.

## Verifying a build on real hardware

CI builds and smoke-tests the binaries, but has no GPU and the Mac has no
NVENC. [windows-verification.md](windows-verification.md) is the checklist
for what only a Windows machine with an NVIDIA card can answer.

## Building on Windows

CI does this for you (see below), so build locally only if you're changing the
link setup. You need FFmpeg **development** libraries built **with NVENC**.

### Option A: vcpkg static (how we ship)

Produces one self-contained `.exe` with no FFmpeg DLLs. NVENC still works
because FFmpeg loads it at *runtime* from the NVIDIA driver
(`nvEncodeAPI64.dll`) and it is never linked in, so a fully static build keeps
hardware encoding.

```powershell
# static-md = FFmpeg + C deps static, MSVC CRT dynamic (matches Rust's default /MD).
vcpkg install "ffmpeg[avcodec,avformat,avfilter,swresample,swscale,nvcodec,openh264,zlib,dav1d]:x64-windows-static-md"
$env:VCPKG_ROOT = "C:\vcpkg"
cargo build --release -p cli -p gui --features cli/vcpkg,gui/vcpkg
```

- **Do NOT** add the `gpl` feature (pulls x264/x265 → forces GPL on the whole
  binary) or `nonfree` (makes the binary non-distributable). Native FFmpeg AAC,
  the built-in H.264/HEVC/AV1 decoders, `h264_nvenc` and `openh264` are all in
  the LGPL-clean set.
- `dav1d` gives fast AV1 decode (RTX 40-series ShadowPlay records AV1);
  `openh264` (BSD) is the software-encode fallback.
- If the MSVC linker complains about system libs, add them in
  `crates/engine/build.rs`, which is what it's for.
- Don't chase `x64-windows-static` (static CRT) or `+crt-static`: mixing `/MD`
  and `/MT` causes `LNK2038`, and the static-CRT FFmpeg triplet is far more
  failure-prone. `static-md` already gives the single-file win.

### Option B: prebuilt FFmpeg (quicker to get linking)

Download a **shared dev** build from
[BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds/releases) (pick an
`n8.x` `win64-lgpl-shared`; NVENC included) and unzip to `C:\ffmpeg`:

```powershell
$env:FFMPEG_INCLUDE_DIR = "C:\ffmpeg\include"
$env:FFMPEG_DLL_PATH    = "C:\ffmpeg\bin"
$env:FFMPEG_LIBS_DIR    = "C:\ffmpeg\lib"
cargo build --release -p cli -p gui
```

The DLLs from `C:\ffmpeg\bin` must then sit next to the built `.exe` or be on
`PATH`. `nvEncodeAPI64.dll` / `nvcuda.dll` come from the NVIDIA driver, so never
bundle those.

> Toolchain: Rust **1.81+**, MSVC. `libclang` (LLVM) is only needed if rsmpeg
> regenerates bindings; the pinned `ffmpeg8` bindings usually avoid that.

## CI and releases

| workflow | trigger | does |
|---|---|---|
| `windows-build.yml` | called by the other two | builds both `.exe`s, uploads the artifact |
| `build.yml` | push to `main` | CI |
| `release.yml` | `v*` tag | builds, packages, drafts a GitHub release |

The Windows build is a reusable workflow so the recipe lives in one place. The
two entry points stay separate because `build.yml`'s `paths-ignore` filter could
otherwise suppress a tagged release build.

vcpkg compiles FFmpeg from source on a cold cache (~30 min); the cache is saved
*before* the cargo build so a link failure can't discard it. Warm builds are
~2 minutes.

```bash
just ci          # trigger a build
just ci-watch    # follow it
just ci-fetch    # download the binaries into ./dist
```

### Cutting a release

```bash
just release 0.1.0
```

Checks the tree is clean and that the crate versions match the tag, then pushes
`v0.1.0`. CI builds and creates a **draft** release with the zip, the bare
`squeeze.exe`, `SHA256SUMS.txt` and a build-provenance attestation. Review it,
then publish. Set `draft: false` in `release.yml` to publish straight from tags.

## The icon

`assets/logo.png` is the source: a square, transparent-background export of
`assets/icon-new.png` (the original artwork, which had the tile drawn on opaque
black). To regenerate after changing the artwork:

```bash
just icon assets/logo.png     # any PNG or SVG works
just icon-preview             # magnifies 16/32/48px to check legibility
```

**Artwork needs a transparent background.** Windows composites icons over
whatever is behind them, so an opaque backdrop shows up as a square block in
Explorer. If an export has one, cut it before generating:

```bash
magick artwork.png -alpha set -fuzz 4% -fill none \
  -draw "alpha 0,0 floodfill" -draw "alpha 1253,0 floodfill" \
  -draw "alpha 0,1253 floodfill" -draw "alpha 1253,1253 floodfill" \
  -trim +repage -background none -gravity center -extent WxW PNG32:assets/logo.png
```

Keep the fuzz low, because a dark backdrop and a dark design are close enough in colour
that a generous tolerance floods into the artwork itself.

This writes `assets/icon.ico` (embedded into both `.exe`s by each crate's
`build.rs` via `winresource`) and `assets/icon-256.png` (the GUI window icon).
Design for **16px**, which is Explorer's list view.

Note it uses `icotool`, not ImageMagick's `-define icon:auto-resize`, which
stores every entry as uncompressed BMP; Windows expects the 256px entry
PNG-compressed.

Windows `VERSIONINFO` (Properties → Details) comes from the
`[package.metadata.winresource]` blocks in each crate's `Cargo.toml`. Without
them the fields default to the *crate* name, so the app would show as "gui".
None of it affects the SmartScreen publisher; that comes from a code signature.

## Code signing

Not signed today. The realistic options for a free, individual-authored,
EU-based project, in order:

1. **[SignPath Foundation](https://signpath.org/apply)**: free OV-grade signing
   for open-source projects, key held on their HSM, CI integration. They want to
   see a project that's already released and actively maintained, so this comes
   after a couple of releases.
2. **[Certum Open Source Code Signing](https://shop.certum.eu/code-signing.html)**
   in the cloud, €49, EU-to-EU, no hardware token needed.

Do **not** buy an EV certificate: Microsoft removed EV's instant-SmartScreen
advantage in 2024, so the premium buys nothing here. Azure Artifact Signing is
unavailable, because individual enrolment is US/Canada only.

Signing does **not** remove the first-download prompt (that's reputation-based),
and it does **not** prevent Defender false positives. For those, submit to
[WDSI](https://www.microsoft.com/en-us/wdsi/filesubmission) as "Software
developer – false positive". What it does fix: the "Unknown publisher" label,
reputation resetting on every release, and Smart App Control hard-blocking
unsigned binaries on Windows 11.
