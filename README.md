<div align="center">

<img src="assets/icon-256.png" width="88" alt="">

# squeeze

**Drop in a gameplay clip. Get back one small enough to post on Discord.**

No settings to learn, nothing to install, and your GPU does the work.

</div>

<!-- TODO: screenshot of the app with a couple of files queued goes here -->

---

## Why

Discord caps uploads at **10 MB** on a free account. A few seconds of ShadowPlay
footage blows straight past that, and the usual answers are a sketchy web
converter, a 500 MB video editor, or memorising FFmpeg flags.

squeeze does the one thing: it re-encodes your clip to land *just under* the
limit, at the best quality that fits, and saves it next to the original.

## Get it

**[⬇ Download the latest release](https://github.com/kayex/squeeze2/releases/latest)** — grab `squeeze.exe` and run it. That's the whole install.

It's a single self-contained file. No installer, no FFmpeg, no runtime, no
registry entries. Delete the file and it's gone.

> **Windows will warn you the first time.**
> You'll see *"Windows protected your PC"* with an unknown publisher. Click
> **More info → Run anyway**. This happens because the app isn't code-signed —
> certificates cost money to keep, and this is a free project. If you'd rather
> check before trusting it, every release ships SHA-256 checksums and a GitHub
> build attestation proving the file came from the public build workflow in this
> repo. [More on that below](#verifying-your-download).

## Use it

1. Open **squeeze.exe**.
2. Drag one or more clips onto the window.
3. Pick a size limit — **10 MB** (Discord free), **50 MB** (Nitro Basic) or
   **500 MB** (Nitro).

That's it. Each clip is saved beside the original with a `_discord` suffix, so
`clip.mp4` becomes `clip_discord.mp4`. **Your originals are never modified.**

You can also drag files straight onto `squeeze.exe` in Explorer to queue them.

## What you get

- **It actually fits.** squeeze measures the finished file and re-encodes at a
  lower bitrate if it overshot, so "under 10 MB" means under 10 MB.
- **Plays inline on Discord** instead of forcing a download — the output is
  H.264 MP4 with the metadata moved to the front, which is what Discord's
  in-app player wants.
- **Fast, and easy on your CPU.** Encoding runs on your NVIDIA GPU (NVENC), so
  a 30-second 1080p60 clip takes a few seconds and leaves the CPU free.
- **Keeps your audio.** The original audio track is copied across untouched
  rather than re-compressed.
- **Handles what ShadowPlay actually records** — MP4 and MKV, H.264, HEVC and
  AV1, variable frame rate, high frame rates, 1440p and 4K.
- **Batches.** Drop in twenty clips and walk away.

When a clip can't fit at full size, squeeze scales it down (1440p/4K → 1080p, or
lower) and caps very high frame rates, rather than handing you a smeared mess at
the original resolution.

## Requirements

- **Windows 10 or 11**, 64-bit.
- **An NVIDIA GPU** with a reasonably current driver (**570 or newer**) for
  hardware encoding. Without one, squeeze falls back to software encoding
  automatically — slower, but the same result.

## Verifying your download

Optional, but here if you want it. Checksums are in every release's notes:

```powershell
certutil -hashfile squeeze.exe SHA256
```

And because the binaries are built in public by GitHub Actions, you can confirm
that the file you downloaded came from this repository's workflow:

```powershell
gh attestation verify squeeze.exe -R kayex/squeeze2
```

## Command line

There's a `squeeze-cli.exe` in the release zip for scripting and batch jobs:

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

## Building from source

See **[docs/development.md](docs/development.md)**. Short version: it's a Rust
workspace, Windows builds happen in GitHub Actions, and `just` runs everything.

## Licence

MIT — see [LICENSE](LICENSE). squeeze links the FFmpeg libraries under the
LGPL v2.1, built without GPL or non-free components.
