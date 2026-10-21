# PRTS status

## Current state
- **Revised-plan MVP wrap-up active (2026-10-21, supersedes older next actions below):**
  both repos remain on `wip/mvp-flyouts`; preserve the four already working
  app/context/select/omnibox paths. Standard PRTS branding and shared Windows
  Chrome UA/Client Hints hook are applied but await incremental build/runtime
  qualification. Chromium WIP `92256ff842` is exported as31 patches; infra WIP
  `dda95d3` is pushed. Missing direct GN Views deps were fixed and GN check PASS;
  Rust release/tests/Clippy/fmt and Node unit checks PASS. Baseline identity gate
  correctly rejects build40's HeadlessChrome/two-brand values. CPU/software
  flyout latency is report-only, with all pixel,
  input, restoration and resource assertions retained; GPU/QSV keeps250ms.
  Added production-launch HTTP/JS identity gate covering page, dedicated/shared
  workers and service workers. Lessons: [lessons-flyouts.md](lessons-flyouts.md).
  Touch, other flyouts and independent tracks for extra windows remain known gaps.
- **Next action:** inspect bounded incremental build `prts-mvp-wrap45.*` (7200s).
  Wrap44 parsed the manifest in~10min and completed113 actions, then failed only
  the UA constant-false branch under `-Wunreachable-code`; it now uses a normal
  build guard. About7k branding-dependent actions remain. Profiling `.sisorc`
  was removed. Previous wrap41/42/43 were stopped during preparation, without
  deleting state or outputs. Then qualify identity
  and full regression, export patches, commit/merge qualified wrap-up to main and
  push. Then finish the existing non-component Release build (PID10124, started
  07:38; do not start another), rebuild it incrementally with final patches,
  package/clean-extract verify and publish the GitHub preview. Re-architecture,
  encoders and isolation follow the release in PLAN order. Exact commands/logs
  are in [mvp-flyouts-wip.md](mvp-flyouts-wip.md) and journal-mvp.md.

### Earlier WIP detail (historical next actions superseded above)
- **Active WIP: `wip/mvp-flyouts` in both repos**, not merged into main. Chromium `0f563c2575`
  (build30, exported27 patches) adds owned-layer scenes, fair two-resource alpha capture and correct
  Aura mouse-release flags for Views buttons. Daemon CPU/GPU composition and512MiB bounded caches
  are implemented. Dedicated `-Suite flyouts` PASS11 gates: ordinary checks, nine host cycles, Intel
  GPU/reference pixels, and CPU/software + GPU/QSV with HELLO inflight1/2, each DSF1/2/1.25. Decoded
  app-menu pixels/alpha/close restoration and actual pointer selection creating a new tab all pass;
  unchanged100ms missing-layer/250ms appearance gates pass. New right-click context-menu gates
  also PASS all4 transport/credit variants at3 DSFs, including decoded alpha, exact close restoration
  and actual Reload selection rerunning page script (`prts-flyouts-context-all1-*`, Rust22 checks PASS).
  Select-popup gates also PASS all4 variants at3 DSFs, including decoded dropdown pixels, Escape
  restoration/no selection change and End/Enter committing blue option with decoded blue page
  (`prts-flyouts-select-all1-*`, Rust23 checks PASS). The one-command runner includes context/select
  variants. Other flyout types remain unqualified.
  Launcher disables Views UI GPU raster only to avoid cold shader stalls; page GPU/composition stay
  enabled. Full regression29 passed24/26 gates; two M5 resize failures came from Viz letterboxing
  an old root surface into new dimensions. Build30 rejects stale source sizes/content bounds;
  unchanged M5 GPU/CPU capture/input now PASS. All4 app-menu variants, production Intel auto GPU
  (decoded WebGL/video, QSV zero-copy) and v2 CPU WebRTC reruns PASS build30. Ineffective all-stages
  launcher comparison removed. Active uncommitted omnibox gates now PASS4 transport/credit variants
  at3DSFs: decoded appearance (CPU max244.32ms), exact close restoration, complete selected-row
  containment and real pointer navigation. Build40 has read-only native/WebUI geometry probes and
  first Aura poll as posted task (all routing/ACK/state checks retained). Software encoding uses
  existing packed-BGRA SIMD for even sizes and up to4 workers; odd/MF padding unchanged. Rust checks
  PASS; full38-gate regression40 and GN include check running before WIP checkpoint. Other flyouts
  remain next. Exact resume point:
  [mvp-flyouts-wip.md](mvp-flyouts-wip.md), latest journal-mvp.md. Source `work` remains5a7769bea8;
  current chrome binary is WIP. No full flyout signoff.
