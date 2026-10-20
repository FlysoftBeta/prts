# M5b flyouts WIP: resume here

Both repositories are on `wip/mvp-flyouts`. Nothing in this unfinished feature
has been merged into main. Main's completed GPU/MF milestone is `c445ddd`, with
checkpoint `e015ae6`; Chromium `work` remains `5a7769bea8` (23 baseline patches).
The current `out/Default/chrome.exe` was incrementally built from the flyout WIP,
not the qualified main source. No browser/daemon is left running.

## Implemented so far

- Generic opt-in Views observer in `desktop_headless_input.h` and
  `desktop_window_tree_host_win.cc`: copied creation metadata, explicit widget
  readiness, and StackAbove/StackAtTop notifications. No observer runs outside
  the existing headless input opt-in.
- `chrome/browser/prts/prts_host_tracker.{h,cc}`: weak root/widget/parent/context
  identities, widget and Env lifetime observation, visible owned host snapshots,
  logical source IDs, owner ancestry/context/parent/bubble anchor/transient
  resolution, actual transparency, parent source and virtual restack ordering.
- Registry installs/removes that observer and exposes test-only request `0x42c`
  -> reply `0x48c` (requires `--prts-test-hooks`). Probe includes logical IDs,
  widget client screen bounds, root screen bounds, host client pixels, owner
  client pixels, type, event policy and transparency.
- Rust `support/m5_hosts.rs`, included by `browser_m5`, runs with
  `PRTS_M5_HOSTS=1`. It opens the real app menu with Aura Alt+F, checks ownership,
  positive ready geometry, cross-window exclusion, closes with Aura Escape and
  repeats. The final gate also requires the menu to fit its owner's stream.
  Rust decoder accepts the exact new test reply type.
- `cr export [branch]` can export the WIP branch without moving the qualified
  Chromium `work` ref. Default export and sync still use `work`.

## Current failing gate and next action

Incremental chrome build4 PASSED, release `browser_m5` build PASSED,
Clippy all targets with warnings denied and fmt PASSED. Initial ownership-only
gate passed three real menu open/close cycles, cross-owner exclusion and desktop
hidden/cloaked/taskbar checks. That is **not** flyout signoff.

The mandatory placement assertion now FAILS reproducibly: the menu's widget,
Aura root and native host all report `[-32178,-68,390,608]`; its owner client
canvas is `[32768,0,960,640]`. This is actual host placement, not a difference
between the coordinate APIs. No compositor should silently clip this menu or
reinterpret the ownership-only pass as usability.

Next: trace the menu anchor/work-area/InitParams bounds before creation. Inspect
the headless screen position client, virtual display mapping and menu bounds
calculation. Correct placement relative to the owning virtual display, keeping
edge/nested menus usable, then make the unchanged containment gate pass. The
observed x difference is 65,536 pixels (twice owner origin); this is diagnostic
evidence, **not** justification for an ad-hoc coordinate offset.

Reproduce from repo root (no capture/input full gates implied):

```powershell
cargo build --manifest-path prtsd/Cargo.toml --release --bin browser_m5
$env:PRTS_M5_HOSTS='1'
# Only for this disconnected session; connected desktop qualification remains open:
$env:PRTS_M1_ALLOW_DISCONNECTED_DESKTOP='1'
./prtsd/target/release/browser_m5.exe
```

Run it with a 180-second external deadline, using the `Gate` function in
`scripts/test.ps1` as in journal-mvp.md. The failing output is
`C:/src/chromium/prts-mvp-flyouts-placement-m5-hosts.log`; error details are in
the adjacent `.err.log`. Earlier owner-only passes are `host2` / `host3` logs.
Build logs: `C:/src/chromium/prts-mvp-flyouts-build4-chrome.*` (exit0).

## Remaining implementation (do not relax these requirements)

The registry still emits a **base-only scene**. The daemon still explicitly
rejects auxiliary-layer scenes. No flyout pixels are captured/composed yet.
After placement is correct, follow phase2-m5-design.md steps4-5:

1. Complete owned-host scene snapshots, readiness/destruction reconciliation,
   unknown-owner diagnostics, parent/descendant ordering and input hit testing
   shared with virtual headless screen queries. Observe Aura reparent/stacking
   changes too; the current tracker primarily observes Widget events.
2. Extend capture with fair bounded source scheduling. Current `PrtsCapture`
   admits only two sources and keeps two source-specific resources; it cannot
   simply admit arbitrary popups without retirement/fairness changes. Preserve
   existing v1 gates and the one-credit progress requirement.
3. Preserve premultiplied alpha for layers. Current CPU publication forcibly
   writes alpha255 and resources validate opaque format1 only. Add negotiated
   layer format/alpha handling, masks/shadows and CPU/GPU equivalent composition
   before NV12. Keep caches bounded and compose each update from a fresh base.
4. Add decoded pixel gates for all required flyouts, popup-only updates, close
   restoration, latency, resize/DSF, nested menus, dialogs and permission prompts;
   require GPU/QSV plus CPU/software paths and desktop checks. Do not remove the
   daemon's auxiliary-layer rejection before composition actually exists.

The initial host gate uses Alt+F; final qualification must additionally open the
app menu by a real toolbar click located through stable Views geometry.
`ToolbarView::GetAppMenuControl()->GetFocusablePaneView()` supplies the button
view; include `app_menu_control.h` when adding the geometry probe.

Then implement M5 step6 per-window streams, touch input and remaining cleanup.
oneVPL/NVENC and paused isolation remain lower priorities. Never merge this WIP
into main until all its acceptance gates pass. Export its Chromium commits with
`./cr.cmd export wip/mvp-flyouts` before committing the infra WIP snapshot.
