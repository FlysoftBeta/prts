# Phase 2 M3: real Chromium CPU frames

Implemented on 2026-10-20, Chromium 158.0.8111.0, Windows x64. The usable
flow now runs real PRTS pages: Node server -> frontend -> Chromium instance ->
decoded WebRTC video -> mouse/text input -> second instance -> receiver/encoder
stats. The frontend defaults to **Chromium (CPU fallback)**; Synthetic remains
selectable and its tests do not need Chromium.

## Design and files

Chromium adds `chrome/browser/prts/prts_capture.{h,cc}` and includes it through
`prts_sources.gni`. Small changes in `prts_target_registry.{h,cc}`,
`prts_controller.cc`, and `prts_pipe_client_win.cc` connect subscriptions,
resource responses/releases and the existing frame pipe. There are no new
content-layer hooks or changes to the M2 injection adapter.

`PrtsCapture` is the capture/resource adapter for M4 to extend. Each subscribed
target owns a `ClientFrameSinkVideoCapturer`, configured through the target's
RenderWidgetHostView with ARGB and default read-only shared memory. A retained
`IncrementCapturerCount(empty_size, stay_hidden=false, stay_awake=false,
is_activity=false)` lease enables hidden/background rendering without changing
the native headless HWND behavior. A 100 ms frame-sink check replaces the
capturer when navigation swaps the view/sink. Target replacement, viewport/DPR
changes, subscription and generation changes rebuild the adapter as needed.
No cursor overlay or desktop/DWM capture is used.

This is explicitly the **CPU readback fallback**, with these copies:

1. Viz shared-memory ARGB/ABGR -> reusable browser-owned BGRA section.
2. Read-only imported section -> daemon-owned latest BGRA frame.
3. CPU conversion -> I420 for OpenH264, or NV12 for Intel QSV's existing MFT.

Output is top-down tightly packed BGRA8, opaque alpha, with platform ARGB/ABGR
ordering normalized. Known-color acceptance checks exact bytes before encoding
and RGB pixels after WebRTC decoding. Capture dimensions follow CSS size times
DPR. Frame timestamps use Viz capture-begin time relative to HELLO_ACK, rather
than machine uptime. The daemon's current latency metrics start when it copies
the section for encoder ingress; they exclude browser readback and earlier pipe
queue time. The separate static-PLI measurement includes capture through decode.

The instance has at most two section resources, including retired generations.
A single subscription can pipeline two frames; two subscriptions share one
credit each. Requested max_inflight=1 is honored. A third subscription returns
resource_exhausted. Registration uses even browser request IDs; prtsd duplicates
the source section handle from its retained browser process handle, maps it
read-only, validates metadata/layout and replies OK before publication. No
pixels or handles appear in per-frame messages. CPU reads finish before release;
encoding uses the private copy. Old resources unregister only after releases,
and remain alive until unregister OK. Unacknowledged resource requests expire
after five seconds. Slow consumers exhaust credits and skip capture delivery;
they cannot grow a pixel queue. The daemon keeps one latest unencoded frame per
target (at most two, bounded by registered sections) and the existing single
latest encoded publication. Slow viewers hold neither pool.

`FRAME_CONFIG` mode 2 subscribes, mode 0 with zero FPS/credits unsubscribes;
`FRAME_REFRESH` (0x0322, target -> OK) requests a full/static refresh. Configuration
advances the target generation and resets old input state. FPS is a ceiling:
1..120 on the browser wire, 1..60 in current instance options, default 30.
No subscriber means no capture. Joining viewers and RTCP PLI/FIR requests force
refresh through the browser source and request an encoder keyframe. The protocol
changes are documented in sections 2.5 and 3 of `protocol.md` on both sides.

Rust adds `frame_source.rs`, `browser_frames.rs`, `bin/browser_m3.rs`, and a shared
`bin/support/desktop.rs` probe extracted from M1 without changing its checks.
`video.rs` consumes the new FrameSource trait through the existing encoder and
WebRTC publication path. `browser.rs` handles section registration, bounded
frame ingress and resource ACKs on the existing independent pipe readers.
`daemon.rs` maps browser lifecycle/target operations into the control plane and
forwards validated DataChannel input through M2, preserving CSS coordinates,
generation and a per-instance wire sequence. Title/load updates preserve held
input; owner loss forwards focus=false. Cursor events come from Chromium.