- **Next action: continue MVP (PLAN priority 2), M5b steps 4-5**: host-family tracking and GPU/CPU
  flyout composition/input (menus, context menus, omnibox dropdown, bubbles, tooltips, dialogs,
  permission/select/extension popups). Then step 6 per-window streams, touch input and remaining cleanup.
  Develop unfinished features on matching WIP branches; merge only after their gates pass.
- **MVP GPU/MF submilestone qualified (2026-10-21)**: production launcher in default auto capture mode
  reports active Intel vendor 32902/device 39877/LUID 0xe369, SwiftShader=0, with decoded WebGL/video and
  QSV DXGI zero-copy. MF H264 HRD corrected from two seconds to 100ms with a 32KiB IDR floor; mandatory
  settings readback and one-inflight timestamp contract. GPU encoder latency mean9.281ms/p95 10.737ms,
  CPU mean10.656ms/p95 11.862ms; each path180/180 pictures decoded immediately including bitrate restart.
  All19 full-suite gates plus production-auto GPU gate pass (20 total), strict MF20-cycle handles301 flat.
  One-command runner `powershell -File scripts/test.ps1 -Suite full` has per-gate deadlines/logs;
  disconnected-session override is explicit. README run steps corrected; v1 page compatibility remains
  the default pending M5b completion (`PRTS_BROWSER_PROTOCOL=2` opts into whole-window mode).
  See [mvp-mf-latency.md](mvp-mf-latency.md) and journal-mvp.md. Chromium source/patches unchanged.
- Isolation (I1) PAUSED and parked OFF main: infra branch `wip/i1-isolation` (d045f0a, incl. its exported
  patches) and Chromium branch `wip-i1` (9d3afd6eae; original history at `backup-i1-pre-park` = 23ec43701b).
  Resume instructions + pending items (Mojo execute-guard fix, outer-job UI-limit root cause, per-tenant
  window station + cross-tenant UI probe, precise LPAC failure diagnosis/minimal caps, AC loopback fixture
  gap) are in journal-i1.md "Resume later". WIP state observed: job limits/kill-on-close tests pass;
  AppContainer + private desktop passed M4 QSV zero-copy on Intel (LUID e369), but the final AC smoke
  failed (Mojo execute guard); LPAC fails at startup (not yet diagnosed).
- main: M5 steps 1-3 (whole-window capture, Aura input, v2 protocol) + odd/fractional-size fix (the Intel DXGI
  MFT rejects dims = 2 mod 4, so QSV input is padded to 4-aligned with edge replication and the frontend crops
  via true-size metadata; MF cropping is honoured only for even sizes). All 18 qualify-m5a gates pass on main
  (C:/src/chromium/prts-park-qualified-*). Chromium `work` = 5a7769bea8 = patches 0001-0023 (unchanged).
- Open gaps: connected-desktop foreground/physical cursor unverified (disconnected session), Windows 10
  22H2 qualification, QSV low-cap overshoot fallback, isolation (paused).

### Older state (superseded by the above)
- Phase 2 M5 DESIGN completed (source validation only): whole-window root capture,
  separate popup-host GPU/CPU composition, Aura/Views input and logical focus,
  per-window virtual displays/DSF, proposed v2 window/tab/source protocol and
  per-window tracks. See phase2-m5-design.md and journal-m5.md for exact hooks,
  investigation mapping, ordered implementation steps and acceptance gates.
  Implementation, builds and popup/input/decoded-pixel qualification remain next.
