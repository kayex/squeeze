# squeeze2 dev tasks — run `just` to list them.
# Local recipes target macOS (system FFmpeg, no NVENC). The Windows .exe is
# built in GitHub Actions (see the ci-* recipes) — see docs/deploy-and-test.md.
set shell := ["bash", "-euo", "pipefail", "-c"]

artifact := "squeeze-windows-x64"

ffmpeg_prefix := `brew --prefix ffmpeg 2>/dev/null || echo /opt/homebrew/opt/ffmpeg`

# Env so cargo links the system (Homebrew) FFmpeg on macOS. No NVENC here.
export PATH := "/opt/homebrew/bin:/usr/local/bin:" + env_var('PATH')
export PKG_CONFIG_PATH := ffmpeg_prefix / "lib/pkgconfig"
export LIBCLANG_PATH := "/Library/Developer/CommandLineTools/usr/lib"
export DYLD_FALLBACK_LIBRARY_PATH := ffmpeg_prefix / "lib"

# List available recipes
default:
    @just --list

# Type-check the workspace (macOS, system FFmpeg)
check:
    cargo check -p cli --features system

# Release build of the CLI (macOS, system FFmpeg)
build:
    cargo build -p cli --features system --release

# Compress a file locally (macOS dev: x264, since macOS has no NVENC).
# Extra args pass through, e.g.: just run ~/clip.mp4 --max-mb 8
run FILE *ARGS:
    cargo run -p cli --features system --release -- --encoder x264 {{ARGS}} "{{FILE}}"

# Launch the drag-and-drop GUI. Optional files are queued immediately.
gui *FILES:
    cargo run -p gui --features system --release -- {{FILES}}

# Generate a ShadowPlay-like test clip (1080p60 H.264 High + AAC, 30s)
sample OUT="/tmp/shadowplay_sample.mp4":
    ffmpeg -hide_banner -y \
      -f lavfi -i "testsrc2=size=1920x1080:rate=60:duration=30" \
      -f lavfi -i "sine=frequency=440:duration=30" \
      -c:v libx264 -profile:v high -preset veryfast -b:v 45M -pix_fmt yuv420p \
      -c:a aac -b:a 160k -movflags +faststart "{{OUT}}"
    @echo "wrote {{OUT}}"

# Build assets/icon.ico (+ icon-256.png) from a logo: SVG, PNG, or anything
# ImageMagick reads. Windows needs ONE .ico holding several sizes — Explorer,
# the taskbar and Alt-Tab each pick a different one.
#   just icon ~/Desktop/logo.svg
#
# Needs: brew install imagemagick icoutils librsvg
icon SOURCE OUT="assets/icon.ico":
    #!/usr/bin/env bash
    set -euo pipefail
    src="{{SOURCE}}"; out="{{OUT}}"
    [ -f "$src" ] || { echo "no such file: $src" >&2; exit 1; }
    for t in magick icotool; do
        command -v $t >/dev/null || { echo "missing $t — brew install imagemagick icoutils" >&2; exit 1; }
    done
    # Microsoft's baseline is 16/24/32/48/256; 64 and 128 fill in HiDPI steps.
    sizes="16 24 32 48 64 128 256"
    tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

    case "${src##*.}" in
      svg|SVG)
        command -v rsvg-convert >/dev/null || { echo "missing rsvg-convert — brew install librsvg" >&2; exit 1; }
        # Render each size straight from vector: far crisper at 16/24px than
        # downscaling one big raster.
        for s in $sizes; do rsvg-convert -w "$s" -h "$s" "$src" -o "$tmp/$s.png"; done
        ;;
      *)
        w=$(magick identify -format '%w' "$src[0]")
        h=$(magick identify -format '%h' "$src[0]")
        [ "$w" = "$h" ] || echo "note: source is ${w}x${h} (not square) — padding to square on transparency"
        if [ "$w" -lt 256 ] || [ "$h" -lt 256 ]; then
            echo "warning: source is only ${w}x${h} — supply >=256x256 (or an SVG) for a sharp large icon"
        fi
        m=$(( w > h ? w : h ))
        magick "$src[0]" -background none -gravity center -extent "${m}x${m}" "$tmp/sq.png"
        for s in $sizes; do
            magick "$tmp/sq.png" -resize "${s}x${s}" -background none -gravity center \
                   -extent "${s}x${s}" -depth 8 "PNG32:$tmp/$s.png"
        done
        ;;
    esac

    mkdir -p "$(dirname "$out")"
    # icotool, not ImageMagick: `-define icon:auto-resize` stores EVERY entry as
    # uncompressed BMP (a 256px entry alone is ~270 KB). Windows expects the
    # 256px entry PNG-compressed, which is what --raw does.
    icotool -c -o "$out" \
        "$tmp/16.png" "$tmp/24.png" "$tmp/32.png" "$tmp/48.png" "$tmp/64.png" "$tmp/128.png" \
        --raw="$tmp/256.png"
    # eframe needs a plain PNG for the window/Alt-Tab icon.
    cp "$tmp/256.png" "$(dirname "$out")/icon-256.png"
    echo "wrote $out and $(dirname "$out")/icon-256.png"
    just _ico-info "$out"

