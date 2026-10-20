# Phase 2 M5a implementation (steps 1–3)

Work in progress. Final acceptance results will replace this status before handoff.
Browser checkout: C:/src/chromium/src, branch work. v1 remains the default.
No patch export/apply/sync or clean/GN-args change was performed.

## Implemented so far

- V2 bootstrap negotiation and strict post-ACK envelope version validation;
  exact-v2 daemon offers cannot silently select page-stream v1.
- Browser window registry, tab membership snapshots, one shared monotonic
  Window/Tab/Source ID allocator, independent window/source generations,
  asynchronous accepted-close semantics, zero-window keepalive reuse.
- Root compositor capture using root FrameSinkId and VideoCaptureLock. M4
  CPU sections/keyed D3D11 slots, registration, credits, retirement and recovery
  are reused. Independent synthetic headless displays provide window DSF;
  v2 never applies per-tab DevTools emulation.
- Scene codec and source ownership/generation validation, plus opt-in
  PRTS_BROWSER_PROTOCOL=2 single-selected-window daemon adapter through M4.
  Native v2 Node API and simultaneous window workers remain step 6.
- Pre-Widget native cursor suppression, raw-input/drag/move-loop rejection,
  printing policy and external application launch prevention.

## Evidence to date

- Step 1 browser_m5 registry gate passed: two Browsers/five tabs, wrong-kind,
  unknown/stale IDs, malformed records, v1 isolation, real tab move/discard
  identity preservation, zero-window recreation.
- capture6gpu/cpu passed full decoded Chrome tab-strip/omnibox/toolbar/page
  ROIs; 960x640@1/@2/@1.25, 961x641@1.25, compositor size/DSF, page DPR and
  smaller viewport, resize generation/resource drain, static refresh.
- Edge WebRTC window-e2e3 and e2e4 passed decoded full Chrome UI with actual
  gpu-texture/d3d11-zero-copy stats through release prtsd and Node.
- Hidden/cloaked HWNDs and unchanged taskbar passed. Foreground remains
  UNVERIFIED in the disconnected desktop. GetCursorInfo returns ACCESS_DENIED;
  physical cursor observation is UNVERIFIED. Native-write counters/guards are
  being qualified separately. No successful global cursor query is claimed.
- Rust library tests and all-targets compilation, release bins, Node 7/7 unit
  tests passed at intermediate checkpoints; final full checks are pending.

## Runtime deviations

See phase2-m5-design.md, Implementation deviations. Outer/client insets need
measured correction; fractional pixel enclosure needs an owned Aura logical-root
size property. Intel QSV can fail a later1202x802 encode, so M4's existing
software recovery is exercised and reported rather than claiming native QSV
for that odd fractional case. Popup discovery/composition remain steps 4–5.

## Source and commits

Chromium: chrome/browser/prts/prts_protocol.h, prts_pipe_client_win.*, 
prts_controller.*, prts_target_registry.*, prts_window_registry.*, prts_capture.*,
prts_sources.gni; ui/aura/headless_root_size.h and window_tree_host.cc;
ui/views/widget/desktop_aura/desktop_headless_input.h and Windows host/drag code;
ui/views/win/hwnd_message_handler.cc; ui/base/ui_base_features.cc;
chrome/browser/external_protocol/external_protocol_handler.cc.

Infra: prtsd/src/window_wire.rs, browser_wire.rs, browser.rs, lib.rs,
bin/browser_m5.rs, bin/support/m5_capture.rs and desktop.rs;
server/test/e2e-window.js. Screenshot: phase2-m5-e2e.png.

Logical milestones: 4b6fd6a0a0 (initial registry), d862c67d5b (root/display),
397badde8d (client insets), a4b2151964 (fractional logical root),
e3929cdd1d + 4c25c6b68f (native guards). Additional compilation/test fixes are
recorded in journal-m5.md and the work branch log. All Chromium commits include
the required coauthor trailer. Infra changes are staged only.