- Phase 0 DONE (protocol.md, chromium-hooks.md, isolation.md). Phase 1 DONE: prtsd (Rust, synthetic source,
  OpenH264 + Intel QSV via Media Foundation HW MFT with software fallback, webrtc, packet shaping) + Node
  server/frontend; real-browser E2E (`cd server; npm run e2e`) passes. Toolchains installed: Rust stable MSVC, Node LTS 24.
- Dev machine GPU: Intel UHD Graphics 630 (QSV verified there).
- Phase 2 M1 implemented and built: browser bootstrap, bounded pipe transport/handshake, target registry,
  Rust browser launcher and real-browser acceptance pass. Target HWNDs are hidden/DWM-cloaked, with no
  Chromium Explorer taskbar entry. Foreground preservation is UNVERIFIED because Flysoft session 2 is
  disconnected and GetForegroundWindow=NULL. See phase2-m1.md for exact checks, build times and commits.
- Phase 2 M2 implemented and built: validated/serialized RWH mouse, wheel, key, committed text and focus
  input; OOPIF hit testing; root-selected built-in cursor events. Real browser acceptance at DPR 1/2,
  hostile input validation, held-state blur release, navigation/close races, full M1 requalification,
  Rust tests/Clippy/fmt all pass. See phase2-m2.md. Custom cursor capability remains unadvertised.
- Patches 0001-0009 exported to patches/ and committed (infra f91806c M1, 751bb9b M2).
- Phase 2 M3 implemented; static background/resize stability fix qualified with 10/10 consecutive uninstrumented M3 passes. Real Viz CPU fallback sections -> prtsd OpenH264/QSV CPU input -> WebRTC;
  browser target/control/input integration and frontend default Chromium source. Two-resource pool,
  static/PLI refresh, background capture, DPR2, startup cancellation and Job Object lifetime pass.
  See phase2-m3.md Stability fix. Chromium work commits ca0285ef29, 63b67b8bc4, 61f0533a79, a8cf4e1c42, 57381798f0; infra staged only.
- Phase 2/3 M4 fast path implemented and built: native BGRA GMB -> browser keyed
  NT export pool -> private daemon D3D11 BGRA -> GPU NV12 -> MF DXGI QSV samples;
  CPU sections and software staging remain available with actual path stats.
  **M4 signoff blocked:** strict three-cycle handle check finds +3 Intel driver
  registry watcher handles per hardware MFT creation, reproduced by the exact
  pre-M4 encoder too. Functional GPU/CPU/recovery/E2E/M1/M2 tests pass. See
  phase2-m4.md and journal-m4.md; infra staged only, Chromium work committed.
- Next action: fix/qualify the Intel MFT activation handle lifetime (do not relax
  the gate), then rerun opt-in hardware lifetime and browser_m4 acceptance.
  Physical device-removal and connected-desktop focus remain unqualified.
- Open gaps: Intel MFT handle lifetime/signoff, acquired-device-loss recovery (currently protocol restart), QSV overshoots at very low caps
  (<=150 kbps -> auto-fallback to software), Windows 10 22H2 qualification, AppContainer/LPAC security isolation untested at runtime.

## Log
- 2026-10-21 (Codex MVP GPU/MF): Qualified Intel hardware in production auto capture with active GPU
  info, decoded WebGL/video and QSV zero-copy assertions. Fixed H264 HRD byte units (100ms target,
  32KiB floor after unchanged 150kbps decode gate exposed tiny-buffer corruption), read back mandatory
  settings, reject delayed/reordered output, added CPU/GPU latency/IDR/restart gate. Full19 + GPU gate
  PASS; no acceptance thresholds relaxed. Added bounded one-command runner and accurate README;
  refreshed stale daemon notes/UI label. Native failure/exception/timeout runner smoke gives7/1/124.
  Tests retain hidden/cloaked/taskbar checks; connected-desktop focus/cursor and Windows10 remain
  unverified. No Chromium edits/rebuild; export unchanged23 patches. Flyouts/per-window/touch still next.
