# Phase 2 M5a implementation: design steps 1–3

Implemented on Chromium `work`, based on `7ea7adb8af`, with v1 still the
default. Steps 1–3 passed final qualification on 2026-10-20. M5a covers one
opaque browser root and Aura input. Popup composition
and simultaneous window publication remain steps 4–7.

## Files and behavior

Chromium paths are relative to `C:/src/chromium/src`:

| Files | Change |
| --- | --- |
| `chrome/browser/prts/prts_protocol.h`, `prts_controller.*`, `prts_pipe_client_win.*` | Bootstrap HELLO/ACK uses v1 envelopes; negotiated v2 then requires exact v2 envelopes. Independent v2 commands/capabilities preserve legacy v1. |
| `chrome/browser/prts/prts_window_registry.*`, `prts_target_registry.*`, `prts_sources.gni` | Browser/tab discovery, shared monotonic Window/Tab/Source IDs, tab movement/discard identity, membership, generations, accepted asynchronous close/cancellation and zero-window keepalive. Registry owns independent synthetic displays and one base source per window. |
| `chrome/browser/prts/prts_capture.*` | Root FrameSinkId plus capture lock captures Chrome controls and page. Existing M4 keyed D3D11 texture/CPU section registration, credits, retirement, refresh and fallback are reused. |
| `ui/aura/headless_root_size.h`, `window_tree_host.cc`, `BUILD.gn` | Optional owned logical-root extent survives fractional display notifications; compositor allocation is ceil(DIP × DSF). |
| `chrome/browser/prts/prts_window_input.*` | Window DIP pointer/key/text routing through Aura, logical focus and capture, precise wheel remainders, held-state reset, bounded queues, cancellation on owner/navigation/close and window cursor reporting. |
| `content/browser/renderer_host/aura_input_barrier.cc`, `render_widget_host_view_event_handler.cc`, `content/public/browser/aura_input_barrier.h`, associated BUILD files | Observe normal Aura routing and renderer acknowledgments; settle focus/text state before subsequent commands without duplicate renderer injection. Cancel obsolete epochs and handle Views-consumed/destruction outcomes. |
| `ui/views/widget/desktop_aura/desktop_headless_input.h`, `desktop_window_tree_host_win.cc`, `desktop_drag_drop_client_win.cc`, `ui/views/win/hwnd_message_handler.cc`, `ui/views/BUILD.gn`, `ui/base/ui_base_features.cc` | Pre-Widget opt-in native cursor suppression/counters; suppress raw input, native drag/move loops, pointer lock and system keyboard hooks. Preserve logical cursor shape through visibility changes. |
| `chrome/browser/external_protocol/external_protocol_handler.cc` | Prevent external native application launch at final launch boundaries for PRTS. Printing is disabled by PRTS policy. |

Infra implementation: `prtsd/src/window_wire.rs`, `browser_wire.rs`,
`browser.rs`, `lib.rs`, `bin/browser_m5.rs`, `bin/support/m5_capture.rs`,
`bin/support/m5_input.rs`, `bin/support/desktop.rs`, and `server/test/e2e-window.js`. The daemon's opt-in
`PRTS_BROWSER_PROTOCOL=2` projects one selected window into the existing M4
Node/media API; source ownership/generation is validated against window scenes.
Logical input blur does not clear video selection. Screenshot:
`phase2-m5-e2e.png` (supplementary; decoded pixel assertions are the authority).

Resume fixes: `prtsd/src/bin/browser_m1.rs`, `browser_m3.rs`, and
`bin/support/m5_input.rs`, plus `scripts/qualify-m5a.ps1`. Infra WIP `20343fc`
remains in history; all resume
changes are staged on top, without an infra commit.

## Clean Chromium commits

| Commit | Logical change |
| --- | --- |
| `3e8bd3d1e3` | Isolated v2 negotiation and stable window/tab registry. |
| `7c700545f0` | Root capture, independent virtual displays, measured client bounds, fractional DSF and opt-in Views geometry diagnostics. |
| `6c3a6eea60` | Native desktop side-effect suppression and final external-launch guards. |
| `5a7769bea8` | Aura input, observed renderer completion, logical cursor, paint-holding readiness and lifecycle/race fixes. |

These replace the original nineteen commits `4b6fd6a0a0..fa49d7a0e8`.
Original milestone trees were used; final tree equality was checked before
updating `work`. Backup branch `backup/prts-m5a-fa49d7a0e8` retains the original
chain. Every new commit ends with the required Claude Opus 5.5 coauthor trailer.
No export/apply/sync, clean, GN regeneration or args change was run. Existing
infra patches still reference the old chain and require a later orchestrator
re-export; they were intentionally left untouched here.

