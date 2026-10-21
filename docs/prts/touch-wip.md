# Touch milestone resume

Newest PLAN priority 1b supersedes unfinished E1. Both repositories are on
`wip/touch`, based on qualified main/work (`ef8dc29` / `c14260952f`).
E1 is safely checkpointed/pushed as `wip/e1-onevpl` commit `7c57d40`;
its matching Chromium branch has no additional source changes. Do not merge E1
before touch ships as v0.1.1-preview.

Implemented on WIP: wire TOUCH 0x208, phases and bounded contact IDs, daemon
validation, frontend real PointerEvent touch path/capture/cancellation, Aura
TouchEvent dispatch with original host routing and reset cancellation.
The frontend preserves held input across metadata updates for the same epoch.
Previously `setTarget` unconditionally sent focus=false for every title/load
refresh; release-stack diagnostics proved this cancelled multi-touch contacts.

Evidence outside repository: `C:/src/chromium/prts-touch-*`.
Final source `a898866032`, exported35 patches; component build5 and incremental
non-component Release build PASS. Ordinary Rust, Clippy/fmt, Node8 pass.
CPU8 passes app-menu/Newtab and core gestures. Matrix1 passes GPU/CPU DSF1.25
and GPU DSF2, including native held-contact cancellation on DataChannel close
and subsequent control recovery. Its initial DSF1 and CPU DSF2 failures are
retained: the test's readiness predicate used an old send-only channel
reference or considered video ready before the new control channel opened.
The test now captures the actual datachannel event and requires the new
channel open with a live target/video. Full1 is running with that correction
and an explicit post-release fling assertion, plus all original full gates.

`server/test/e2e-touch.js` drives the real frontend with CDP touch (no remote
page scripting/injection), receives fixture reports over HTTP and asserts
gestures. It also gates native app-menu/Newtab with decoded pixels and active
URL. Control-plane targets represent windows, so do not use their count to
infer native tab creation. Touch app-menu commands use a three-tile layout;
the first Newtab tile is at owner DIP670,90 for the960-wide fixture.
Earlier raster diagnostics are outside the repository. Temporary Chromium
diagnostics were removed before build5; touch capability64 is advertised.
Runner `scripts/test.ps1 -Suite touch` includes quick checks plus sequential
GPU/QSV and CPU/software touch gates at DSF1/1.25/2. Rust wire validation and
frontend held-state/metadata/contact-ID cancellation regressions pass.
Still required: finish full1, clean-extract gates and v0.1.1 publication.

Use newest journal lines for the exact next operation. Do not debug deferred
omnibox/latency failures. Keep incomplete code off main.