- 2026-10-21 (orchestrator run 5, 20:04-00:55): Codex i1 (isolation) ran 20:07-21:41 and was killed by the
  5h usage limit (window 17% -> 100%, ~502k tokens). Codex oddsize (20:07-20:38) kept odd sizes on QSV; all
  gates pass. The user then paused isolation, so i1b was not resumed. Codex park (00:36-00:50) moved isolation
  to wip/i1-isolation + Chromium wip-i1, reset work to 5a7769bea8, committed the odd-size fix on main (9737575);
  full qualify-m5a 18/18 PASS, cr export unchanged. Pushed main + wip/i1-isolation. Usage: i1+oddsize consumed
  one full 5h window (~80 points); park was small.
- 2026-10-20 (Codex m5d): Reviewed the full chromium-output-investigation repo
  (7476579f2ce0) and validated hooks against local PIN + patches 0001-0019
  (work HEAD 7ea7adb8afc7). Added phase2-m5-design.md, proposed v2 protocol
  override and superseded the old page-surface decision in chromium-hooks.md.
  Chose separate root-host popup captures composed in prtsd before NV12/QSV,
  with preserved alpha/CPU equivalence and bounded caches/credits. Found native
  cursor side effects and creation-order headless hit tests requiring small
  opt-in hooks. Existing HeadlessScreenManager supplies window DSF; existing
  headless file chooser already cancels. Docs-only checks; no Chromium edits,
  probes, builds, commits or runtime pass claims. M4's later activation-fix
  qualification in phase2-m4.md is the reuse baseline; the older M4 blocker
  headline above predates it. Next: implement the ordered M5 gates, beginning
  with v2 registry/codecs and root capture/virtual display acceptance.
