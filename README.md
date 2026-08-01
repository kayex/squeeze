<div align="center">

<img src="assets/icon-256.png" width="88" alt="">

# squeeze

</div>

A Windows tool that compresses NVIDIA ShadowPlay clips to a size Discord will
accept.

Drag clips onto the window and pick a size limit. Each one is re-encoded to fit
under it and saved next to the original with a `_discord` suffix — `clip.mp4`
becomes `clip_discord.mp4`. Originals are left alone.

<!-- TODO: screenshot of the app with a couple of files queued goes here -->

## Install

Download `squeeze.exe` from the
[latest release](https://github.com/kayex/squeeze2/releases/latest) and run it.

The first time you do, Windows will show *"Windows protected your PC"* and an
unknown publisher — click **More info → Run anyway**. This is because the app
isn't code-signed. If you'd rather check the file first, see
[verifying a download](#verifying-a-download).

## Requirements

- Windows 10 or 11, 64-bit
- An NVIDIA GPU with driver 570 or newer, for hardware encoding. Without one,
  squeeze encodes in software instead — slower, same output.

## Usage

1. Open `squeeze.exe`.
2. Drag one or more clips onto the window, or onto the `.exe` itself in Explorer.
3. Pick a size limit: **10 MB** (Discord free), **50 MB** (Nitro Basic) or
   **500 MB** (Nitro).

Reads MP4 and MKV containing H.264, HEVC or AV1, which covers what ShadowPlay
records. Writes H.264 MP4. Clips too large to fit at their original resolution
are scaled down, and very high frame rates are capped.

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
| `--keep-fps` | Don't cap 60fps → 30fps when the budget is tight |
| `--no-audio` | Drop audio instead of copying it |

## Verifying a download

Checksums are listed in each release's notes:

```powershell
certutil -hashfile squeeze.exe SHA256
```

Builds are produced by GitHub Actions, so you can also confirm a file came from
this repository's workflow:

```powershell
gh attestation verify squeeze.exe -R kayex/squeeze2
```

## Building from source

See [docs/development.md](docs/development.md).

## Licence

MIT — see [LICENSE](LICENSE). squeeze links the FFmpeg libraries under the
LGPL v2.1, built without GPL or non-free components.
