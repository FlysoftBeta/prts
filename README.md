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

PRTS streams real Chromium pages over WebRTC using a CPU readback fallback.
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
node server
```

Open `http://127.0.0.1:8080`, enter a URL and create a Chromium instance
(the default source, labelled CPU fallback). Click the video to interact with
the real page. Select Synthetic for tests without Chromium. Use New tab,
the tab buttons and Resize to manage targets. A second browser tab can create
another instance or view an existing one; uncheck Request control to share an
instance already controlled by another viewer. Receiver and encoder stats appear
below the video. Network simulation supports bitrate caps, packet delay and loss.

Run the automated real-daemon/browser test (build prtsd first):

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

CPU capture, OpenH264/QSV CPU input and Job Object process-tree lifetime are
implemented. D3D11 shared textures and AppContainer/LPAC isolation remain later
phases. See [M3 acceptance/design](docs/prts/phase2-m3.md). See [the plan](docs/prts/PLAN.md) and the
[server](docs/prts/server-notes.md) / [daemon](docs/prts/prtsd-notes.md) notes.

Branch `main` in src always equals `PIN`; `work` = `main` + patches. `sync` refuses to run with uncommitted changes.
