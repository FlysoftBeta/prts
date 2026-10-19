# PRTS — cloud browser framework: master plan & handoff

This file is the single source of truth for requirements and orchestration rules.
Progress lives in `docs/prts/STATUS.md` (append-only log + current state). Every
orchestrator MUST read both before acting and update STATUS.md before exiting.

## Architecture

```
PRTS (custom headless Chromium)  --anonymous pipes-->  prtsd (Rust, windows-rs)
                                                        |  anonymous pipes (stdio) -> control plane (Node.js sample server)
                                                        |  WebRTC (VP8/VP9/H264, latency first) -> browser client
```

## Requirements

### 1. Chromium custom build (codename PRTS)
- Base: `--headless=new`. Must not touch the Windows desktop (no windows, no focus issues,
  no synthetic OS-level clicks).
- Wire out: multi-window (multi-target/tab) management, input forwarding (mouse/keyboard/wheel
  injected at the content/RenderWidgetHost level), frame/texture forwarding.
- Efficient GPU texture path: frames as D3D11 shared textures (NT handles duplicated into prtsd)
  so prtsd can zero-copy feed a HW encoder. No DWM / readback paths in the fast path.
  A CPU-readback fallback is acceptable as a stepping stone but the target is zero-copy.
- Minimal invasiveness for cross-version portability. Reference ideas (NOT verified, never
  compiled — ideas only): https://github.com/FlysoftBeta/chromium-output-investigation.git
- Source + a clean full first build: `C:\src\chromium` (out\Default, component release+dcheck;
  full build took ~6.5h on 16 threads; incremental should be much faster).
- Infra repo (this repo): `PIN`, `patches/`, `args.gn`, `cr.cmd` -> `scripts/cr.ps1`. Carry our
  Chromium changes as patches in `patches/` using the `cr` subcommands (read scripts/cr.ps1).
  depot_tools at `C:\src\depot_tools`, `DEPOT_TOOLS_WIN_TOOLCHAIN=0`.
- Builds are long: run detached, poll with long sleeps, never block an agent context on them.

### 2. prtsd (Rust)
- Official `windows` crate (windows-rs), latest dependencies.
- Multi-tenancy isolation via Windows primitives (server is Win10 22H2): investigate
  Job Objects (limits, kill-on-close, UI restrictions), AppContainer / LPAC, restricted tokens,
  separate desktops/window stations, integrity levels. MUST keep GPU acceleration working
  (Chromium GPU process inside AppContainer needs care — verify GPU is not falling back to SwiftShader/WARP).
- Encoders: Intel QSV (oneVPL, or Media Foundation HW MFT on Intel) required; NVENC optional.
  Also a software fallback (e.g. openh264 / libvpx) is fine for development on machines without QSV.
- Manages browser instances; talks to each browser over anonymous pipes (frames/texture handles,
  input, control).
- Talks to the control plane over anonymous pipes (its stdin/stdout, framed protocol).
- WebRTC to the client (event channel = DataChannel; video track). Latency first
  (no B-frames, low-latency rate control, small jitter buffer hints, playout-delay ext if possible).
  A Rust WebRTC stack such as `webrtc-rs` or `str0m` is acceptable.

### 3. Node.js sample server (control plane)
- Spawns and controls prtsd over anonymous pipes.
- Sample web frontend: shows video, controls (mouse/keyboard/wheel), instance management
  (create/close/navigate/list tabs), network condition simulation (bandwidth/latency/loss —
  e.g. via encoder bitrate caps + an injectable shaping layer), connection quality display
  (RTT, fps, bitrate, packet loss, jitter, decode latency from getStats).
- Signaling for WebRTC goes client <-> Node <-> prtsd.

## Delegation rules (MUST follow)
- ALL tool-heavy / context-heavy work (reading Chromium source, writing code, building,
  debugging) goes to **Codex**, model `gpt-6.1-sol` (NOT gpt-6-sol), reasoning medium (high for
  hard bugs). Verified invocation:
  `codex exec -m gpt-6.1-sol -c model_reasoning_effort=medium --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox -C <dir> "<prompt>"`
  Prefer passing long prompts via a prompt file on stdin (`- < prompt.md`). Use `-o <file>`
  (`--output-last-message`) to capture the final report so you only read a short summary.
  Run long Codex jobs in the background and wait on them; give each a timeout.
- Codex may run in parallel (2-3 jobs max, on disjoint files) but pace it: don't burn the 5h
  window quickly. Check Codex rate-limit status between jobs (e.g. `codex exec --json` output
  includes rate-limit info / token counts; or ask a cheap codex job to run `/status` equivalent).
  If the weekly limit is exhausted and a banked reset exists, it may be used.
- If Codex is exhausted: WAIT (long sleeps) for the reset. Never fall back to Claude subagents
  for grunt work — Claude budget is small.
- Claude agents only orchestrate: write prompts, read short Codex reports, decide next step.
  Do not read large source files yourself.
- Ask Codex to write its findings/designs into files under `docs/prts/` so knowledge persists
  across contexts, and to keep its final message short (<= 40 lines).

## Repo layout (target)
- `patches/` — Chromium patches (via cr.ps1)
- `prtsd/` — Rust crate/workspace
- `server/` — Node.js control plane + `server/public` frontend
- `docs/prts/` — PLAN.md, STATUS.md, protocol.md, isolation.md, design notes

## Phases
0. Design: protocol spec (browser<->prtsd pipe protocol, prtsd<->control plane protocol),
   Chromium hook points (validated against actual C:\src\chromium sources), isolation design.
1. prtsd skeleton + Node server + frontend using a *stub frame source* (synthetic frames) so the
   WebRTC/encode/input/quality pipeline works end-to-end without Chromium. Software encoder OK first, then QSV.
2. Chromium patches: headless=new embedder hooks for frame export (shared D3D11 texture),
   input injection, multi-window control over pipes. Build incrementally, test with prtsd.
3. Integrate: zero-copy texture -> QSV encode; isolation (Job Object + AppContainer) with GPU verified.
4. Polish: network simulation, quality stats, multi-instance, docs (README with run steps).

Definition of "usable": `node server` -> open frontend -> create instance -> see the PRTS page
rendering live via WebRTC, click/type into it, open a 2nd instance, see stats.

## Git
Commit to `main` in this repo at meaningful milestones (small commits ok). End commit messages with:
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
