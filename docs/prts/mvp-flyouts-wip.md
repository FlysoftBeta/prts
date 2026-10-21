# M5b flyouts WIP: resume here

Both repositories use `wip/mvp-flyouts`. Nothing from this unfinished feature
has merged to main. Qualified Chromium `work` stays `5a7769bea8` (23 baseline
patches). `out/Default/chrome.exe` is currently **WIP build40**, not main.

## Current checkpoint (2026-10-21)

### Revised-plan wrap-up (supersedes the older backlog below)

PLAN.md revised07:30 accepts the four working flyout types; do not extend to
other flyouts before the preview release. Current edits apply standard PRTS
BRANDING/branded strings, a small shared Windows UA/Client Hints hook, four
missing direct GN Views dependencies, and CPU/software report-only latency.
GPU/QSV still enforces250ms. Lessons are in lessons-flyouts.md. Touch and
independent extra-window tracks remain known gaps.

Bounded Default build: `C:/src/chromium/prts-mvp-wrap41.*` (1800s).
Quick suite: `prts-mvp-wrap41-quick-*` (1200s parent).
GN check: `prts-mvp-wrap41-gn.*` (300s).
New `server/test/e2e-identity.js` uses the production daemon/pipe launcher and
HTTP reports from page/dedicated/shared/service workers; full suite includes it.
Existing Release build is still running (siso PID10124, about38k/70k steps);
never start another Release ninja concurrently. Final incremental Release build
and clean-extract verification must precede publishing.

Next: inspect these exits; run fixed-binary identity and full acceptance matrix;
fix genuine failures; commit/export WIP then merge only qualified work to main,
update STATUS and push. Release follows. Historical notes below retain evidence.

### Active omnibox follow-on after select checkpoint1e9b856

Current incremental binary is build40. Chromium HEAD remains0f563c2575 with
uncommitted changes in `prts_window_registry.cc` (test-only native/WebUI row
geometry) and `content/browser/renderer_host/aura_input_barrier.cc` (first poll
posted without a timer; all pending-router/ACK/visual-state checks retained).
Infra adds `m5_omnibox.rs`, `m5_popup_fixture.rs`, software packed-BGRA SIMD
conversion for even sizes and up to4 available CPU encoder workers. Odd edge
replication and MF padded conversion stay unchanged. Rust37 checks PASS,
including a BT.601/chroma/SIMD-tail regression.

**Omnibox-all2 PASS4/4** CPU/software+GPU/QSV xHELLO1/2, each3DSFs, with decoded
appearance (CPU max244.32ms against unchanged250ms), exact close restoration,
full selected-row stream containment and real pointer navigation to decoded
blue page. The runner now includes all app/context/select/omnibox combinations
(23 dedicated-suite gates including ordinary/host/GPU-reference checks).

Exact next step: inspect bounded full38-gate regression currently running at
`C:/src/chromium/prts-flyouts-regression40-*` and GN include check
`C:/src/chromium/prts-flyouts-includes40-*`; fix failures without relaxing gates.
Then commit/export Chromium WIP, commit/push infra WIP, and continue dialogs,
permission/bubble/profile/extension popups, tooltip and nested-edge menus.

Probe invariants:0x42c native selected-row marker0x7fff0001;0x42b WebUI shadow-DOM
read-only geometry. Use non-creating `WebView::web_contents()`; GetWebContents()
creates/attaches empty content and corrupts hidden popup state. DOM geometry
retains fractional DIP while native bounds round; readiness permits <1DIP
rounding but returns the complete DOM row for exact driver containment. No
probe injects input or invokes navigation. Static loopback fixture avoids OOPIF
first-paint false positives. Use unique per-DSF second-query fragments because
history may autocomplete a prior URL, and retyping identical text does not
reopen suggestions. Latency visibility witnesses use a tighter120DIP region
below the omnibox and borrowed CPU pixels, retaining >1000 changed pixels and
the250ms deadline. Temporary logs/Ganesh comparison were removed; failed test
pixel dumps remain diagnostic-only outside the repo.
Owned-host scenes, fair two-resource capture, premultiplied CPU/GPU composition,
and actual app-menu pointer selection work. The dedicated one-command suite
passes all11 gates:

```powershell
powershell -File scripts/test.ps1 -Suite flyouts -AllowDisconnectedDesktop
```

The desktop override applies only to this disconnected session. Connected
foreground/physical-cursor qualification is still open. Existing gates verify
hidden/cloaked windows, taskbar exclusion and native cursor-write counters.

Evidence: `C:/src/chromium/prts-flyouts-qualified1-*` (build29). This covers
Rust release/tests/Clippy/fmt, Node unit tests, nine real host cycles, Intel
GPU/reference alpha/mask/shadow composition, and four decoded app-menu gates:
CPU/software and GPU/QSV, each HELLO inflight1/2, each DSF1/2/1.25. App menu
opens by real toolbar click; decoded appearance stays <=250ms, missing-layer
100ms diagnostics stay zero, alpha is preserved and close restores exact page
pixels. A second real menu click selects New tab and requires a new selected
tab before cleanup. This is **partial M5b qualification**, not all-flyout signoff.

Chromium source checkpoint `0f563c2575` is committed and exported as27 patches;
infra checkpoint `255ffcd` was pushed before that final resize correction.
Full regression `C:/src/chromium/prts-flyouts-regression29-*` completed24/26:

```powershell
powershell -File scripts/test.ps1 -Suite full -AllowDisconnectedDesktop
```

