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
- **The stream is the WHOLE browser window including Chrome UI** (tab strip, omnibox, toolbar,
  menus, bubbles, context menus, dialogs, permission prompts) — NOT just page content. The user
  operates the real Chrome UI remotely. One stream = one browser window; multi-window = multiple
  browser windows (each its own stream/track). Input goes to the browser window (aura/views level),
  so clicks on tabs/omnibox and accelerators (Ctrl+T/L/W) work. Popup widgets that are separate
  top-level windows (menus, omnibox dropdown, bubbles) must appear in the stream (composited).
- Wire out: multi-window management, input forwarding (mouse/keyboard/wheel
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
- Encoders: Intel QSV via **oneVPL directly** required (the Intel MF hardware MFT is only an
  interim backend — fewer low-latency knobs, more overhead); D3D11 texture input zero-copy
  (MFX_IOPATTERN_IN_VIDEO_MEMORY / external D3D11 surfaces), AsyncDepth=1, LowDelayBRC,
  no B-frames, intra refresh option; keep the MF backend as fallback; NVENC optional.
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

**Priority order (user, revised 2026-10-21 16:15 — SUPERSEDES all lists below):**
1. Finish publishing `v0.1.0-preview` NOW. Flyout latency (>250 ms) and flaky omnibox-popup checks are
   KNOWN ISSUES deferred to item 7: for release gating they are report-only (record results in the release
   notes "Known issues"); do NOT debug them now. All other gates must pass. Then move on to item 2.
2. oneVPL: direct Intel QSV encoder backend (D3D11 zero-copy input, low-latency settings, VBV fix for low
   bitrates), MF kept as fallback; benchmark MF vs oneVPL into `docs/prts/encoder-bench.md`.
3. I1 isolation: resume from `wip/i1-isolation` (journal-i1.md "Resume later"): Job Object + AppContainer
   AND LPAC working with the hardware GPU, per-tenant window station/desktop, cross-tenant tests.
4. Release `v0.2.0` (same packaging/verification procedure as v0.1.0-preview, non-component out\Release).
5. Patch re-architecture: squash the whole series into one diff and find better hook points (see item 3 of
   the older list below for the method).
6. NVENC backend.
7. Last: investigate why flyouts are slow to appear (latency >250 ms seen even on GPU/QSV; see
   lessons-flyouts.md and journal-mvp.md for the measurements so far).

**Older order (revised 2026-10-21 07:30, superseded):**
1. MVP WRAP-UP (now): stop chasing the flyout latency gate. Keep the flyout code that already works
   (app menu, context menu, select, omnibox popup appear and take input) and merge it to main with gates
   that pass (latency report-only); write the lessons learned to `docs/prts/lessons-flyouts.md`
   (what worked, what failed, measured numbers, root causes found). Finish cleanup (README, one-command
   tests). Touch input only if it's quick; otherwise list it as known gap.
   **MVP-required: Google Chrome identity** (reduce extra bot checks): the UA string must look exactly like
   stable Google Chrome on Windows (no "HeadlessChrome", no "Chromium"; reduced-UA format
   `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/<major>.0.0.0 Safari/537.36`),
   and User-Agent Client Hints must be consistent with it everywhere: `Sec-CH-UA` / `Sec-CH-UA-Full-Version-List`
   brands = "Google Chrome" + "Chromium" + GREASE brand (same order/format as real Chrome), `Sec-CH-UA-Platform`,
   `-Mobile`, `-Platform-Version`, `-Arch`, `-Bitness`, and JS `navigator.userAgent`,
   `navigator.userAgentData.brands` / `getHighEntropyValues()` in pages, workers and service workers. Also
   check other headless tells reachable from pages (e.g. `navigator.webdriver` false, `window.chrome` present,
   plugins/mimeTypes, screen/window outer sizes non-zero and consistent with the window). Do it at the
   embedder level (one small hook, e.g. brand/product in embedder_support user-agent utils + headless
   product override) rather than many patches. Gate: a test page dumping headers + JS values compares
   against expected Chrome values for our PIN's major version.
   **MVP-required: product branding = "PRTS"** — the browser's own branding (chrome://settings/help "About",
   chrome://version, window/app titles, resource/version info) shows PRTS. Use Chromium's standard branding
   mechanism (`chrome/app/theme/chromium/BRANDING` and the chromium-branded resources/strings) rather than
   ad-hoc string patches. Web-facing identity stays Google Chrome (UA + client hints above), so the UA/brand
   hook must not derive from the product name. Do the branding change EARLY (it triggers a wider rebuild;
   out\Release is building now and will pick it up incrementally).
   **All Chromium changes must be as small and elegant as possible**: prefer one well-placed hook over many,
   use existing extension points (embedder/content-client overrides, BRANDING, features/switches), no
   copy-pasted code, each patch focused and documented in its commit message.
2. RELEASE: publish a GitHub release on FlysoftBeta/prts (e.g. `v0.1.0-preview`, marked pre-release):
   zipped PRTS Chromium binaries from the NON-component release build `out\Release` (component off, dcheck
   off, symbol_level=0; full build started 2026-10-21 07:38 via scripts/build-release.ps1, log
   C:\src\chromium\prts-release-build.log, ~6-8 h). Before packaging, rebuild it incrementally with the
   final patches (`autoninja -C out\Release chrome`); never start another full build; don't run two
   ninja builds on out\Release at once. prtsd release binaries, the Node server + frontend (without node_modules; with install steps),
   a quickstart in the release notes, and SHA256 sums. Verify the zip runs from a clean extract dir.
   Use `gh release create`. Keep assets < 2 GB each (split if needed).
3. Then: unify flyout and window code paths (one window/surface model); minimize Chromium patch surface
   (fewer, smaller hooks; move logic out of Chromium into prtsd where possible).
   **Patch re-architecture (after the MVP release):** don't review patch by patch. Squash the whole
   series into ONE combined diff against PIN, treat it as a behavioural spec (what capabilities it adds:
   pipe/control, window capture, flyouts, input, cursor, branding/UA, ...), and independently search the
   Chromium tree for the best hook points to provide those capabilities with the smallest diff
   (existing extension points, embedder/content-client overrides, observers, features/switches, public
   APIs, or moving logic into prtsd). Record findings in `docs/prts/patch-rearch.md`. Re-implement on a fresh
   branch as a small, clean, logically ordered series, with every gate passing before it replaces `main`'s
   series. Goal: minimal diff to upstream, easy rebase onto new Chromium versions.
4. Then: encoder backends (oneVPL QSV, NVENC; MF fallback) and isolation (AppContainer + LPAC, per-tenant
   window station; resume from `wip/i1-isolation`).
5. Then: re-integrate everything, full regression, new release.

**Older order (superseded):**
1. Isolation PAUSED (I1 WIP parked; default profile = none).
2. MINIMUM USABLE STATE ("MVP"), then commit + push:
   - Chromium hardware GPU acceleration verified working (not SwiftShader/WARP) in the shipped config.
   - Media Foundation (Intel QSV MFT) encoder with a proper low-latency setup (CODECAPI low-latency
     mode, no B-frames, CBR/low-delay rate control, small VBV, GOP/intra-refresh policy, async/queue depth
     minimal), measured latency documented.
   - Common flyout handling: menus, context menus, omnibox dropdown, bubbles, tooltips, dialogs,
     permission prompts, select popups, extension/profile popups appear in the stream and take input
     (m5b scope) — plus one stream per browser window.
   - Touch handling: client touch/pointer events -> Chromium touch events (multi-touch, gestures like
     scroll/pinch, tap) through the window-level input path; frontend sends touch on touch devices.
   - Codebase organized: dead/obsolete code from the tab-surface era removed or clearly marked,
     consistent module layout, README run steps accurate, tests runnable with one command.
3. Better codec backends: oneVPL QSV direct, NVENC; MF kept as fallback; benchmark.
4. Isolation: BOTH AppContainer and LPAC working (with hardware GPU), plus per-tenant window station/desktop.

Definition of "usable": `node server` -> open frontend -> create instance -> see the PRTS page
rendering live via WebRTC, click/type into it, open a 2nd instance, see stats.

## Git
Commit to `main` in this repo at meaningful milestones (small commits ok). End commit messages with:
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
