# MVP flyouts: qualification and resume

The revised PLAN accepts the four qualified popup families for the preview.
All native gates pass on Chromium `3337c44996`, now promoted to `work` and
exported as33 patches. Component build47 is qualified. The original native5s
shutdown/RPC deadlines pass after pipe cancellation/join uses BLOCK_SHUTDOWN;
both temporary daemon workarounds were removed.

Full acceptance prefix: `C:/src/chromium/prts-mvp-wrap47-resume-full`.
39/40 passed initially; synthetic receiver loss stayed0 despite shaper drops.
The unchanged gate passes `prts-mvp-wrap47-synthetic-repeat1.*`. No assertions
were relaxed. GPU/QSV popup appearance86.25–248.13ms; CPU/software69.55–262.33ms
(report-only). Worker identity fixture closes227ms, all11 descendants exited.

Next: commit/merge qualified wrap-up to main, push, then one incremental
Release build. The initial non-component build completed exit0 at13:13:30.
Use scripts/package-preview.ps1 and scripts/verify-preview.ps1; require the full
clean-extract suite before publishing the preview. Detailed state is STATUS.md.

The backlog below is historical. Follow PLAN priority order: release first,
then unified window/flyout model and minimal Chromium patch re-architecture,
then encoders/isolation and reintegration. Touch, other flyouts and independent
extra-window videos remain known gaps for this preview.
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
