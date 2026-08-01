<div align="center">

<img src="assets/icon-256.png" width="88" alt="">

# Squeeze

</div>

A Windows tool that compresses NVIDIA ShadowPlay clips to a size Discord will
accept.

Drag clips onto the window and pick a size limit. Each one is re-encoded to fit
under it and saved next to the original with a `_discord` suffix, so `clip.mp4`
becomes `clip_discord.mp4`. Originals are left alone.

<!-- TODO: screenshot of the app with a couple of files queued goes here -->

## Installation

Download `squeeze.exe` from the
[latest release](https://github.com/kayex/squeeze/releases/latest) and run it.

The first time you do, Windows will show *"Windows protected your PC"* and an
unknown publisher. Click **More info → Run anyway**. This is because the app
isn't code-signed. If you'd rather check the file first, see
[verifying a download](#verifying-a-download).

## Requirements

- Windows 10 or 11, 64-bit
- An NVIDIA GPU with driver 570 or newer, for hardware encoding. Without one,
  Squeeze encodes in software instead, which is slower but gives the same result.

## Usage

1. Open `squeeze.exe`.
2. Drag one or more clips onto the window, or onto the `.exe` itself in Explorer.
3. Pick a size limit: **10 MB** (Discord free), **50 MB** (Nitro Basic) or
   **500 MB** (Nitro).

Reads MP4 and MKV containing H.264, HEVC or AV1, which covers what ShadowPlay
records. Writes H.264 MP4.

## What happens to your clip

Nothing here is something you do. It is what Squeeze works out on its own once
you have picked a size limit. It is described only so the output holds no
surprises.

The limit and the length of the clip decide everything else: a fixed number of
bytes spread over more seconds means fewer bits per second, and below a certain
point fewer or smaller frames look better than blurry ones.

1. **Measurement**: the duration, resolution and frame rate are read from the
   file.

2. **Bitrate**: roughly `size limit ÷ duration`, minus what the audio needs,
   aiming a little under the limit for safety. A 30-second clip at 10 MB gets
   about 2.3 Mbit/s of video; two minutes at the same limit gets about
   500 kbit/s. Never more than the clip already had, so a small clip is never
   re-encoded larger than it started.

3. **Resolution**: 1440p and 4K are kept when there is plenty of bitrate to go
   round (above ~10 Mbit/s) and come down to 1080p when there is not. Below
   ~1.6 Mbit/s they drop again to 720p, and below ~700 kbit/s to 480p.
   The **Keep resolution** switch (or `--keep-resolution`) overrides this.
   Clips are never enlarged.

4. **Frame rate**: anything above 45 fps drops to 30 when the bitrate is
   under ~3 Mbit/s, which is the usual case for clips longer than about
   20 seconds at 10 MB. The **Keep fps** switch (or `--keep-fps`) overrides
   this. Variable frame rate, which ShadowPlay records, is converted to
   constant.

5. **Encoding**: H.264, with the original audio track copied across rather than
   re-compressed. This runs on the GPU where possible. The **No audio** switch
   (or `--no-audio`) drops the track and spends its share on video.

6. **Size check**: the finished file is measured, and if it came out over the
   limit it is encoded again at a lower bitrate, up to three attempts.

Squeeze shows the resolution and frame rate settled on for each clip, so a
clip that was scaled down is visible at a glance. A 30-second 1440p60 clip aimed
at 10 MB comes out 1080p30, for instance; the same clip aimed at 50 MB stays
1440p60.

## Command line

`squeeze-cli.exe`, in the release zip, does the same thing without the window:

```powershell
squeeze-cli.exe --max-mb 10 "C:\Videos\clip.mp4"
```

| option | |
|---|---|
| `--max-mb <MB>` | Size ceiling (default `10`) |
| `--encoder <E>` | `auto` \| `nvenc` \| `x264` \| `openh264` (default `auto`) |
| `--passes <N>` | Max re-encode attempts (default `3`) |
| `--suffix <S>` | Output suffix (default `_discord`) |
| `-o, --outdir <DIR>` | Output directory (default: next to the input) |
| `--keep-fps` | Don't drop frame rates above 45 to 30 when the budget is tight |
| `--keep-resolution` | Don't scale the frame down when the budget is tight |
| `--no-audio` | Drop audio instead of copying it |

## Verifying a download

Checksums are listed in each release's notes:

```powershell
certutil -hashfile squeeze.exe SHA256
```

Builds are produced by GitHub Actions, so you can also confirm a file came from
this repository's workflow:

```powershell
gh attestation verify squeeze.exe -R kayex/squeeze
```

## Building from source

See [docs/development.md](docs/development.md).

## Licence

MIT. See [LICENSE](LICENSE). Squeeze links the FFmpeg libraries under the
LGPL v2.1, built without GPL or non-free components.