## M1/M3 final-run failures: cause and confirmation

Original logs are preserved as `C:/src/chromium/prts-m5a-final-*.log`.

- M1's negative version-negotiation probe offered only **2.0**, expecting an
  unsupported ERROR. M5 now supports that version and correctly returned ACK.
  The probe now offers unsupported **3.0**. The focused rerun passed the
  correlated unsupported ERROR, EOF, malformed-record, deadline and shutdown
  checks: `prts-m5a2-diagnose-m1.log`, exit 0.
- M3 is a CPU BGRA acceptance test, but the previous runner left
  `PRTS_FRAME_MODE=gpu` set after M5. GPU frames carry a texture and an empty CPU
  pixel vector, so its pixel index panicked. The corrected runner selects CPU
  for M3. M3 also rejects a non-CPU negotiation with an actionable error before
  indexing pixels. Full focused CPU capture/pixel/resize/pacing/credit/resource
  acceptance passed: `prts-m5a2-diagnose-m3.log`, exit 0.
- Both were deterministic harness assumptions; no Chromium v1 production fix
  was required. M4 recovery finished in the old detached run: its `.exit` and
  journal record are 0; its log confirms disabled-GPU negotiation, mismatched
  adapter, import-rejection CPU pixels and clean shutdown.
- The old CPU E2E runner also retained `PRTS_TEST_ENCODER=qsv`; the new runner
  explicitly covers GPU/QSV and CPU/software, for both v1 and v2.

M5 reruns additionally exposed intermittent `child:null` fixture-readiness
timeouts after successful DSF1/2 functional input. Both unchanged diagnostic
repeats passed, but a subsequent unlogged run failed again. A standalone local
Windows probe (`C:/src/chromium/prts-m5a2-socket-probe.rs/.log`) established
that accepted sockets inherit a nonblocking listener's mode: a read returned
WSAEWOULDBLOCK 10035 immediately despite a read timeout, whereas explicitly
blocking the accepted stream read the delayed request successfully. Full-driver
HTTP diagnostics in `prts-m5a2-fixture1/2/3.log` confirmed repeated immediate
10035 errors in the actual browser driver; failing repeats 2/3 had no child
request after the affected navigation. The fixture now explicitly makes
each accepted stream blocking before its bounded full-header read. Read/write
errors are diagnosed, and optional `PRTS_M5_FIXTURE_LOG=1` logs request lines.
All input/pixel/readiness assertions remain unchanged; no sleep or retry was
inserted between input commands. Focused post-fix GPU and CPU gates both passed
with ordinary logging (`prts-m5a2-fixture-fixed-gpu/cpu.log`, exit 0), without
10035 errors. Complete post-fix qualification is recorded below.

## Acceptance and final regression evidence

Reproduce from infra with Windows PowerShell:
`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/qualify-m5a.ps1`.
The runner sets the disconnected-desktop override and runs gates sequentially,
with journal entries before and after each. Logs and exit files use prefix
`C:/src/chromium/prts-m5a2-verified-` (final full sequence, 17 gate exit files).
Earlier complete runs use `prts-m5a2-final-` / `prts-m5a2-qualified-`;
unchanged GPU confirmations use `prts-m5a2-repeat-gpu1/2.log`.