- 2026-10-20 (orchestrator run 2, 04:12-05:20): Verified 5h window reset (0% used) before starting. Ran Codex m1 (~32 min, incremental build 38.8 s final) and m2 (~25 min); both exit 0, acceptance passed, cr export -> patches 0001-0009, committed f91806c and 751bb9b. Desktop invisibility: target HWNDs hidden + DWMWA_CLOAKED=1, auxiliary HWNDs hidden, no taskbar entry; foreground preservation unverified (disconnected session). Stopped before M3: Codex usage after m2 = 5h window 79% (m1 ~40 points, m2 ~39), weekly ~20%+; M3+M4 would exceed the window, so per pacing rules next orchestrator starts after 09:13 reset. Decision: budget ~1 milestone per 40% of a 5h window.
- 2026-10-20 (orchestrator run 1): Ran Codex jobs p0-protocol, p0-hooks, p0-isolation (parallel), p1-server + p1-prtsd (parallel), p1-e2e, p1-qsv. All exited 0. QSV job: native QSV 28 fps / 13.9 ms encode at 720p; software at 150 kbps cap 22 fps; latency/loss shaping verified in E2E. Codex usage after run: 5h window 86% (resets ~2026-10-20 04:06 local), weekly 13%. Next orchestrator should wait for the 5h reset before starting Phase 2.
- 2026-10-19: Phase 1 part C passed real release prtsd + Node + headless Edge 155 browser acceptance. Added server/test/e2e.js and npm run e2e: live H264, decoded pointer/click/text pixels from real frontend input, target create/switch/resize/close, two instances/three viewers, receiver and encoder UI stats, bitrate cap reduction (latest run 571680 to 135652 bps at a 50 kbps target), clean instance/server/exact-daemon-PID teardown. Fixed other-operator instance discovery and spurious normal-close UI errors; added IPC supervisor shutdown for Windows. Release build, four Rust tests, seven Node tests and browser E2E pass. Saved phase1-e2e.png and updated root README/component notes. No Chromium build or git commit; optional latency/loss shaping remains unsupported.
- 2026-10-19: Phase 1 part B implemented server/ Node control plane and vanilla browser viewer, framed stdio, scoped WebSocket signaling, instance/tab controls, input, network controls and receiver/encoder stats. Installed Node LTS 24.20.0; seven mock/unit tests pass without Rust or Chromium. Added server-notes.md; no protocol changes, Chromium build, prtsd edits or commit. Real media/input/shaping integration remains to be verified with the concurrent prtsd implementation.
- 2026-10-19: Added isolation.md after inspecting Chromium 158.0.8111.0 Windows sandbox/GPU policy and Microsoft API references. Covers job/AC/LPAC profiles, ACLs, handles, network enforcement and hardware-GPU acceptance gates. Outer AppContainer, nested jobs and Windows 10 hardware rendering remain untested runtime gates. Documentation checks and all 18 source-path checks passed. No Chromium build, source edits or commit; session usage metrics unavailable.
- 2026-10-19: plan written. Codex CLI 0.163.0 verified with `-m gpt-6.1-sol`.
- 2026-10-19: Added chromium-hooks.md after validating local Chromium 158.0.8111.0 at caef9db47e2cf3fe8e9f07ddceeff6ddf6957385 (matches PIN). Recommended ClientFrameSinkVideoCapturer with kPreferMappableSharedImage, DXGI NT handle duplication from browser into prtsd, keyed mutex and retained frame leases; documented Windows native-handle preference restriction and hidden HWND/DComp behavior. Read scripts/cr.ps1. Documentation only; no Chromium build, source edits, or commits.
- 2026-10-19: Phase 1 part A implemented prtsd/ Rust synthetic daemon (webrtc/rtc 0.21.1, OpenH264 0.9.9, official windows 0.62.2). Release build, fmt, Clippy with warnings denied, and four standalone integration tests pass: real decoded RTP/video with visible DataChannel input, PLI, stats, target lifecycle/resize, trickle ICE, renegotiation/restart, malformed connection and EOF cleanup. Added README and prtsd-notes.md. Protocol adds optional fps and encode_ms_avg, and clarifies synthetic input indicator. Bitrate caps work; QSV/Chromium/packet delay-loss shaping remain unsupported. No Chromium build, server edits or commits in this task.
- 2026-10-20: Phase 1 part D implemented Intel hardware MF H.264 (UHD Graphics 630 verified), async events, CPU NV12, low latency/CBR/no B frames/GOP/IDR, auto/qsv/software selection and logged software fallback. Intel live bitrate property is ignored: MFT restart achieves native 1 Mbps (999998 bps); native 150 kbps overshoots ~440120 bps and measured guard triggers fallback. Software browser at 150 kbps: 120872 bps, 22.83 encoded/22.44 decoded FPS, 12.69 ms encode; QSV baseline: 3.08 Mbps, 28.30/28.14 FPS, 13.92 ms; requested QSV at 150 kbps falls back to software: 137883 bps, 20.35/19.23 FPS. Fixed capture-gap IDR storms, RTP time across skipped frames, software live bitrate updates, and synthetic held-button indication. Added encrypted UDP packet shaper with seeded loss/delay, 4 MiB oldest-drop queue, settings/close flush and separate drop stats. Browser software and qsv suites pass including delay/loss, input/tabs/fanout/recovery/cleanup; 400 ms delay raises jitter and 15% loss raises receiver packetsLost. Release build, Rust tests, two opt-in hardware tests, Clippy/fmt and seven Node unit tests pass. See prtsd-notes.md for measurements and driver limitations. D3D11 zero-copy, native QSV 150 kbps, absent-GPU/device-removal and Windows 10 runtime qualification remain open. No Chromium build or git commit.
- 2026-10-20: Phase 2 M1 implemented in seven small Chromium work commits (1d38a8c3f4 through cfd4dc5266): headless browser-only inherited-handle bootstrap, bounded/cancellable pipe I/O, HELLO/PING/SHUTDOWN/FAULT, stable target registry with independent viewport/DPR emulation, lifecycle/popup events and process/profile keepalives. Incremental chrome build passed (final 38.8s); GN checks passed transport/core/chrome_dll. Official windows crate launcher and standalone browser_m1 acceptance added without changing synthetic video. Required and extended real-browser tests pass: page-measured sizes and DPR2, ordered target mutation/load events, zero-window recreation, popup metadata, partial/EOF/peer death, shutdown, malformed framing/IDs/version/bootstrap and stalled writer. Native target HWNDs observed invisible with DWMWA_CLOAKED=1; auxiliary HWNDs hidden; actual Explorer taskbar UIAutomation unchanged. Foreground remains UNVERIFIED: disconnected Flysoft session2 returns NULL; reconnect and rerun. Cold Windows component DLL policy loading took171.96s; warm startup0.50s, HELLO_ACK320ms with five-second deadline preserved. Infra changes staged for orchestrator, patches not exported. See phase2-m1.md and phase2-m1-rust.md; input/cursor and frames remain M2+.

