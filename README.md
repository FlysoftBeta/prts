# PRTS

PRTS streams headless Chromium to a web viewer over WebRTC, with remote input.
Whole-window mode includes the tab strip, omnibox and toolbar; separate flyouts
and touch input are still being implemented. See [current status](docs/prts/STATUS.md).

```
patched headless Chromium --(anonymous pipes: control, input, frames)--> prtsd (Rust, windows-rs)
   prtsd: capture (GPU shared D3D11 texture or CPU readback) -> H.264 (Intel QSV / OpenH264) -> WebRTC
   prtsd --(framed stdio)--> Node control plane (server/) --(WebSocket signalling)--> browser frontend
```

## Repository layout
| Path | Purpose |
|---|---|
| `prtsd/` | Rust daemon: browser launcher, pipe transport, encoder, WebRTC |
| `server/` | Node control plane and vanilla JS viewer (`server/public`), tests and E2E |
| `patches/` | Chromium patches (git format-patch) carried on top of `PIN` |
| `docs/prts/` | Plan, protocol, status, milestone notes (start with `STATUS.md`, `PLAN.md`) |
| `PIN`, `args.gn`, `cr.cmd`, `scripts/` | Chromium checkout/build workflow (below) |

The Chromium checkout and build outputs live outside this repo (`C:\src\chromium`) and are never committed.
Current status: `docs/prts/STATUS.md`.

---

# Chromium workflow

Infra for a pinned, history-less Chromium checkout with local commits carried as patches.
The checkout itself is **not** in this repo: it lives at `C:\src\chromium` (override with `CR_ROOT`).

| Path | Purpose |
|---|---|
| `PIN` | Upstream `chromium/src` commit the checkout is synced to |
| `patches/` | Own commits, `git format-patch` output, applied in name order |
| `args.gn` | Default GN args for new out dirs |
| `cr.cmd` → `scripts/cr.ps1` | All commands |

## Machine prerequisites
VS 2026 Community (Desktop C++, ATL/MFC, ARM64 tools), Windows SDK 10.0.28000 with Debugging Tools,
Git, depot_tools at `C:\src\depot_tools` on user PATH, `DEPOT_TOOLS_WIN_TOOLCHAIN=0`, long paths on.

## Commands
```
cr init               # new machine: shallow checkout of src@PIN + apply patches
cr status
cr pin [rev|latest]   # change PIN only (commit hash, ref like refs/branch-heads/7000, or latest)
cr sync               # export work, sync src+deps to PIN without history, re-apply patches
cr update             # pin latest + sync
cr export / apply
cr gen | build [out] [target] | run [out]
```

## Daily loop
1. In `C:\src\chromium\src` on branch `work`: edit, `git commit`.
2. `cr export`, then commit `patches/` (and `PIN` if changed) in this repo.
3. Rebase onto newer upstream: `cr update`. If `git am` conflicts: fix in src, `git am --continue`, `cr export`.

## PRTS: how to run

PRTS captures D3D11 shared textures and feeds the Intel hardware H.264 encoder
through Media Foundation, with CPU capture and OpenH264 software fallbacks.
It needs Node.js 22+, Rust stable MSVC and Visual Studio C++ tools/Windows SDK
on Windows x64, plus the patched Chromium build. Synthetic mode remains
available without Chromium.

From the repository root in PowerShell:

```powershell
cd prtsd
cargo build --release
cd ../server
npm ci
cd ..
# Optional; the default is this patched component build:
$env:PRTS_CHROME_PATH='C:/src/chromium/src/out/Default/chrome.exe'
# Whole-window capture and Chrome UI input (v1 page-only compatibility is still the default):
$env:PRTS_BROWSER_PROTOCOL='2'
node server
```

Open `http://127.0.0.1:8080`, enter a URL and create a Chromium instance
(the default source). Choose `auto` for hardware encoding with software fallback,
or `qsv` to request the hardware backend. Click the video to interact with
the page or Chrome toolbar in whole-window mode. Select Synthetic for tests without Chromium. Use New tab,
the tab buttons and Resize to manage targets. A second browser tab can create
another instance or view an existing one; uncheck Request control to share an
instance already controlled by another viewer. Receiver and encoder stats appear
below the video. Network simulation supports bitrate caps, packet delay and loss.

Run checks from repo root with one command:

```powershell
powershell -File scripts/test.ps1              # build, Rust tests, Clippy/fmt, Node unit tests
powershell -File scripts/test.ps1 -Suite full  # plus real Chromium, GPU/QSV, CPU/software and WebRTC gates
powershell -File scripts/test.ps1 -Suite flyouts # WIP app/context menus, scale/credits and GPU composition
```

The full suite requires the patched Chromium build, Intel hardware encoding and
installed Chrome/Edge for the viewer (override with `PRTS_BROWSER`). Gates run
sequentially with deadlines, separate stdout/stderr logs and exit-status files
under `%TEMP%`; use `-LogPrefix C:/path/prefix` to choose their location. Use
`-Suite mf` for the hardware encoder gates alone, or `-Suite gpu` for the
production auto-capture Intel adapter check (both require built binaries).
In a disconnected Windows
session, `-AllowDisconnectedDesktop` permits the existing explicit override;
foreground/cursor qualification still requires a connected desktop.

The flyout suite currently qualifies app and right-click context menus; other flyout types remain
in progress on `wip/mvp-flyouts`. The headless launcher rasterizes Views UI tiles
on CPU to avoid cold UI shader stalls. Page GPU rasterization, hardware
compositing, WebGL and shared-texture capture remain enabled.

Individual browser tests (build prtsd first):

```powershell
cd server
npm run e2e          # synthetic source regression
npm run e2e:browser  # real PRTS pages, decoded pixels/input, two instances
```

It uses installed Chrome/Edge or `C:/src/chromium/src/out/Default/chrome.exe`
headlessly. Set `$env:PRTS_BROWSER` to override the browser executable. It starts
its own server on a free loopback port, verifies decoded video/input, stats,
multiple instances/viewers and bitrate changes, saves
`docs/prts/phase1-e2e.png`, and verifies server/daemon shutdown. `npm test` runs
the separate mock/unit suite; `cargo test --release` in `prtsd/` runs the Rust
integration suite. If PATH is stale, prepend `C:/Program Files/nodejs` and
`$env:USERPROFILE/.cargo/bin`.

GPU/CPU capture, QSV DXGI/CPU input, OpenH264 and Job Object process-tree lifetime
are implemented. Isolation is paused on `wip/i1-isolation`; the shipped profile
remains unchanged. See [M5 design and qualification](docs/prts/phase2-m5-design.md),
[MF latency](docs/prts/mvp-mf-latency.md), [the plan](docs/prts/PLAN.md) and the
[server](docs/prts/server-notes.md) / [daemon](docs/prts/prtsd-notes.md) notes.

Branch `main` in src always equals `PIN`; `work` = `main` + patches. `sync` refuses to run with uncommitted changes.