The two failures were M5 resize ROI gates: Viz fitted the old root surface into
new capture dimensions, producing black letterbox margins. Build30 rejects
root frames until `metadata.capture.source_size` and full `content_rect` match
the requested pixels. Unchanged M5 GPU/CPU capture/input gates PASS in
`prts-flyouts-resize20-*`; all4 app-menu variants, production Intel auto GPU
(decoded WebGL/video and QSV zero-copy) and v2 CPU WebRTC PASS in
`prts-flyouts-final30-*`. The ineffective all-stages launcher flag was removed.
All other broad gates passed regression29 (including M1/M2/M3/M4/recovery,
MF hardware/lifetime/latency, odd sizes and GPU/CPU/synthetic WebRTC).

Resize correction infra checkpoint `fa96c6c` is pushed. Context-menu gates now
also PASS CPU/software and GPU/QSV, HELLO1/2, all3 DSFs in
`C:/src/chromium/prts-flyouts-context-all1-*`; Rust22 checks PASS. They require
real page right-click, decoded pixels/alpha within250ms, zero missing-layer
diagnostics, exact close restoration, and pointer Reload executing page script.
The one-command runner includes these4 additional gates. Chromium source unchanged.

Context checkpoint `7dc05b4` is pushed. Select-popup gates now also PASS all4
CPU/GPU/credit variants at3 DSFs (`prts-flyouts-select-all1-*`, Rust23 checks
PASS): visible decoded dropdown below the closed control, exact Escape
restoration/no value change, then End/Enter selecting blue with decoded blue
page. The one-command runner includes context and select variants.

Exact next step: checkpoint/push select coverage on WIP, then implement
real omnibox gates. Main must wait for all required flyout,
input and lifecycle gates. Keep all acceptance thresholds unchanged.

## Implemented behavior

- `prts_host_tracker.*`: opt-in Views host/widget lifecycle tracking, stable
  source identities, owner ancestry/context/parent/bubble-anchor/transient
  resolution, ready geometry, transparency, parent sources and virtual order.
- `prts_window_registry.cc`: owned-layer snapshots and capture epochs,
  automatic visible-host subscriptions, v2 feature29. Test-only host probe0x42c
  includes menu bounds; modern ChroMenu New tab uses a block Button with a
  localized accessible name including its shortcut, rather than MenuItemView.
- `prts_capture.*`: preserved alpha format2; eager registration and fair
  scheduling recycle <=2 resources across sources. Actual one-credit HELLO
  progresses with static base and popup. Legacy v1 behavior remains covered.
- `scene_frames.rs`: private per-source caches (512MiB aggregate bound),
  generation/revision invalidation, owner-dirty output, popup-only updates,
  missing-layer diagnostics and exact restoration. Browser credits are returned
  after private copies, never retained by caches.
- `composition.rs`, `composition.hlsl`, `d3d_composition.rs`: premultiplied
  reference CPU and D3D11 ONE/INV_SRC_ALPHA composition, masks, shadows,
  clipping/scaling/opacity. Intel GPU/reference gate agrees within one byte;
  all-GPU composition performs no CPU readback.
- Popup-aware Aura routing already chooses captured/hit owned roots and weak
  dispatch destinations. Build29 fixes release flags: Aura mouse-up includes
  the changed button, while wire `buttons` retains its post-event held state.
  Without this, Views buttons see the correct press/release but reject clicks.

## Measured latency fix

The previous 100ms missing-layer failure was cold **browser UI GPU raster shader
work**, not resource registration or readback. Trace4 shows a copy waiting106ms
behind Graphite raster flush/Dawn compiler tasks; readback itself takes4ms.
Ganesh alone was intermittent. The production Rust headless launcher sets
`--disable-features=UiGpuRasterization`: Views tiles rasterize on CPU, while page
GPU raster, hardware compositing, WebGL and DXGI capture stay enabled. The
production hardware adapter gate must still pass. Unsuccessful eager startup
refresh retries and temporary low-level diagnostics were removed.

`PRTS_M5_TRACE=1` on the standalone flyout driver uses test-only request0x429
and saves `C:/src/chromium/prts-flyouts-copy.json`. Startup trace switches did
not produce a file. Historical logs and investigation are in journal-mvp.md.

## Placement already qualified (do not redo)

Build12/13 nine-cycle host tests passed toolbar-click and Alt+F/Escape at
960x640@1, 960x640@2 and961x641@1.25, physical containment and cross-owner
exclusion. Fixed headless client insets, physical monitor lookup, 64-bit
nearest-display distance (previously overflowed at48192px), independent size
scaling for DIPToScreenRect and menu border/shadow containment. Chromium source
regressions exist but unit-test binaries were not built under the chrome-only
incremental-build rule; runtime gates cover the regressions.

## Remaining work in priority order

1. Extend real decoded/input gates and fix required flyouts: omnibox suggestions,
   bubbles, tooltip,
   JavaScript dialogs, secure-origin permission prompts (no auto-accept flags),
   nested edge menus, extension/profile popups. Require GPU/QSV and CPU/software,
   alpha/ROI/close restoration, popup-only updates, three DSFs and no desktop UI.
2. Finish shared hit-test/restack policy, Aura reparent/stack observers,
   unknown-owner diagnostics, hover exits, held-state cancellation across popup
   capture/focus/destruction and dialog text acceptance. Exercise cache/credit
   bounds, slow viewers, repeated popup churn and lifetime/recovery gates.
3. M5 step6: independent per-window videos/tracks/encoders and frontend cards.
4. End-to-end multi-touch/tap/scroll/pinch and remaining codebase cleanup.
5. Only after MVP: oneVPL/NVENC, then paused isolation.

Never merge partial flyouts to main. Chromium commits need the required
coauthor trailer, then `./cr.cmd export wip/mvp-flyouts` before committing infra
patches. Keep the qualified source `work` ref unchanged. Don't commit build
outputs or trace artifacts.