Startup runs outside the actor and returns starting immediately. The trusted
`PRTS_CHROME_PATH` environment configures the executable; viewer requests cannot
choose executables or arbitrary switches. Close/EOF/failure cancels startup.
The launcher assigns a kill-on-close Job Object atomically using the CreateProcess
job-list attribute and resumes the initial suspended thread afterward. This
closes the parent-death gap before a later AssignProcessToJobObject call.
It is process-tree lifetime containment, not AppContainer/LPAC security isolation.
Failed instances retain their diagnostic ID while workers, browser and imported
resources are released.

Node/frontend changes are limited to the source default/label and test scripts.
`server/test/e2e-browser.js` adds real-page acceptance; the existing synthetic E2E
explicitly selects Synthetic. The old Rust assertion that Chromium is unsupported
now tests an unknown source instead. `tests/end_to_end.rs` also adds an opt-in
real static-page PLI test; ordinary tests remain Chromium-independent.

## Running

```powershell
cd C:\Users\Flysoft\Desktop\chromium
cargo build --manifest-path prtsd/Cargo.toml --release --bins
$env:PRTS_CHROME_PATH='C:/src/chromium/src/out/Default/chrome.exe' # optional default
node server
```

Open `http://127.0.0.1:8080`, enter a page URL, and Create instance. Build the
patched Chromium first, using only the prescribed incremental command:

```powershell
cd C:\src\chromium\src
$env:PATH='C:\src\depot_tools;'+$env:PATH
$env:DEPOT_TOOLS_WIN_TOOLCHAIN='0'
autoninja -C out\Default chrome
```

Acceptance commands:

```powershell
cd C:\Users\Flysoft\Desktop\chromium
# Only in a disconnected session; foreground remains UNVERIFIED:
$env:PRTS_M1_ALLOW_DISCONNECTED_DESKTOP='1'
./prtsd/target/release/browser_m3.exe
./prtsd/target/release/browser_m1.exe
./prtsd/target/release/browser_m2.exe
cargo test --manifest-path prtsd/Cargo.toml
cargo test --manifest-path prtsd/Cargo.toml --test end_to_end browser_static_pli_requests_fresh_capture -- --ignored --nocapture
cargo clippy --manifest-path prtsd/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path prtsd/Cargo.toml -- --check
cd server
npm test
npm run e2e
npm run e2e:browser
$env:PRTS_TEST_ENCODER='qsv'
npm run e2e:browser
```

`PRTS_BROWSER` selects the installed headless Edge/Chrome **viewer**, independently
of `PRTS_CHROME_PATH`. No browser is downloaded by the tests.

## Acceptance and measurements

All required checks passed, with foreground preservation explicitly UNVERIFIED:

| Check | Result |
| --- | --- |
| Detached incremental `autoninja -C out/Default chrome` | Exit 0 on final source |
| Release Rust build, all binaries | Passed |
| M3 real capture smoke | Passed exact BGRA/stride/dimensions, two simultaneous subscriptions, background capture, static refresh, navigation, DPR2 resize, third-subscription rejection, no-subscriber pause |
| Slow consumer | Four 2 s pauses, at most two queued descriptors/two sections; requested one-credit limit also passed |
| Configured 10 FPS ceiling | 19 frames / 2.00 s |
| Real static-browser RTCP PLI | Passed: settled static source -> new captured/decoded IDR in 95.77 ms (one -> two IDRs) |
| Real browser WebRTC E2E, software | Both tests passed, 14.02 s total |
| Real browser WebRTC E2E, Intel QSV | Both tests passed, 12.72 s total; actual Intel MFT reported |
| Synthetic `npm run e2e` | Passed, 63.89 s, including fanout, bitrate/delay/loss and cleanup |
| M1 required + extended / M2 binaries | Both exit 0; malformed/bootstrap/input/EOF/peer-death/held-state regressions pass |
| Cargo | 14 ordinary tests passed; two hardware tests remain opt-in. New browser PLI test is opt-in and was run explicitly |
| Clippy all targets with `-D warnings`, rustfmt check | Passed |
| Node unit tests | All seven passed |
| `git diff --check` | Clean |

The WebRTC suites use a local page served over HTTP and an installed headless
Edge viewer. They inspect decoded canvas pixels, animation changes, real button
and text reactions, cross-site navigation (127.0.0.1 <-> localhost), CSS-coordinate
scaling/input at DPR 1 and 2, target create/switch/resize/close, static resubscription,
two instances, receiver/encoder UI stats, startup cancellation with no late ready,
asynchronous launch failure with retained failed ID, and exact daemon/server exit.
The fixture is real browser DOM content, not the synthetic renderer.
[Acceptance screenshot](phase2-m3-e2e.png) shows both Chromium instances and QSV.

Observed one-second samples at 640x480, two instances, nominal 30 FPS ceiling:

| Encoder | Instance | Encoded FPS | encode_ms_avg | ingress-to-encode ms |
| --- | --- | ---: | ---: | ---: |
| OpenH264 | i1 | 19.74 | 4.14 | 4.78 |
| OpenH264 | i2 | 14.84 | 4.60 | 5.14 |
| Intel Quick Sync MFT | i1 | 26.84 | 5.01 | 5.67 |
| Intel Quick Sync MFT | i2 | 25.73 | 4.27 | 4.81 |

These are interval samples during the acceptance flow, not steady-state capacity
benchmarks. Scene damage, startup, scheduling and active viewers affect FPS.
The direct 960x640 fullscreen animation capture delivered **71 frames / 3.00 s**.
Its browser Job Object accounted **4.719 CPU seconds / 3.002 s wall**, or **157.2%
of one CPU core**, including renderer/GPU-process CPU and readback, excluding
prtsd encoding. This component release+dcheck build's CPU fallback is expensive.
Across the four slow-consumer pauses, browser-tree peak job committed memory
stayed exactly **1,045,240 KiB** and imported section count stayed **2**. This is
job committed-memory accounting, not GPU VRAM or a process RSS measurement.

The shared M1/M3 desktop probe was run during actual M3 capture: first frame,
two subscribed targets, DPR2 resize, and after unsubscribe. Target HWNDs had
IsWindowVisible=false and DWMWA_CLOAKED=1; auxiliary HWNDs were hidden. Every
checkpoint reported zero visible/uncloaked and zero visible taskbar-eligible
PRTS windows. Actual Explorer UIAutomation task-list buttons stayed identical.
The probe samples foreground every 10 ms, but GetForegroundWindow is NULL in
this disconnected session. Explicit PRTS_M1_ALLOW_DISCONNECTED_DESKTOP=1 leaves
foreground preservation **UNVERIFIED**, not passed. Reconnect and rerun M1/M3
without the override for that gate.

Logs are outside the repos under `C:/src/chromium/prts-m3-`: `build.log`,
`rust-build.log`, `smoke.log`, `m1.log`, `m2.log`, `e2e.log`, `qsv-e2e.log`,
`synthetic-e2e.log`, plus their `.exit` files. The PLI command/output is recorded
in the agent run and reproduced by the explicit command above. No running PRTS
browser or daemon remains after acceptance.

Chromium `work` commits (all with the required coauthor trailer):

- `ca0285ef29`: bounded Viz CPU readback fallback adapter.
- `63b67b8bc4`: subscriptions, shared-section credits and frame-pipe routing.
- `61f0533a79`: requested per-source credits and connection-relative timestamps.

All Chromium builds were detached incremental builds with depot_tools prepended
and DEPOT_TOOLS_WIN_TOOLCHAIN=0. No clean, out/Default deletion, fresh gn gen,
cr apply/sync/export or build-output commit occurred. One early relink overlapped
a smoke run and failed on the loaded chrome.dll; rebuilding after the test exited
passed. Initial compile corrected the native pixel-format enum. An animation
fixture's data-URL percent escapes and a test WebSocket's missing Origin header
were corrected; final acceptance passes without manual intervention. Infra
changes are staged only, for the orchestrator to commit/export later.

## Deviations and M4 handoff

- This path deliberately copies CPU pixels and converts color on CPU; it is not
  the zero-copy target. Extend the capture/resource backend and FrameSource for
  D3D11 shared resources, adapter matching, synchronization, GPU conversion and
  direct encoder import. Do not advertise hardware texture import from M3.
- M3 uses a 100 ms sink watcher rather than another content observer hook.
  Navigation and WebContents replacement retarget capture without changing wire
  target identity. Cross-site navigation is checked through decoded page colors.
- The section pool and descriptors are bounded, but this is a short acceptance
  test, not a production multi-hour memory/driver soak. CPU readback/copy runs on
  the browser UI sequence and is expensive at larger resolutions. High-rate and
  multi-instance capacity need qualification; configured FPS is a ceiling.
- Capture-to-encode/send metrics exclude browser readback and viewer display;
  no unmeasured full interaction latency is claimed. QSV still uses CPU NV12 and
  retains the previously documented low-bitrate driver fallback.
- HWNDs are hidden/cloaked rather than absent. Foreground preservation remains
  UNVERIFIED in this disconnected desktop session and requires a connected rerun.
  Windows 10 22H2, AppContainer/LPAC security and GPU device-loss qualification
  remain later gates.

## Verification

This historical review is retained below. The subsequent **Stability fix**
section records resolution of its capture-stability gate and final qualification.