# Print the internal layout of an .ico (size, bytes, BMP vs PNG per entry).
_ico-info ICO="assets/icon.ico":
    #!/usr/bin/env python3
    import struct
    d = open("{{ICO}}", "rb").read()
    n = struct.unpack_from("<H", d, 4)[0]
    print(f"  {len(d):,} bytes, {n} entries")
    for i in range(n):
        w, h, *_rest, size, off = struct.unpack_from("<BBBBHHII", d, 6 + i * 16)
        kind = "PNG" if d[off:off+8] == b"\x89PNG\r\n\x1a\n" else "BMP"
        print(f"    {w or 256:3}x{h or 256:<3} {size:7,} B  {kind}")

# Preview an .ico: shows the small sizes magnified so you can judge legibility.
icon-preview ICO="assets/icon.ico" OUT="assets/icon_preview.png":
    #!/usr/bin/env bash
    set -euo pipefail
    tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
    # Frame order inside an .ico varies, so split and select by geometry.
    magick "{{ICO}}" -background none -set filename:s '%wx%h' "$tmp/f_%[filename:s].png"
    args=""
    for s in 256 48 32 16; do
        f="$tmp/f_${s}x${s}.png"
        [ -f "$f" ] || continue
        # Magnify the small entries with nearest-neighbour so their real pixels show.
        if [ "$s" = "256" ]; then magick "$f" -resize 256x256 "$tmp/l$s.png"
        else magick "$f" -filter point -resize 128x128 "$tmp/l$s.png"; fi
        args="$args $tmp/l$s.png"
    done
    magick $args +append -background '#1e1e22' -flatten "{{OUT}}"
    echo "wrote {{OUT}} (256px, then 48/32/16px magnified)"

# Format and lint
fmt:
    cargo fmt --all

clippy:
    cargo clippy -p cli --features system

# Remove build artifacts
clean:
    cargo clean

# Tag a release and push it — CI then builds and drafts the GitHub release.
#   just release 0.1.0
release VERSION:
    #!/usr/bin/env bash
    set -euo pipefail
    v="{{VERSION}}"
    [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || { echo "version should look like 1.2.3, got '$v'" >&2; exit 1; }
    [ -z "$(git status --porcelain)" ] || { echo "working tree is dirty — commit first" >&2; exit 1; }
    # Keep the tag and the crate versions in step; FileVersion comes from Cargo.
    for f in crates/cli/Cargo.toml crates/gui/Cargo.toml; do
        have=$(grep -m1 '^version = ' "$f" | cut -d'"' -f2)
        [ "$have" = "$v" ] || { echo "$f says version $have, tagging $v — bump it first" >&2; exit 1; }
    done
    git tag -a "v$v" -m "squeeze v$v"
    git push origin "v$v"
    echo "pushed tag v$v — watch: gh run watch \$(gh run list --workflow release.yml --limit 1 --json databaseId -q '.[0].databaseId')"

# --- CI (Windows .exe builds on GitHub Actions) ---

# Trigger the Windows build workflow
ci:
    gh workflow run build.yml

# Follow the most recent build run to completion
ci-watch:
    gh run watch "$(gh run list --workflow build.yml --limit 1 --json databaseId -q '.[0].databaseId')"

# Download the latest built squeeze.exe into ./dist
ci-fetch:
    rm -rf dist && gh run download --name "{{artifact}}" -D dist
    @ls -la dist

# --- Deploy to a real RTX box over Tailscale/SSH (see docs/deploy-and-test.md) ---

# Copy squeeze.exe + a clip to the test box. e.g.: just push-test me@100.x.y.z ~/clip.mp4
push-test HOST CLIP REMOTE="C:/Users/Public/squeeze":
    ssh {{HOST}} powershell -NoProfile -Command "New-Item -ItemType Directory -Force '{{REMOTE}}' | Out-Null"
    scp dist/squeeze.exe "{{CLIP}}" "{{HOST}}:{{REMOTE}}/"

# Run the NVENC encode on the test box and pull the result back into ./dist
test-remote HOST CLIP REMOTE="C:/Users/Public/squeeze":
    ssh {{HOST}} powershell -NoProfile -Command "& '{{REMOTE}}/squeeze.exe' --encoder nvenc '{{REMOTE}}/{{ file_name(CLIP) }}'"
    scp "{{HOST}}:{{REMOTE}}/*_discord.mp4" ./dist/
    @echo "pulled output into ./dist"
