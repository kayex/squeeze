# squeeze: instructions for Claude

squeeze compresses NVIDIA ShadowPlay clips small enough to share on Discord.
Rust workspace:

| crate | binary | what it is |
|---|---|---|
| `engine` | (lib) | size-targeted H.264 encode: probe, plan, transcode, size loop |
| `gui` | `squeeze.exe` | the app (egui/eframe). Owns the plain name |
| `cli` | `squeeze-cli.exe` | headless companion |

FFmpeg's `libav*` is linked in-process via rsmpeg, with NVENC; there is no
shelled-out `ffmpeg.exe`. Dev tasks go through **`just`**. Windows binaries are
built in GitHub Actions; macOS is dev-only (`--encoder x264`, no NVENC). See
`README.md` for the product, `docs/development.md` for build/CI/release/signing,
`docs/deploy-and-test.md` for GPU testing, `docs/rewrite-plan.md` for history.

## Writing style

**No em-dashes**, anywhere: prose, code comments, UI strings, commit messages,
release notes. Recast the sentence instead of swapping in a hyphen; a colon
often suits a term-then-definition line better than either.

## Verify before asserting

Claims in this repo have a habit of being plausible and wrong. Each of these was
believed, then disproved by actually checking:

- "1440p stays 1440p at 50 MB" (it was downscaled unconditionally).
- ImageMagick's `icon:auto-resize` writes a correct `.ico` (every entry came out
  uncompressed BMP; `icotool --raw` was needed).
- The control jitter was hover-related (it was selection-related).

So: run it, measure it, or read the dependency's source. Do not eyeball a
screenshot and call it verified, and do not write a number into the docs that
has not been produced by a command.

## Working on the GUI

`just shot [files...]` screenshots the running app. It captures **only** the app
window, never the rest of the screen. Needs macOS Screen Recording permission
for the terminal (granted) and `pyobjc-framework-Quartz`. `DELAY=14` catches a
mid-encode frame; `OUT=` sets the path.

Synthetic clicks need Accessibility permission, which is **not** granted, so the
UI cannot be driven programmatically. To inspect a state that idling will not
reach, temporarily change the default in `App::new`, rebuild, screenshot, then
restore it. Comparing those builds is also how to prove a layout does not shift.

Judge layout by measuring pixel columns, not by looking. Eyeballing compressed
screenshots produced three wrong diagnoses in a row.

## egui notes (0.35)

- **Button padding is resolved per widget state** (`widget_style.rs`:
  `button_padding + expansion - bg_stroke.width`), so selecting or hovering
  changes the allocated width and shoves neighbours sideways. Pin it: measure
  with `ui.painter().layout_job(...)` and pass the result as `min_size`.
- `with_layout` that is not wrapped in `ui.horizontal` claims **all** remaining
  vertical space and starves everything below it.
- `clear_color` defaults to near-black, so any area the UI does not paint shows
  as a dark band. The content frame must fill the viewport.
- There are **no mipmaps**: pre-scale images to roughly their display size or
  they alias badly (hence `assets/icon-96.png` for the 30pt header mark).
- The proportional font stack carries no arrows (U+2192); Hack is appended as a
  last-resort fallback so they render.
- Convention: technical values are monospace (IBM Plex Mono), labels are not.
- `selectable_label` paints nothing until selected or hovered, so options that
  are off look like static text. Use a `Button` with an explicit fill/stroke.

## Commit messages

Use **Conventional Commits with no scope**:

```
<type>: <imperative summary>
```

`<type>` must be exactly one of:

| type | use for |
|------|---------|
| `feat`     | a new user-facing capability |
| `fix`      | a bug fix |
| `refactor` | behavior-preserving code change |
| `chore`    | tooling, dependencies, housekeeping |
| `docs`     | documentation only (README, `docs/`) |
| `ci`       | CI / build pipeline (GitHub Actions, vcpkg, packaging) |
| `ai`       | changes to `CLAUDE.md` / agent instructions |

Rules:
- **No scope**: never `feat(ui):`, just `feat:`.
- Lowercase type; imperative, lowercase subject; no trailing period.
- One logical change per commit; pick the single best-fitting type.

Examples: `feat: add drag-and-drop file queue`, `fix: clamp target bitrate floor`,
`ci: cache vcpkg installed tree`, `ai: add commit convention`.