- 2026-10-20: Phase 2 M2 completed in Chromium work commits e2d6537d45 and a3629d6fcc (required coauthor trailers). Added generic content input/ACK adapter and opt-in root DisplayCursor observer without a content-to-Chrome dependency; PRTS validates all input schemas, rejects stale/inactive generations with one nonfatal diagnostic per second, serializes renderer ACKs, clears held state and cancels stale asynchronous dispatch at stream/navigation boundaries. Rust typed input encoding and cursor decoding, bounded command bytes, browser_m2 acceptance added. Incremental chrome build and GN checks pass. Page-side JavaScript confirms main/OOPIF clicks, exact key sequences and single insertion, Unicode commit, wheel, focus and cursor at DPR 1/2 plus resize. Held blur releases, navigation/close races, 16 malformed input cases and stale-fault rate limiting pass. Full M1 required+extended acceptance passes again; HWNDs hidden/cloaked and actual taskbar unchanged. Full cargo tests (12 ordinary passed, two hardware ignored), Clippy and fmt pass. Foreground preservation still UNVERIFIED in disconnected Flysoft session 2. Infra changes staged only; no patch export or infra commit. See phase2-m2.md; proceed to M3 after connected-desktop qualification.

- 2026-10-20: Phase 2 M3 completed in Chromium work commits ca0285ef29, 63b67b8bc4, 61f0533a79 (required coauthor trailers). Added target Viz capture adapter with ARGB shared-memory CPU fallback, two reusable BGRA sections across generations, subscriptions/unsubscribe/static refresh, credits/registration/unregister/release, resize and sink retargeting. Rust FrameSource trait/browser resource import feeds existing software I420/QSV CPU NV12 and WebRTC; browser targets/input/cursor wired into the actor, async/cancellable startup and atomic kill-on-close Job Object lifetime. Frontend defaults to Chromium; synthetic tests explicitly select Synthetic. Final incremental build/release build/Clippy/fmt/Node tests pass. M3 smoke passes exact BGRA, two/background subscriptions, static refresh, DPR2, third-subscription limit, no-subscriber pause, max_inflight=1 and 10 FPS pacing (19 frames/2s). Fullscreen 960x640 capture: 71 frames/3s, browser-tree CPU 157.2% of one core; slow consumer holds <=2 descriptors/sections and constant 1045240 KiB peak job commit. Explicit real static-page PLI -> fresh decoded IDR 95.77ms. Software and native Intel QSV real-browser E2E pass decoded colors/animation/cross-site navigation/click/text/DPR2/two targets/two instances/stats/startup cancellation and launch failure. Synthetic E2E (63.89s), full Cargo tests and M1 required+extended/M2 pass. Shared desktop probe during actual capture finds hidden/cloaked target HWNDs, hidden auxiliaries and unchanged Explorer taskbar; foreground remains UNVERIFIED in disconnected session. See phase2-m3.md; infra staged only, no infra commit or patch export.