Independent skeptical review on 2026-10-20: **FAIL for unconditional M3
signoff**. Real Chromium pixels and input through WebRTC are verified, but the
standalone M3 acceptance intermittently stalls on static background capture or
resize. Successful reruns do not clear that failure.

Reviewed the complete Chromium range `a3629d6fcc..61f0533a79` (the three M3
commits listed above), staged daemon/resource/source/video/lifecycle changes,
and browser acceptance assertions. Capture/resource state is serialized on the
browser UI sequence; sections survive until release/unregister ACK. Both sides
bound resource count to two; daemon transport queues have byte/message limits,
and CPU/encoded publications coalesce to one latest frame. Daemon frame reads
match registered target/generation/dimensions/format and validate stride/length
and damage bounds; this wire format has no browser-supplied pixel offset.
Subscription removal destroys the capturer and its visibility lease. Source,
frontend and documentation explicitly identify CPU readback as fallback.

One confirmed daemon defect was fixed in staged `prtsd/src/browser_frames.rs`:
`MapViewOfFile` success alone did not establish that the declared pixel range
was committed/readable. A real `SEC_RESERVE` section was accepted by the old
code; the regression failed before the fix without attempting a faulting read.
Registration now uses `VirtualQuery` to require a contiguous committed,
read-only `MEM_MAPPED` data view covering the declared length, also rejecting
image views. Rejection drops the mapping and duplicated handle through RAII.
The regression now passes; descriptor identity/format/extreme-dimension
rejections were also exercised before allocation or copying. No Chromium
source change, Chromium build, Chromium commit, patch export or infra commit
was made; the infrastructure fix is staged for review.

Checks rerun against rebuilt release Rust binaries after the fix:

- `npm run e2e:browser`: both tests passed with software (15.90 s) and QSV
  (12.75 s); both QSV instances reported the Intel hardware MFT. The assertions
  draw the received HTML video into a canvas and inspect fixture colors,
  animation, real DOM click/text reactions, navigation, target switching and
  DPR2 scaling. This is decoded page content, not the synthetic renderer.
- `cargo test`: 15 ordinary tests passed; three opt-in tests remain ignored in
  that command. The real static-browser PLI test was run separately and passed:
  fresh captured/decoded IDR in 95.21 ms. The two hardware-only Rust tests were
  not separately rerun. Release all-binary build, all-target Clippy with
  `-D warnings`, rustfmt check, seven Node unit tests and diff whitespace checks
  passed. One intermediate Cargo relink hit LNK1104 while the earlier rate
  test executable was still running; the final full suite passed after it exited.
- `browser_m3.exe`: initial run passed all assertions. Subsequent runs include
  15-second timeouts waiting for target 2 generation 2 at 800x600, and target 2
  generation 3 at 960x640 after static background resize. Other complete runs
  passed, including two runs with temporary frame diagnostics (removed again).
  This is an unresolved intermittent acceptance failure, not a verified fix
  or a proven attribution to either capture or daemon coalescing. Across six
  uninstrumented runs, three passed and three failed; the final three returned
  pass/fail/pass. Raw logs:
  `C:/src/chromium/prts-m3-review-smoke*.log`, `prts-m3-review-final-*.log`,
  `prts-m3-review-diagnostic*.log`, `prts-m3-review-e2e.log` and
  `prts-m3-review-qsv.log`.

Remaining M4 gates: resolve the static capture/resize stall before treating M3
as reliably complete; qualify long-running memory/handle behavior and high
resolution load. Short successful slow-consumer runs retained at most two
sections/descriptors with stable peak job commit, but are not a leak soak.
Foreground preservation remains UNVERIFIED in this disconnected desktop
(`PRTS_M1_ALLOW_DISCONNECTED_DESKTOP=1`); hidden/cloaked HWND and taskbar checks
passed. D3D11 synchronization/device loss, direct encoder import, AppContainer/
LPAC and Windows 10 qualification remain outstanding. This CPU path remains
expensive and its ingress latency metrics exclude browser readback.

## Stability fix

The follow-up investigation found two independent lost-frame paths; neither
requires raising a timeout or retrying the acceptance test:

1. **Viz failed-capture accounting.** `MaybeDeliverFrame()` only called
   `VideoCaptureOracle::CompleteCapture()` for successful readbacks with a current
   capture version. A failed ARGB readback (observed as capture result 3 in the
   startup trace), or a capture invalidated by a target change, therefore left
   `num_frames_pending_` nonzero. Passive refresh events deliberately refuse to
   sample while any capture is pending. An immediate `RequestRefreshFrame()`
   following section registration can be FPS-limited and fall back to this
   passive timer; with a phantom pending capture, it never recovers without new
   compositor damage. Static background pages and resize transitions expose
   this; animated E2E pages usually supply the damage that hides it. Complete
   every recorded capture exactly once, passing success=false for failures.
