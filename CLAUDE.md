# squeeze: instructions for Claude

squeeze compresses NVIDIA ShadowPlay clips small enough to share on Discord.
Rust workspace: **`engine`** (size-targeted H.264 encode library, in-process
FFmpeg/`libav*` via rsmpeg, NVENC) + **`cli`** (the `squeeze` binary). Dev tasks
go through **`just`** (see the `justfile`). The Windows `.exe` is built in GitHub
Actions; macOS is dev-only (`just build` / `just run`, x264). See `README.md`,
`docs/rewrite-plan.md`, and `docs/deploy-and-test.md`.

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
