# Phase 2 M2: input and cursor

Implemented on Chromium `work`, on top of the M1 commits, on 2026-10-20.
Infra changes are staged, not committed. No cr apply/sync/export, clean build,
output deletion, or manual GN generation was performed.

## Files and design

Chromium:

- `chrome/browser/prts/prts_input.{h,cc}`: complete positional input validation,
  synthetic mouse/key/character/focus construction, held-state tracking, and a
  per-target bounded dispatch queue. Invalid payloads produce nonfatal
  invalid_argument FAULT. Queue exhaustion produces fatal resource_exhausted;
  a five-second renderer/input synchronization deadline produces fatal timeout.
- `prts_controller.cc`, `prts_target_registry.{h,cc}`, `prts_sources.gni`, and
  `chrome/browser/BUILD.gn`: notification dispatch, per-connection increasing
  input sequences, active target/generation checks, and cursor event encoding.
  Stale input produces nonfatal stale_state FAULT at most once per second.
  Activation, resize, WebContents replacement and committed main-frame navigation
  clear held state and invalidate queued/pending asynchronous injections.
- `content/public/browser/synthetic_input.h` and
  `content/browser/renderer_host/synthetic_input.cc`: a narrow generic embedder
  adapter that keeps all content internals out of Chrome. Mouse/wheel events
  use the input router's GetRenderWidgetHostAtPointAsynchronously; target widget
  coordinates are supplied by its hit test, including OOPIF transforms. Mouse
  delivery explicitly updates the root CursorManager view-under-cursor state.
  Keys resolve GetFocusedRenderWidgetHost at dispatch time and always use
  skip_if_unhandled. Renderer ACKs serialize mouse, wheel and key transitions.
  Weak views/hosts and cancellable completion callbacks protect navigation/close.
- `content/public/browser/render_widget_host_cursor_observer.h` and a small
  addition in `render_widget_host_view_aura.cc`: opt-in UI-sequence callback list
  invoked by root DisplayCursor after CursorManager chooses the hovered view.
  There is no content-to-Chrome dependency. Both new headers live in a separate
  small `content/public/browser:synthetic_input` target; the common public
  browser module is not expanded. Content BUILD.gn registers the implementation.

Wire coordinates remain CSS pixels. The browser applies Chromium's browser/page
zoom scale to obtain widget DIPs, while device emulation performs the DPR
transformation in rendering/compositing. No additional DPR multiplication occurs
in input code. Wheel down/right deltas are negated once for Chromium, use precise
pixel granularity and begin/end phases, and wait for visual state before hit
checking. Synthetic screen positions are derived only from the headless view's
bounds, following InputHandler; no Windows desktop coordinate/input API is used.

KEY_DOWN is RawKeyDown with empty event text even if its informational wire text
is populated. A single UTF-16-unit KEY_CHAR produces one character event, retaining
keypress/input ordering. Longer committed Unicode/IME text uses one ImeCommitText
operation, with its completion serialized before subsequent input. Preedit is
not implemented. Blur synthesizes releases for held keys/buttons and changes
renderer page focus, without activating a native window.

Cursor wire shapes 0..7 map to default/pointer/text/crosshair/move/ew-resize/
ns-resize/none; built-ins have zero hotspot/dimensions and an empty PNG. The
custom cursor capability is deliberately **not advertised** (HELLO_ACK features
remains 1). Custom/unsupported shapes fall back to default; no bitmap is sent.

Rust:

- `prtsd/src/browser_wire.rs`: InputHeader, Input/Key encoders, Cursor decoding,
  strict lengths/enums/Unicode/PNG limits, and two hostile-input wire tests.
- `prtsd/src/browser.rs`: ordered input notifications, command queue byte budget
  (8 MiB in addition to the 1024-message cap), and custom-cursor feature checking.
  Callers supply an increasing sequence per owner through InputHeader.
- `prtsd/src/bin/browser_m2.rs`: real Chromium acceptance, two short-lived local
  HTTP servers, page-JavaScript reports read exclusively through TARGET_CHANGED,
  desktop snapshots, raw malformed-input checks and orderly teardown. The main
  page uses 127.0.0.1 and the iframe uses localhost on a different ephemeral port;
  test-only --site-per-process forces the cross-site iframe into an OOPIF.
  No script evaluation or external debugging endpoint is used.

## Validation actually run

- Incremental `autoninja -C out\Default chrome` passed with depot_tools prepended
  and DEPOT_TOOLS_WIN_TOOLCHAIN=0. All builds ran detached and were polled.
- GN header checks passed for chrome/browser:core, content/browser:browser and
  content/public/browser:synthetic_input. Chromium git diff --check passed.
