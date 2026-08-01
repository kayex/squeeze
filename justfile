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

# Format and lint
fmt:
    cargo fmt --all

clippy:
    cargo clippy -p cli --features system

# Remove build artifacts
clean:
    cargo clean

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
