# Verifying squeeze on real Windows hardware

Everything measured so far came from a MacBook running **libx264**, or from CI
running **openh264** with no GPU. Neither is what most users get: the shipped
`.exe` carries NVENC and openh264 only, and almost everyone lands on NVENC.

So the numbers in the commit history describe settings *relative to each other*
and say nothing about what a real user sees. This is the list of things only a
Windows machine with an NVIDIA GPU can answer.

Work through it in order: section 1 is the one that matters most, section 6 is
nice to have.

## Before you start

```powershell
git pull
cargo build --release -p cli -p gui --features cli/vcpkg,gui/vcpkg
nvidia-smi          # note the driver version: NVENC on FFmpeg 8 wants 570+
```

Have a real ShadowPlay capture to hand, ideally more than one:

- a normal clip (whatever you usually record)
- a long one, over three minutes, which is where the budget gets tight
- if you can, one recorded with **separate game and mic audio tracks**
- if you can, one at **120 or 144 fps**

`ffmpeg` on PATH is optional but makes several checks much better
(`winget install Gyan.FFmpeg`).

## 1. NVENC: the encoder that actually ships

Never exercised in any test so far. The risk is not that it fails loudly, but
that its rate control lands differently from x264 and the size loop behaves
differently.

```powershell
.\target\release\squeeze-cli.exe --max-mb 10 .\clip.mp4
```

Check:

- [ ] The pass line says `[h264_nvenc]`, not openh264. If it says openh264, NVENC
      is not being selected and that is the finding.
- [ ] It reports `✓ fits`.
- [ ] **Pass count.** On the Mac essentially everything finished in one pass. If
      NVENC routinely needs two or three, the `margin: 0.92` in
      `crates/engine/src/lib.rs` is mistuned for it and should come down.
- [ ] Encode time. A 296-second clip took ~8 minutes on the Mac's CPU; NVENC
      should be far quicker. Anything slower than realtime is suspicious.

Then the same clip at each tier, and a long one:

```powershell
foreach ($mb in 10, 50, 500) {
  .\target\release\squeeze-cli.exe --max-mb $mb --suffix "_$mb" .\clip.mp4
}
.\target\release\squeeze-cli.exe --max-mb 10 .\long-clip.mp4
```

- [ ] Every output is at or under its ceiling. This is the product's one promise.
- [ ] The long clip fits. It re-encodes the audio to make room; confirm the
      output still *has* audio and it sounds right.

### NVENC against the software fallback

```powershell
.\target\release\squeeze-cli.exe --max-mb 10 --suffix _nv .\clip.mp4
.\target\release\squeeze-cli.exe --max-mb 10 --encoder openh264 --suffix _sw .\clip.mp4
```

- [ ] Both fit.
- [ ] Watch them side by side. openh264 runs Constrained Baseline with no
      B-frames and drops frames to hold the bitrate, so it should look clearly
      worse. If it looks *unusable* rather than merely worse, that is worth
      knowing, since it is what every GPU-less user gets.

With ffmpeg available, put numbers on it (`W`/`H`/`FPS` from the source):

```powershell
ffmpeg -v info -i .\clip_nv.mp4 -i .\clip.mp4 -lavfi `
  "[0:v]scale=W:H,fps=FPS[d];[1:v]scale=W:H,fps=FPS[r];[d][r]libvmaf" -f null -
```

## 2. The Auto-selection crash fix

`Auto` used to crash with an access violation on machines with no NVIDIA driver:
it probed `h264_nvenc` by opening it, and FFmpeg's cleanup after that failed
init touched uninitialised state. CI covers the no-GPU path. What CI cannot
check is that the fix did not break selection on a machine that *does* have a
GPU.

- [ ] Plain `squeeze-cli.exe --max-mb 10 clip.mp4` still picks `h264_nvenc`.
- [ ] `--encoder nvenc` works explicitly.
- [ ] `--encoder openh264` works explicitly.

Do **not** rename `nvcuda.dll` to fake a GPU-less machine; it is a system file.

## 3. The GUI

Drag-and-drop has never been tested on Windows at all. It is the primary way
people use this.

```powershell
.\target\release\squeeze.exe
```

- [ ] Drag one clip onto the window. It queues and encodes.
- [ ] Drag several at once. They queue and run one after another.
- [ ] Drag a clip onto `squeeze.exe` in Explorer (the argument path, separate
      code from the drop handler).
- [ ] The tier buttons (Free / Nitro Basic / Nitro) change the target.
- [ ] `Keep fps`, `Keep resolution`, `No audio` each visibly change the result.
- [ ] Turn on `Keep resolution` with a long clip: the amber note about the frame
      being held should appear, naming a smaller size.
- [ ] Feed it a clip already under the limit: the card should say
      *"copied, already under the limit"* and finish nearly instantly.
- [ ] The timer counts up while encoding and freezes at the end.
- [ ] The window looks right: fonts crisp, logo not pixelated, no dark band at
      any edge, title bar reads **Squeeze**.
- [ ] Try it on a 125% or 150% display scale, and resize the window.

## 4. Real ShadowPlay captures

The synthetic clips used so far are far easier to encode than gameplay, and
ShadowPlay writes files with quirks no generated file has.

- [ ] A capture that is genuinely variable frame rate comes out constant, with
      audio still in sync at the end of a long clip (check the last few seconds).
- [ ] If ShadowPlay is set to **separate audio tracks**: the output has one
      track and *both* game sound and microphone are audible in it. This was
      silently dropping the second track until recently.
- [ ] If you can record **HEVC or AV1**: it decodes and comes out as H.264 MP4.
- [ ] A **120 or 144 fps** capture: with a tight budget it should step to 60 or
      72, not straight to 30. Confirm the output rate with
      `ffprobe -v error -select_streams v:0 -show_entries stream=r_frame_rate -of csv=p=0 out.mp4`.

## 5. Does Discord actually accept it

The whole point, and completely untested.

- [ ] Upload a 10 MB output on a free account. It should not be rejected.
- [ ] It plays inline in the Discord client.
- [ ] It plays in a browser tab as well.
- [ ] An **ultrawide** result plays correctly. A 21:9 source comes out at sizes
      like 2226x932, which is unusual enough to be worth checking.
- [ ] A result at an odd frame rate (72, or 29.97) plays smoothly.

## 6. Windows odds and ends

- [ ] First run of an unsigned build: SmartScreen shows *"Windows protected your
      PC"*. Confirm **More info -> Run anyway** works, since that is what the
      README tells people to do.
- [ ] A path with spaces, and one with non-ASCII characters (`å ä ö`). Paths go
      through `to_string_lossy` into FFmpeg, so this is a real risk.
- [ ] Exit codes, which now distinguish success from a clip that would not fit:

```powershell
.\target\release\squeeze-cli.exe --max-mb 10 .\clip.mp4;  $LASTEXITCODE   # 0
.\target\release\squeeze-cli.exe --max-mb 0.2 .\long.mp4; $LASTEXITCODE   # 1
```

- [ ] Defender does not quarantine the exe.

## What to bring back

For anything that fails, the full console output is worth more than a summary:
it carries the FFmpeg and encoder lines. Also useful regardless:

- driver version and GPU model
- pass counts and encode times from section 1
- whether NVENC ever missed a ceiling