| Gate | Required observations | Result |
| --- | --- | --- |
| Incremental chrome after squash | `autoninja -C out/Default chrome`, unchanged tree | PASS, `prts-m5a2-build.log`, exit 0; already up-to-date. |
| Final quick incremental chrome | Repeat after complete qualification, with no browser test processes alive | PASS, `prts-m5a2-build-final.log`, exit 0; already up-to-date. |
| Rust release, cargo test, Clippy, fmt, Node units | Release bins; ordinary Rust tests; `clippy --all-targets -- -D warnings`; formatting; Node units | PASS, `release`, `cargo-test` (18 ordinary passed, 5 opt-in ignored), `clippy`, `fmt`, `node-unit` (7/7). |
| M5 step 1, GPU and CPU | Exact negotiation/envelope isolation; two Browsers/five tabs; wrong-kind, cross-owner, unknown/stale IDs and malformed records; real move/discard preserves tab IDs; zero-window recreation | PASS in both `m5-gpu` and `m5-cpu`, including full resource drain before shutdown. |
| M5 step 2, GPU and CPU | Decoded tab strip/omnibox/toolbar/page ROIs; 960×640 DIP at DSF 1/2/1.25 and 961×641 at 1.25; actual logical/compositor sizes, DPR and smaller page viewport; generation/resource drain; static refresh | PASS in both `m5-gpu` and `m5-cpu`; standard GPU sizes use QSV, odd 1202x802 uses reported software recovery. |
| M5 step 3, GPU and CPU | Real tab clicks plus metadata/decoded colors; omnibox typing/Enter and Ctrl+L/T/W; no-sleep main/OOPIF exact Unicode insertion, Backspace/Enter; wheel sign and fractional precision at DSF 1/2; UI/page cursor and exit; capture outside video; held-key/pointer blur; owner switch; navigation/close races; native cursor writes 0 | PASS at DSF1/2 in both `m5-gpu` and `m5-cpu`, including races and native cursor writes0. |
| Legacy M1/M2/M3 | v1 control, input, CPU pixels and transport/resource acceptance | PASS, `m1`, `m2`, `m3`; M3 explicitly CPU. M1/M3 also passed focused confirmations. |
| M4 and recovery | GPU capture/QSV decode; unchanged strict three-cycle handles; disabled GPU, adapter mismatch, rejected import to exact CPU pixels | PASS, `m4`, `m4-recovery`; handles359/357/358, 30/30 decoded QSV frames per cycle (8.37/8.41/8.43ms). |
| MF hardware regressions | Dynamic bitrate/IDR/decode, low cap fallback, strict 20-cycle handle gate with unchanged +2 tolerance | PASS 3/3, `mf-hardware`, serialized; all20 handle samples266; 90/90 and150/150 decoded; native1Mbps999998bps; low cap software155057bps. |
| v1 browser E2E, GPU/QSV and CPU/software | Real Edge WebRTC decode, page/input/targets, two instances, backend stats and cleanup | PASS 2/2 tests in each mode, `v1-e2e-gpu` / `v1-e2e-cpu`; actual GPU texture/zero-copy and CPU section/software stats asserted. |
| v2 window E2E, GPU/QSV and CPU/software | Decoded Chrome UI/page, WebGL, two animated HTML video colors, actual backend stats, video after logical blur | PASS 1/1 test in each mode, `v2-e2e-gpu` / `v2-e2e-cpu`; decoded root/UI/WebGL/animated video and blur-publication checks passed. |

The complete post-fix sequence finished at 20:00 on 2026-10-20: all 17 gate
exit files are 0 and `prts-m5a2-verified.done` contains `PASS`. Final quick
incremental compilation passed at 20:00; Chromium remains clean with tree
`1c188c132a6e884952b8bd53181b42a45eedafcf`, identical to the pre-squash tree.
No browser/daemon/acceptance processes remained after qualification.

## Deviations and remaining work

See also `phase2-m5-design.md`, Implementation deviations.

- Capabilities remain partial: discovery/root-window capture are enabled;
  popup/alpha composition is not advertised. v1 remains available/default.
- The Windows host setter takes outer bounds while its getter reports client
  bounds. Bounded measured-inset correction obtains the requested root size.
  An owned Aura root-size property prevents fractional notifications from
  expanding logical layout. The odd case is 961×641 DIP / 1202×802 pixels;
  renderer CSS viewport may enclose fractional pixels by one CSS pixel.
- Intel QSV may accept the first 1202×802 frame and fail a later encode with
  E_FAIL. Existing any-frame software encoder recovery is exercised, retaining
  GPU capture with staging/software encoding. Standard sizes qualify QSV;
  odd fractional recovery is never described as zero-copy encoding.
- Input completion additionally releases post-navigation paint holding before
  explicit remote input. Motion does not wait for an unnecessary visual-state
  callback. Fixture readiness uses complete HTTP headers/graceful shutdown,
  unique navigation identity and a 127.0.0.2 child site for genuine OOPIF routing.
- Display management and routing live in registry/input files rather than
  separate design-proposed manager files. Popup-aware hit testing, capture
  routing and virtual stacking still require steps 4–5.
- In this disconnected session, foreground is NULL and GetCursorInfo returns
  ACCESS_DENIED. Physical focus/cursor observation remains **UNVERIFIED**.
  All-host hidden/cloaked/taskbar checks and native cursor-write counters are
  separate evidence; they do not substitute for connected-desktop qualification.

Step 4: host-family readiness/destruction, ownership/deduplication, immutable
scenes, fair bounded source pools and virtual stack/Screen queries. Step 5:
GPU/CPU popup alpha, masks/shadows and composition with decoded menu, bubble,
tooltip, select, permission, dialog and edge/nested-menu gates. Step 6: native
v2 Node/frontend API and independent per-window encoder/publication workers;
the temporary single-window projection must be replaced. Step 7: popup churn,
recovery/lifetime/stale-frame stress and connected-desktop physical focus/cursor
qualification before retiring v1. Acquired-device-loss recovery, other GPUs,
Windows 10 and isolation hardware qualification remain unqualified by M5a.
