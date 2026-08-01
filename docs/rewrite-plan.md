# squeeze2: Architecture Recommendation

Rust rewrite of `squeeze`, a drag-and-drop tool that compresses NVIDIA ShadowPlay
gameplay captures for sharing on Discord. Priorities: smallest output, near-zero
config, native UI (no Electron/webview), Windows 11 first (macOS nice-to-have),
GPU (NVENC) encode when available.

Findings reflect 2025-2026 research, adversarially fact-checked against live sources.
Where a verifier refuted or flagged a claim, the corrected position is used.

---

## 1. Executive recommendation

- **UI:** `egui`/`eframe` (pin `eframe` 0.34.3). Native wgpu/glow renderer, no webview, single self-contained .exe, first-class multi-file OS drag-and-drop returning real `PathBuf`s. Use the `glow` backend to shave binary size.
- **Encode path:** GPU-first via **NVENC**, driven in **target-bitrate/2-pass mode, not CRF**, with a **size-verify-and-re-encode loop** as the hard ceiling guarantee. This is the load-bearing risk, so spike it first (see §8).
- **Decode + demux + mux:** Use a real media library, not a hand-rolled pure-Rust pipeline. Two candidate engines, decided by the §8 spike: **(A) link `libav*` via `rsmpeg` (0.18.0+ffmpeg.8.0), LGPL-only build, NVENC/NVDEC by codec name**, or **(B) Windows Media Foundation via the official `windows` crate** for an even smaller bundle. A is the safer default for hitting a tight size target; B is the leaner "purest no-ffmpeg" option but carries an unresolved rate-control defect (§2).
- **Output target:** **MP4, H.264 High profile, AAC-LC 96 kbps stereo, `+faststart` (moov at front)**. H.264 is the only video codec that reliably inline-previews on Discord. HEVC/AV1 are optional power-user outputs only, since they download rather than embed.
- **Hardware-accel posture:** NVENC when present (your users have ShadowPlay-capable GeForce cards by definition); graceful fallback to a software encoder (x264 via the libav build, or MF's software MFT) so non-NVENC machines still work.
- **ffmpeg posture:** **Drop the external `ffmpeg.exe` process: yes. Drop ffmpeg-the-codebase entirely: no, not for v1.** Link `libav*` in-process (option A) or use OS frameworks (option B). A fully pure-Rust encode pipeline is not viable today (§2).

---

## 2. Can we actually drop ffmpeg?

**Blunt verdict: you can eliminate the shelled-out `ffmpeg.exe` binary, and that is worth doing. You cannot build a fully pure-Rust decode→encode→mux pipeline today that produces small files. The blocker is the encoder.**

What "no external ffmpeg.exe" realistically means, from cleanest-to-build to most-self-contained:

1. **Link `libav*` as a library (rsmpeg / ffmpeg-next).** No child process, no `ffmpeg.exe` on disk. You still ship FFmpeg *code* (DLLs or static libs) and inherit FFmpeg's Windows/MSVC build pain (vcpkg, `ffmpeg:x64-windows-static-md`) and its licensing posture. This gives you x264-quality software encode **and** `h264_nvenc`/`hevc_nvenc`/`h264_cuvid` hardware paths, full demux/decode/mux, and audio. rsmpeg 0.18.0+ffmpeg.8.0 is current and does exactly this. **This drops the binary, not the codebase.**

2. **OS frameworks + NVENC SDK, genuinely ffmpeg-free.** Decode via Media Foundation (or NVDEC), encode via NVENC or the MF encoder MFT, mux via a Rust MP4 muxer. Ships zero FFmpeg code. Costs: more glue, weaker non-NVIDIA fallback, and, critically, **a confirmed open defect: `MediaTranscoder` ignores the requested bitrate when hardware acceleration is enabled** (Microsoft WindowsAppSDK issue #4804; a 10 Mbps request produced ~30 Mbps). It reproduced on AMD, did *not* reproduce on an NVIDIA 3090, so it may not bite your NVENC users, but it strikes directly at priority #1 (smallest file) and is unresolved with no documented workaround. If you go this route you will likely need to drop to the lower-level `IMFSinkWriter` + encoder-MFT `ICodecAPI` rate-control properties, not the one-call high-level API.

3. **The pure-Rust fantasy: not viable.**
   - **No production pure-Rust H.264 encoder.** The only one, `less-avc`, is all-intra/lossless with no rate control, which is fatal for small files.
   - **No pure-Rust HEVC encoder at all** (only decoders like `rust_h265`).
   - **`openh264`** (the realistic Rust H.264 option) is a binding to Cisco's C library, BSD-2, **Constrained Baseline profile only** (no B-frames, no CABAC), giving ~15-25% larger files than libx264 High. That directly undercuts priority #1. (Pin ≥0.9.0 / openh264-sys2 ≥0.9.3 to clear RUSTSEC-2025-0008.)
   - **`rav1e`** is genuinely pure-Rust and production-grade, but **AV1-only and software/CPU-bound (slow)**, and AV1 doesn't embed on Discord. Wrong codec, wrong speed.
   - Muxing *can* be pure Rust (`mp4` crate; `muxide` exists but its "production-ready" claim is uncorroborated, v0.2.x, single author). Mux-out is the *only* layer pure Rust plausibly covers; **decode-in and encode have no mature pure-Rust answer.**

**Net:** target option 1 (rsmpeg, LGPL-only) as the pragmatic default, or option 2 (MF) if you value the smallest possible bundle and the §8 spike proves you can pin the bitrate. Pure Rust is a research project, not a v1.

---

## 3. Trade-off matrix

| | **A: MF (Win) + VideoToolbox (mac)** | **B: NVENC/NVDEC SDK direct** | **C: link libav* (rsmpeg), LGPL, hw-accel** ★ | **D: mostly-pure-Rust (dav1d/openh264/rav1e)** |
|---|---|---|---|---|
| **Drops external ffmpeg.exe?** | Yes (fully, no FFmpeg code) | Yes (fully) | Yes (binary gone; code linked in) | Yes (fully) |
| **Hardware encode?** | Yes (NVENC/QSV/AMF, transparent) | Yes (NVENC only) | Yes (`h264_nvenc`, QSV, AMF) | No (rav1e is CPU; openh264 is CPU) |
| **Decodes all ShadowPlay inputs?** | H.264 yes; HEVC/AV1 need OS extensions or NVDEC | Yes via NVDEC (no OS extensions) | Yes (libav handles H.264/HEVC/AV1, MP4+MKV) | Partial/immature (HEVC decode unproven) |
| **Windows effort** | Med-High (COM/WinRT, async; bitrate bug #4804) | High (CUDA ctx, surfaces, you supply demux/mux/audio) | Med (code easy; **build/link pain** is the cost) | High (glue everywhere, weak quality) |
| **macOS effort** | Low (VideoToolbox/AVFoundation mirror) | N/A (NVIDIA-only) | Med (same build pain) | Med |
| **Bundle size** | Smallest (OS provides codecs) | Small (thin SDK glue; codec in driver) | Largest (FFmpeg DLLs, tens of MB) | Small-Med |
| **Licensing risk** | Lowest (OS-licensed H.264) | Low-Med (NVIDIA pushes patent duty to you; AV1 RF) | Med (**must build LGPL-only**, avoid x264/fdk-aac to stay non-GPL) | Lowest (BSD/RF) but wrong-codec |
| **Dev complexity** | Med-High | Highest | **Med, best effort/quality balance** ★ | High |

★ **Recommended: C (rsmpeg, LGPL-only build, NVENC for encode / NVDEC for decode).** It best satisfies priority #1 (proven x264-and-NVENC quality with real rate control), decodes every ShadowPlay variant including MKV/AV1/HEVC without OS-extension prompts, and keeps NVENC for speed. The price is FFmpeg's Windows build/link burden and a tens-of-MB bundle. **If bundle size or "zero FFmpeg code" is a hard constraint, fall back to A**, but only after the §8 spike proves you can pin output size on NVENC despite bug #4804.

---

## 4. Recommended output defaults for Discord

**Current Discord limits (mid-2026):** free **10 MB**, Nitro Basic **50 MB**, full Nitro **500 MB**. Free dropped from 25 MB in Sept 2024, so ignore older guides quoting 8/25 MB. Treat the limit as a **config constant**, not hard-coded; Discord has changed it before and runs per-user experiments.

**Default container/codec:**
- **Container:** MP4, `+faststart` (moov atom relocated to front, required for inline streaming without full download; relocation needs no re-encode).
- **Video:** H.264 **High** profile, 8-bit 4:2:0, B-frames on. (Only H.264, and arguably VP9, reliably inline-previews; HEVC/AV1 download instead. H.264 is the safe default.)
- **Audio:** AAC-LC, 48 kHz stereo, **96 kbps** (use Opus only if you go fully OS/NVENC-free and accept that Opus-in-MP4 preview on Discord is unverified).

**Zero-config size logic (the core of "make it fit & small"):**
1. Default budget **9.0-9.5 MB** (headroom under the 10 MB free cap; do not assume Nitro). Offer 50 MB / 500 MB presets.
2. Compute target video bitrate from budget and duration:
   `video_kbps = (target_MB × 8192 / duration_s) − audio_kbps`
   (e.g. 9 MB, 60 s, 96 k audio → ~1132 kbps video.)
3. **Use 2-pass ABR, not CRF.** CRF cannot guarantee a file size; the whole point is a hard ceiling. Then **verify actual output size and re-encode at a lower bitrate on overshoot**. This loop is mandatory because neither NVENC nor MF guarantees "never exceed N bytes" (and MF may ignore the target entirely per bug #4804).

**Auto-downscale / framerate rules** (ShadowPlay is often 1440p/4K 60fps; at ~1.1 Mbps that's hopeless without scaling):
- Downscale 1440p/4K → **1080p**, and → **720p** when computed bitrate is very low.
- **Cap fps 60 → 30** when the budget is tight (~40% bitrate saving).
- **Normalize VFR → CFR (mandatory).** ShadowPlay outputs variable frame rate; leaving it VFR reproduces the A/V-desync bug that breaks editors. Force CFR / normalize timestamps during re-encode.
- Defensively handle MP4s with **zero or multiple audio tracks**, and demux **MKV as well as MP4** (AV1/HDR captures may arrive as MKV).

**Caveat:** the "uploaded H.264 MP4 still auto-previews inline post-May-2025" assumption rests on secondary/anecdotal sources, not a definitive Discord statement. **Validate with one real upload test before committing.**

---

## 5. UI design

**Framework: `egui`/`eframe`** (pin `eframe` 0.34.3). Rationale: native non-webview renderer (rules out Tauri/Dioxus, which embed WebView2 and violate the no-web-engine constraint); first-class OS file-drop returning real `PathBuf`s; single self-contained .exe (~6-8 MB, no runtime DLLs); cross-platform so the macOS nice-to-have stays open; immediate-mode makes the job-list-with-progress-bars UI nearly free. Backup: `iced` 0.13.1 (also native, Elm-style, but one drop event per file and more boilerplate). Rejected: Slint (external file drop from Explorer not yet supported, disqualifying for a drop-centric app), native-windows-gui (Windows-only, frozen since 2022), fltk-rs (CMake/C++ toolchain, manual `file://` parsing).

**Drag-and-drop:** On Windows you **must** set `ViewportBuilder::with_drag_and_drop(true)` (default; if disabled, `dropped_files` is silently always empty). Read via `ctx.input(|i| i.raw.dropped_files)` → `Vec<DroppedFile>`; on native each has a populated `path: Option<PathBuf>` (set by the egui-winit backend). Use `i.raw.hovered_files` for the hover affordance. **`path` is `Option`**, so defensively handle the rare bytes-only/`None` case, and spike a 20+-file simultaneous drop on Windows 11 to confirm all paths populate.

**File/progress list:** one row per dropped file: filename, source resolution/duration, a per-file progress bar, and final output size + "fits under 10 MB ✓". Immediate-mode redraw per encode tick. Run encodes on a worker thread (or thread pool for batch); push progress via a channel the UI polls each frame.

**Output-suffix flow:** zero-config: write output next to each input with a suffix (the original used `_q5`; here use something budget-derived like `_discord` or `_9mb`). No save dialog. Never overwrite the source.

**Packaging/signing:** ship a **bare self-contained .exe**, which matches the near-zero-config / drag-one-.exe priority. Optional `cargo-wix` MSI or NSIS only for Start-menu/uninstall entries. **Signing:** unsigned is a defensible launch posture for a free tool (one-time SmartScreen "Unknown Publisher"). If warnings hurt adoption, buy an **OV or Individual-Validated Authenticode cert (~$200-300/yr), not EV**. Microsoft's March 2024 Trusted Root change erased EV's instant-SmartScreen-reputation advantage.

---

## 6. Licensing & distribution

**Lowest-risk choice: source the H.264 encoder from the OS/driver, never bundle x264/x265.**

- **Do not bundle x264/x265**: GPLv2 copyleft forces your product's source open, and the GPL is *separate from* (does not satisfy) the H.264 patent obligation. Fatal for a closed indie tool.
- **Do not ship a GPL `ffmpeg.exe`** (libx264 build) unless you accept the GPL source-availability obligation for that binary. The original shell-out design technically carried this.
- **If you link libav* (recommended option C): build LGPL-only, dynamically linked, encode via NVENC** (`h264_nvenc`) or a fallback, **not** `--enable-gpl`/libx264, **not** `--enable-nonfree`/fdk-aac (the latter is non-distributable). LGPL-only + OS/NVENC encoders keeps you clean.
- **OS-encoder path (option A)** is the lowest practical patent burden: Windows ships an MPEG-LA-licensed H.264 stack, and the AVC license grants end users personal/consumer/internal use without remuneration. You ship zero codec code.
- **Audio:** prefer **Opus** (RFC 6716, royalty-free, clean Rust crates) if you control the container; **never bundle fdk-aac** (no patent grant, Debian non-free). If you need AAC for guaranteed Discord preview, get it from the OS/libav LGPL path rather than fdk-aac.
- **AV1** is the cleanest license (royalty-free, BSD encoders) but the **wrong product choice**, because Discord can't thumbnail/embed it. **HEVC** licensing got *worse* in Dec 2025 (Access Advance absorbed Via-LA's HEVC/VVC program). Both stay optional-only; H.264 is the delivery codec.

**Open legal question (not settled):** whether Windows' H.264 license extends patent coverage to a third-party app that merely invokes the MF encoder is not definitively documented. Industry practice treats OS-codec use as fine, and there's no evidence of pools pursuing free hobby tools, but this is a risk-appetite call.

---

## 7. Key risks & unknowns

1. **Hard size ceiling on a hardware encoder (highest risk).** Neither NVENC nor MF guarantees "never exceed N bytes." MF's high-level `MediaTranscoder` has a *confirmed open bug* (#4804) ignoring requested bitrate under hw-accel. **Mitigation: 2-pass ABR + measure-and-re-encode loop; on MF, plan for `IMFSinkWriter` + `ICodecAPI` rate-control, not the one-call API.** Prototype before committing (§8).
2. **Quality-per-byte at ~1.1 Mbps.** The original's tiny files came from libx264 -crf at a slow preset. NVENC (Turing+) is competitive but generally less bitrate-efficient; openh264 Baseline is 15-25% worse. At a 10 MB ceiling every percent costs visible quality. **Mitigation: measure NVENC vs libx264 on real ShadowPlay gameplay clips; lean on downscale + fps cap.**
3. **FFmpeg Windows build/link/distribution story** (if option C). vcpkg static linking, DLL bundling, LGPL compliance, MSVC: non-trivial. Spike the actual build.
4. **VFR→CFR correctness** across the real spread of ShadowPlay timestamps (one sample showed min 2.4 / max 127.9 fps). Get this wrong and you ship the A/V-desync bug.
5. **Container variety:** MKV as well as MP4; zero/multiple/sidecar audio tracks; HDR 10-bit captures needing tone-mapping before SDR re-encode.
6. **Discord inline-preview assumption** rests on weak sourcing. **Do a real upload test.**
7. **AV1/HEVC ShadowPlay decode** without forcing users to install Windows AV1/HEVC extensions, which argues for NVDEC or libav decode over pure MF.
8. **egui multi-file drop on Windows 11**: `path` is `Option`; verify a large simultaneous drop.

---

## 8. Phased implementation roadmap

Ordered so the **riskiest assumption, the encode pipeline hitting a hard size target on hardware, is de-risked before any UI work.**

**Phase 0: Encode spike (do this first; 1-2 weeks). The go/no-go gate.**
A headless CLI, no GUI. Take one real ShadowPlay MP4 → produce a `+faststart` H.264 High MP4 **under 9.5 MB** via 2-pass ABR + size-verify-re-encode loop, with VFR→CFR normalization. **Build it twice and compare:**
- **Spike A:** rsmpeg (LGPL build) driving `h264_nvenc` + `h264_cuvid`.
- **Spike B:** MF via the `windows` crate (`IMFSinkWriter`), testing whether you can actually pin output size on NVENC hardware despite bug #4804.

**Measure:** final size accuracy, visual quality vs the original libx264 -crf output, encode speed, and Windows build/link effort. **Decide engine A vs C here.** Also: one real Discord upload to confirm inline preview.

**Phase 1: Walking skeleton (1 week).** egui/eframe window, drag-and-drop one file, hardcoded 9.5 MB budget, call the Phase-0 encoder on a worker thread, write `_discord`-suffixed output next to input, show a single progress bar. Proves the end-to-end UI→encode→file loop.

**Phase 2: Batch + robustness (1-2 weeks).** Many-file drop, per-file job list with progress + result size, thread pool. Handle MKV input, multiple/zero audio tracks, auto-downscale (1440p/4K→1080p/720p) and fps cap. Graceful software fallback when NVENC absent. Defensive `path: Option` handling.

**Phase 3: Polish & defaults (1 week).** 10/50/500 MB presets, desktop completion notification (parity with the original), HDR tone-map handling if prevalence warrants, edge-case error surfacing.

**Phase 4: Distribution (few days).** Strip/LTO the .exe, optional cargo-wix MSI, decide signing (ship unsigned or OV/Individual cert), README. macOS port (VideoToolbox via `objc2-video-toolbox`) only if still wanted.

**Bottom line:** Phase 0 is the whole ballgame. If the NVENC-via-rsmpeg spike hits 9.5 MB with acceptable quality and a tolerable Windows build, commit to option C and the rest is conventional Rust app work. If the build pain is unacceptable and MF can pin the size on NVENC, take option A for the smaller bundle. Either way you drop `ffmpeg.exe`; do not chase the pure-Rust pipeline for v1.