- M2 page acceptance passed main-frame and OOPIF clicks at DPR 1 and DPR 2;
  resize 800x600@1 to 900x650@2; exact keydown/keypress/input/keyup sequences and
  `a` then `aa` values in both frames; down-text did not duplicate insertion.
  It also passed one committed `hello` plus emoji text insertion, wheel down/up
  at both DPRs, document.hasFocus transitions, and held key/button blur releases.
- Cursor default/pointer/text and OOPIF pointer selection passed at both DPRs.
  Cursor IDs and generations were checked by the driver.
- Stale resize generation, inactive target, invalid DOM code, navigation while
  input is in flight, and closing a target during input passed without a crash.
  Ping and orderly shutdown passed after the race cases.
- Raw browser validation passed NaN, truncation, trailing bytes, modifier bits,
  button mask/enum/click count/down-state errors, key-up text, invalid location,
  nonboolean repeat/system/focus, malformed UTF-8, NUL and oversized text. Every
  case returned nonfatal invalid_argument and left the connection usable. A
  burst of twenty stale notifications produced exactly one stale_state fault.
- M1 full required AND extended acceptance passed again, including independent
  targets/DPR, lifecycle ordering, popup discovery, peer death/EOF, all malformed
  framing/bootstrap cases and stalled event reader shutdown.
- Cargo build --bins, cargo fmt -- --check, cargo clippy --all-targets --
  -D warnings, and cargo test passed. There are 12 ordinary tests (including
  the two new wire tests); two opt-in hardware tests remain intentionally
  ignored. The software rate-control regression passed in about 85 seconds.

The initial runtime uncovered a real ordering issue: key notifications overtook
asynchronous mouse targeting, so the input element did not yet have focus. The
final implementation waits for renderer acknowledgements internally. The test
sends click/down/char/up without sleeps between input events. A short 300 ms
settling interval after initial page/DPR readiness permits compositor surfaces
to become available; result waits use page state, not guessed input timing.

Logs, outside both repositories:

- `C:/src/chromium/prts-m2-build.log` and `.exit`.
- `C:/src/chromium/prts-m2-acceptance.log`, `.exit`, and `prts-m2-chrome.log`.
- `C:/src/chromium/prts-m2-m1-acceptance.log`, `.exit`, and
  `prts-m2-m1-chrome.log`.
- `C:/src/chromium/prts-m2-cargo-test.log` and `.exit`.

## Desktop qualification and remaining scope

M2 enumerated browser-owned top-level windows after input/resize and teardown,
rejecting visible, uncloaked windows. M1 requalification used its existing
Notepad sentinel, 10 ms foreground sampler, HWND/style/DWM inspection and actual
Explorer UIAutomation taskbar snapshots: no visible/uncloaked browser window or
new Chromium taskbar button appeared.

Flysoft session 2 remains disconnected; console session 3 is connected and
GetForegroundWindow is NULL in this agent's session. Both acceptance binaries
used the explicit PRTS_M1_ALLOW_DISCONNECTED_DESKTOP=1 override. Foreground
preservation is **UNVERIFIED**, not a pass; reconnect session 2 and rerun both
binaries to complete that qualification. Production code uses no SendInput,
PostMessage, SetForegroundWindow or desktop input coordinates.

Custom cursor PNG export is an unadvertised optional capability, not implemented
in M2. Drag-and-drop, pointer lock, preedit composition, native dialogs/select
popups, and key release after a focused widget is independently destroyed still
need dedicated product policy/acceptance. Ordinary target navigation and closure
are covered. CPU/GPU frame production and server/frontend integration with this
browser input API remain M3 and later work; the synthetic media path is intact.

## Reproduction

From the infra root:

```powershell
& "$env:USERPROFILE/.cargo/bin/cargo.exe" build --manifest-path prtsd/Cargo.toml --bins
# Only in this disconnected session; omit on an interactive desktop:
$env:PRTS_M1_ALLOW_DISCONNECTED_DESKTOP='1'
& ./prtsd/target/debug/browser_m2.exe
& ./prtsd/target/debug/browser_m1.exe
```

Chromium commits on work (both with the required coauthor trailer):

- `e2d6537d45`: generic widget input/ACK and root selected-cursor adapters.
- `a3629d6fcc`: PRTS validation, serialized input, held state and cursor events.

Final incremental build exited 0; final M2 run completed at about 05:13 local,
including all 16 raw validation cases and the 20-record stale burst (exactly
one nonfatal diagnostic). Final M1 required+extended requalification and the
full Rust test run also exited 0. No chrome/browser_m1/browser_m2 processes
remained at the final check.
