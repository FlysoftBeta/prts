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

Branch `main` in src always equals `PIN`; `work` = `main` + patches. `sync` refuses to run with uncommitted changes.