- 2026-10-20: Independent M3 verification: FAIL signoff. Full three-commit Chromium diff and staged Rust resource/lifecycle/source paths reviewed. Confirmed and staged a daemon shared-memory validation fix: SEC_RESERVE uncommitted views previously registered successfully; VirtualQuery now requires committed read-only mapped pages covering the layout. Regression demonstrated failure before fix and passes afterward. Rebuilt Rust release binaries; 15 ordinary Cargo tests, explicit real-browser static PLI (95.21ms), all-target Clippy/fmt, seven Node tests, software/QSV decoded-page E2E pass. M3 acceptance is intermittent: three failures across six uninstrumented runs, on static background subscription or DPR2 resize; final three runs pass/fail/pass. Two temporary diagnostic runs passed; diagnostics removed. No Chromium edits/build/commit, infra commit or patch export. Foreground remains unverified; see phase2-m3.md Verification and C:/src/chromium/prts-m3-review-*.log. No PRTS processes remain.

- 2026-10-20: M3 stability fix qualified. Root causes: Viz failed readbacks/invalidated capture versions skipped VideoCaptureOracle::CompleteCapture, leaving phantom pending frames that block passive refresh forever on static sources; daemon single latest CPU slot also let another target overwrite a background frame. Chromium work commits a8cf4e1c42 (complete every capture; passive-recovery regression added) and 57381798f0 (remember credit-blocked updates, refresh on resource/release transitions, gated VLOG). Daemon now coalesces per target, bounded by the two registered sections, purging unread copies on last-section retirement; regression fails with cross-target coalescing and passes fixed. Prescribed detached incremental chrome build and release all-binary Rust build exit 0. Unchanged standalone M3 acceptance passes 10/10 consecutive uninstrumented runs, each exit 0, including static background, DPR2, resource bounds and desktop checks. M1 required+extended and M2 pass; static browser PLI 93.70ms; software/QSV browser E2E pass (16.95s/14.04s; both QSV instances use Intel MFT); synthetic E2E passes (57.67s). Cargo 16 ordinary tests, Clippy all targets -D warnings, fmt check and seven Node tests pass. Viz regression source added but viz_unittests not built under chrome-only build rule. HWND invisibility/cloaking/taskbar unchanged; foreground UNVERIFIED in disconnected session. Logs C:/src/chromium/prts-m3-stability-*. Infra staged only, no export/sync/apply or infra commit. See phase2-m3.md Stability fix for each run and remaining M4 gates.

- 2026-10-20 (Codex M4): Implemented native Windows GMB capture, separate browser key-0/1 NT texture pool preserving M3 credits/passive refresh, matching Intel LUID 0xe369, private daemon GPU copies, D3D11 BGRA->NV12 video processor and MF DXGI surface/device manager input. Software uses explicitly reported staging; CPU negotiation and sticky pre-acquisition import recovery pass. Chromium commits 35d3603e10, 4dbc560ff6, 5d7b7d54d0, 966cc136f9, 7ea7adb8af; build8 passes. Release build, Rust/Node ordinary tests, Clippy/fmt, unchanged CPU M3, M1/M2, GPU software and QSV browser E2E, CPU browser E2E, synthetic E2E and two hardware functional tests pass. Three GPU capture/encode/teardown cycles succeed but settled process handles grow360/363/366. Focused diagnostics reproduce one Intel adapter Classes registry key/event/wait packet per MFT creation, including exact pre-M4 module; all shutdown variants succeed without fixing it. M4 is not signed off. Fixed4Mbps1080p QSV harness GPU29.87fps/13.87ms/18.36% pipelineCPU vs CPU29.85fps/22.74ms/53.08%; real daemon adaptive measurements and caveats in phase2-m4.md. Hidden/cloaked/taskbar checks pass; foreground UNVERIFIED disconnected session. No export/apply/sync or infra commit.