2. **Cross-target CPU coalescing.** Diagnostics recorded target 2's 800x600 frame
   reaching prtsd, then being overwritten by target 1 before `poll_frame()`
   returned. Keep the latest copy per target, with at most two pending copies
   bounded by the registered two-section pool. New frames replace only their
   own target; unregistering the last matching section clears an unread copy.
   The encoded publication remains a single latest frame.

The PRTS adapter also remembers updates dropped during section registration or
credit exhaustion and requests a refresh on release/resource ACK. Only blocked
sources are refreshed, replacing the previous unconditional all-source refresh
and duplicate per-registration request. It does not continuously refresh an idle
static page. Visibility leases, frame-sink targeting, native HWND handling and
acceptance timeouts/fixtures are unchanged. Temporary tracing and daemon
per-frame diagnostics were removed. Optional browser logging includes PRTS
resource transitions at VLOG(1), and frame/credit details at VLOG(2).

Evidence before the Chromium fix: the diagnostic baseline failed on background
target 2 generation 2; fixing only daemon coalescing still failed on run 6 of six
runs, with the background section registered/ACKed but no subsequent capture.
The daemon regression reproduces the old cross-target replacement failure and
passes with per-target coalescing. A Viz regression was added for an empty copy
result followed by passive recovery without damage. The prescribed build target
is chrome; viz_unittests is not built here, so that new C++ test is not claimed as
executed.

Final validation and commit IDs follow.
Logs are under `C:/src/chromium/prts-m3-stability-*`; the original capture trace
is preserved as `prts-m3-stability-prefixed-startup.trace.json`, and the separate
capture-stall reproduction is `prts-m3-stability-isolate-6{,.capture}.log`.

Final Chromium `work` commits (required coauthor trailers present):

- `a8cf4e1c42`: complete failed Viz captures and add passive-recovery regression.
- `57381798f0`: wake capture on credit/resource availability; gated diagnostics.

The detached incremental `autoninja -C out/Default chrome` and release Rust
all-binary builds both exited 0. No clean, fresh GN generation, patch export or
infra commit was performed. The following ten runs used the unchanged release
`browser_m3.exe`, with PRTS_BROWSER_LOG and temporary diagnostics unset:

| Run | Result | Wall time |
| --- | --- | --- |
| 1 | PASS (exit 0) | 35.62 s |
| 2 | PASS (exit 0) | 19.66 s |
| 3 | PASS (exit 0) | 20.72 s |
| 4 | PASS (exit 0) | 20.92 s |
| 5 | PASS (exit 0) | 19.61 s |
| 6 | PASS (exit 0) | 20.81 s |
| 7 | PASS (exit 0) | 20.65 s |
| 8 | PASS (exit 0) | 19.65 s |
| 9 | PASS (exit 0) | 20.76 s |
| 10 | PASS (exit 0) | 19.6 s |

Each run checked exact BGRA, static background capture, explicit static refresh,
DPR2 resize, navigation, two-section/credit bounds under slow consumers, requested
max_inflight=1, FPS pacing, no-subscriber pause, and desktop invisibility.
Raw results: `prts-m3-stability-final-results.log`; per-run output:
`prts-m3-stability-final-1.log` through `prts-m3-stability-final-10.log`.

| Additional final check | Result |
| --- | --- |
| M1 including extended malformed/bootstrap/backpressure cases | PASS, exit 0 |
| M2 acceptance | PASS, exit 0 |
| Static-browser PLI -> fresh captured/decoded IDR | PASS, 93.70 ms |
| Browser E2E, software | 2 passed, 16.95 s |
| Browser E2E, QSV | 2 passed, 14.04 s; both instances used Intel Quick Sync MFT |
| Synthetic npm run e2e | PASS, 57.67 s |
| Cargo test | 16 ordinary tests passed; 3 opt-in tests ignored by default; static PLI test executed separately |
| Clippy all targets with -D warnings | PASS |
| cargo fmt --check | PASS |
| Node unit tests | 7 passed |

Two hardware-only Rust tests were not rerun separately. All M3 desktop
checkpoints and M1 found target HWNDs hidden/cloaked, no visible taskbar-eligible
PRTS windows, and unchanged Explorer taskbar buttons. Foreground preservation
remains **UNVERIFIED** with PRTS_M1_ALLOW_DISCONNECTED_DESKTOP=1. Long-duration
soak, GPU zero-copy, isolation and connected-desktop qualification remain M4 gates.
