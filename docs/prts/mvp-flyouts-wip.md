# M5b flyouts WIP: resume here

Both repositories use `wip/mvp-flyouts`. Nothing from this unfinished feature
has merged to main. Qualified Chromium `work` stays `5a7769bea8` (23 baseline
patches). `out/Default/chrome.exe` is currently **WIP build29**, not main.

## Current checkpoint (2026-10-21)

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

Chromium source checkpoint `7da2e4f41e` is committed and exported as26 patches;
infra checkpoint is pending. Full regression currently running with prefix
`C:/src/chromium/prts-flyouts-regression29-*`:

```powershell
powershell -File scripts/test.ps1 -Suite full -AllowDisconnectedDesktop
```

Regression29 already finds black decoded tab-strip ROI at1001x777@1.25 on both
existing M5 capture paths. M1/M2 and the dedicated flyout gates pass. Pending
launcher comparison adds `--run-all-compositor-stages-before-draw` so CPU UI
tiles finish before draw; this edit is not yet rebuilt/tested. Exact next step:
let the bounded suite finish and inspect exit/done files, then rebuild Rust and
rerun unchanged M5 CPU/GPU gates with that flag. Fix failures without relaxing
assertions. Rerun dedicated flyouts and production Intel GPU evidence after the
launcher change, then checkpoint/push WIP only. Main must wait for all required
flyout, input and lifecycle gates.

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

1. Extend real decoded/input gates and fix required flyouts: right-click context
   menu, select popup/change selection, omnibox suggestions, bubbles, tooltip,
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
