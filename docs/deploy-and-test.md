# Building & testing from a MacBook (no Windows machine)

The whole pipeline runs without booting Windows locally:

```
push to GitHub  ──►  GitHub Actions builds the binaries  ──►  download artifact
                                                                    │
                                          scp one file to a real RTX box  ◄─┘
                                                     │
                                            run it over SSH (NVENC)
```

Because each binary is a **single static binary** (FFmpeg linked in; NVENC
`dlopen`'d from the NVIDIA driver at runtime), deploying to a test machine is
"copy one file" — the test box needs **nothing** installed but its GeForce driver.

## Build — GitHub Actions

`.github/workflows/build.yml` builds the static `.exe` on `windows-latest` and
uploads it as the `squeeze-windows-x64` artifact. The runner needs **no GPU** to
build (NVENC is resolved at runtime, not link time). The first run compiles
FFmpeg via vcpkg (~20–40 min, cold); the vcpkg binary cache makes later runs fast.

From the Mac:

```bash
just ci         # trigger the workflow (or just `git push`)
just ci-watch   # follow the run to completion
just ci-fetch   # download the built binaries into ./dist
```

## Test — friend's Windows 11 + RTX PC (primary, free)

Highest-fidelity test: a real consumer GeForce GPU. One-time bootstrap, then
fully scriptable from the Mac.

### One-time bootstrap (needs ~15 min of interactive access)

You can't SSH in to enable SSH — so do this once via a screen-share (Windows
**Quick Assist**, Discord, etc.) or an in-person visit. In an **elevated
PowerShell** on the friend's PC:

```powershell
# 1. OpenSSH server (Tailscale SSH does NOT cover Windows hosts — use OpenSSH)
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Start-Service sshd
Set-Service -Name sshd -StartupType Automatic

# 2. Make PowerShell the default SSH shell (default is cmd.exe -> quoting pain)
New-ItemProperty -Path 'HKLM:\SOFTWARE\OpenSSH' -Name DefaultShell `
  -Value 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe' `
  -PropertyType String -Force

# 3. Tailscale (interactive browser login, or use a pre-made auth key)
winget install --id tailscale.tailscale -e
```

Add your Mac's SSH public key to `C:\Users\<friend>\.ssh\authorized_keys` (and
for admin accounts, `C:\ProgramData\ssh\administrators_authorized_keys`) so you
don't need his password. Note his Tailscale IP (`100.x.y.z`).

### Per-test (from the Mac, repeatable)

```bash
just ci-fetch                                   # get the latest binaries
just push-test  friend@100.x.y.z  ~/clip.mp4    # scp exe + clip to the box
just test-remote friend@100.x.y.z ~/clip.mp4    # run NVENC encode, pull result back
```

If NVENC fails, the app prints a clear error and you can retry with
`--encoder x264` to isolate whether it's NVENC or the pipeline.

## Test — cloud GPU Windows VM (friend-free backup, paid)

If the friend's box isn't available, rent a GPU by the hour. Same
copy-one-file-and-run flow.

- **AWS `g4dn.xlarge`** (NVIDIA T4 — same Turing NVENC family as many GeForce
  cards), Windows on-demand ~**$0.71–0.76/hr**. **Use a Marketplace AMI with the
  NVIDIA gaming/GRID driver preinstalled** (e.g. "NICE DCV for Windows" gaming
  variants) — a *bare* Windows AMI has **no** NVIDIA driver, so `nvEncodeAPI64.dll`
  is missing. Terminate when done; a smoke test costs cents.
- **Azure `Standard_NV6ads_A10_v5`** (1/6 of an A10), Windows ~**$0.45/hr** —
  slightly cheaper, but needs the Azure NVIDIA vGPU driver, and NVENC on a
  fractional vGPU partition is worth smoke-testing before relying on it.

> Driver floor: FFmpeg 8's NVENC needs an NVIDIA driver **≈570+**. Older/absent
> drivers fail cleanly (`ENOSYS` / "cannot load nvEncodeAPI64.dll") and the app
> falls back to software x264.
