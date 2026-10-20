# Phase 2/3 M4: D3D11 texture capture and QSV input

Implemented fast path. The Intel activation handle leak is fixed by creating the
enumerated hardware CLSID directly; the focused lifetime gate now passes 20
cycles with exactly 299 handles. See the handle-lifetime results below for the
cause, compatibility limits, and subsequent browser qualification.

## Design

Viz requests ARGB with `kPreferMappableSharedImage`. On Windows this delivers
BGRA8 native DXGI GMBs after GPU completion. Pixels are never mapped in this
path. The browser opens the source on the negotiated hardware D3D11 adapter,
acquires Viz's existing key-0 mutex, and copies the visible rectangle into a
PRTS-owned BGRA export texture with SHARED_NTHANDLE | SHARED_KEYEDMUTEX. It waits
for GPU completion, releases Viz at key 0, releases the export at key 1, then
ends the Viz lease. This separates Chromium's key-0 convention from wire keys
0/1 without changing upstream SharedImage synchronization.

The browser creates the NT handle; prtsd duplicates it from its retained browser
process handle (noninheritable, SAME_ACCESS). The older hook proposal's reverse
duplication is superseded. prtsd opens with OpenSharedResource1, checks the
negotiated LUID and actual descriptor/mutex, acquires key 1, copies into a private
GPU BGRA texture, waits for GPU reads, releases key 0 and ACKs. Coalescing or
stale-frame rejection occurs after this transition, so even dropped frames
return mutex ownership correctly. Encoder retention never holds browser credits.

The export pool preserves M3's global two-resource ceiling across subscriptions,
retirement, generation changes and outstanding registration/unregistration.
Two targets get one resource each. M3 credit-blocked refresh and Viz's failed
capture completion fixes are unchanged. Private daemon copies coalesce per
target, bounded by registrations, and retiring the last resource removes unread
copies. MF samples own their NV12 texture until the asynchronous MFT releases it.

The MF MTA attaches IMFDXGIDeviceManager before setting media types when the MFT
advertises D3D11 awareness. ID3D11VideoProcessor converts full-range BGRA to
BT.601 limited-range NV12; MFCreateDXGISurfaceBuffer supplies encoder samples.
The input/output views and GPU query enforce completion. Every sample has a
private surface, so no encoder-retained input is overwritten. `d3d11-zero-copy`
means no CPU pixel readback; GPU copies and conversion still occur.

OpenH264 texture input uses BGRA staging readback followed by CPU I420 conversion
and reports `staging`. CPU sections retain the M3 copy/conversion behavior.
Raw HRESULT checks preserve WAIT_TIMEOUT/WAIT_ABANDONED and S_FALSE semantics
that windows-rs Result wrappers otherwise erase.

## Negotiation and recovery

Set trusted daemon environment `PRTS_FRAME_MODE=auto|gpu|cpu` (default auto).
`gpu` is a preference with mandatory CPU recovery; acceptance requires actual
GPU stats explicitly. No new browser launch switch is needed. CPU offers no
LUID and only section mode. GPU negotiation probes a matching hardware device
and allocation of a real keyed NT texture, rejects known Chromium adapter
mismatches/SwiftShader, and rechecks actual GPU information at first capture.
Cold HELLO can precede Chromium GPU information; a zero/unknown reported LUID
is not treated as a known mismatch. Opening the native GMB on the matching
hardware device validates its resource adapter as well.

Observed actual capture: Intel vendor 32902 (8086), device 39877 (9bc5, UHD630),
LUID 58217 / 0x000000000000e369, SwiftShader=false, hardware=true. prtsd matches
that LUID. No WARP device is created. Other vendors are unqualified.

Unsupported GMB handles, failure to obtain a first native frame within five
seconds, export allocation failure, and daemon import/registration rejection
trigger one diagnostic and sticky CPU fallback for that target, with a new
generation/TARGET_CHANGED. Failed imports close partial objects before ERROR.
A rejected resource is never published. Registration failures caused by device
removal follow this recovery too. Failure after mutex acquisition or uncertain
GPU completion still invalidates the connection under the existing protocol;
never forge a release. Physical device removal has not been injected.

Actual stats persist across idle intervals:

- `frames.backend`: gpu-texture / cpu-section / synthetic.
- `encoder.input_path`: d3d11-zero-copy / staging / cpu-nv12 / cpu-i420.
- The actual encoder name remains authoritative after software fallback.

Protocol changes clarify implemented GPU copies and recoverable pre-acquisition
import errors; no new message types, version changes or mutex keys were added.

## Files and Chromium commits

Chromium `work` (all with the required coauthor trailer):

- `35d3603e10`: native GMB capture, D3D11 export pool, per-target CPU recovery.
- `4dbc560ff6`: pipe thread ownership transfer on UI before asynchronous join.
- `5d7b7d54d0`: hardware adapter negotiation gate.
- `966cc136f9`: handle cold-start GPU information and validate at first capture.
- `7ea7adb8af`: keyed texture allocation probe, native-start timeout, formatting.

New Chromium implementation is in PRTS files (`prts_d3d.{h,cc}` and capture /
controller / registry); upstream changes are two Windows link libraries in
chrome/browser/BUILD.gn. No Viz stability patches were changed. Shutdown testing
exposed an existing pipe-worker DCHECK: DetachFromSequence ran on a worker after
Start bound the thread owner to UI. Transferring ownership on UI fixes it.

Infra: d3d.rs, browser_frames.rs, browser.rs, frame_source.rs, mf_encoder.rs,
encoder.rs, video.rs, daemon.rs, lib.rs and Windows crate features; standalone
browser_m4, browser_m4_recovery, browser_m4_bench; hardware lifetime regression;
Node browser E2E actual-path assertions and optional process measurements;
protocol/hooks/component notes, this document, journal and STATUS. Staged only.

## Earlier qualification commands and results (before activation fix)

All commands use release binaries for browser/encoder acceptance. Logs are in
`C:/src/chromium/prts-m4-*`; the append-only journal records individual runs.
Set `PRTS_M1_ALLOW_DISCONNECTED_DESKTOP=1` in this disconnected session.

- Detached prescribed `autoninja -C out/Default chrome`: build8 passed, after
  earlier GN/style/undeclared-variable build failures were corrected. No clean,
  args changes, export/apply/sync, overlapping builds or test-time relink.
- `cargo build --release --bins`: passed; final cleanup rebuild recorded below.
- `cargo test` and `cargo test --release`: 16 ordinary tests passed. Existing
  hardware/static-browser cases are opt-in. New lifetime regression is opt-in
  and intentionally retains the known failure, not skipped from signoff.
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`: passed.
- `cd server; npm test`: seven passed.
- `PRTS_FRAME_MODE=gpu browser_m4`: all three cycles complete exact imported
  BGRA colors, 30/30 decoded QSV surface samples, two targets, background/static
  refresh, resize/DPR2, pacing, one-credit limit, <=2 resources/descriptors,
  bounded memory, unsubscribe/unregister and clean shutdown. Final process
  handle assertion FAILS: settled counts 360/363/366. QSV sample averages
  8.41/8.27/8.30 ms including test-only decode overhead.
- `PRTS_FRAME_MODE=gpu browser_m4_recovery`: passed real registration rejection
  → CPU exact pixels in generation3, disabled-GPU CPU negotiation and shutdown.
  The final version also probes a nonexistent adapter LUID (see final results).
- `PRTS_FRAME_MODE=cpu browser_m3`: unchanged M3 passed, 21.00 s.
- `browser_m1` required + extended: passed, 36.18 s; `browser_m2`: 7.85 s.
- `PRTS_FRAME_MODE=auto npm run e2e:browser`: software/staging passed 14.65 s;
  QSV/gpu-texture/d3d11-zero-copy passed 13.82 s. Assertions check actual stats
  as well as decoded page colors, animation, click/text, cross-site navigation,
  DPR2, target switching, two instances, startup failure/cancellation and cleanup.
- `PRTS_FRAME_MODE=cpu PRTS_TEST_ENCODER=qsv npm run e2e:browser`: passed 13.51 s.
- `npm run e2e`: synthetic passed 55.78 s.
- Existing two opt-in hardware functional regressions passed: 90/90 decoded;
  native 1 Mbps =999998 bps; low-cap software recovery decoded150/150. Existing
  Intel native <=150kbps rate-control limitation remains.

HWNDs remain hidden/cloaked, auxiliaries hidden, Explorer taskbar unchanged.
Foreground is UNVERIFIED because GetForegroundWindow is NULL in the disconnected
session. Reconnect and rerun without the override for focus qualification.

## Measurements (2026-10-20, percent of one CPU core)

Fixed 4 Mbps QSV capture+encode harness: same CSS animation, 30 FPS subscription,
two-second warmup and eight-second samples, sequential GPU/CPU runs. Browser
CPU uses Job Object accounting including exited descendants. Daemon-pipeline CPU
is the standalone harness process using the actual browser ingress/MF code,
without WebRTC/control-plane overhead. This is the comparable QSV comparison.

| Frame path | Device size | Browser tree CPU | Pipeline CPU | FPS | Convert/encode ms |
| --- | --- | ---: | ---: | ---: | ---: |
| GPU / DXGI QSV | 960x640 | 134.87% | 21.67% | 29.86 | 8.959 |
| CPU / NV12 QSV | 960x640 | 125.57% | 16.79% | 29.87 | 7.719 |
| GPU / DXGI QSV | 1920x1080 | 109.17% | 18.36% | 29.87 | 13.874 |
| CPU / NV12 QSV | 1920x1080 | 125.28% | 53.08% | 29.85 | 22.735 |

GPU costs more at the smaller size in this harness; at 1080p it reduces pipeline
CPU and conversion/encode time. These are host-specific observations, not
latency guarantees. Current browser completion waits run on UI and can stall it
for the bounded driver wait. An asynchronous GPU export worker is future work.

Actual release prtsd + Node + WebRTC + Edge viewer, same real animated page,
2.5-second warmup and eight ~1.05-second intervals per size; process CPU from
Windows process accounting, excluding the viewer and Node. Run with
`PRTS_M4_BENCH=1 PRTS_TEST_ENCODER=qsv npm run e2e:browser`, once per frame mode.
These include adaptive receiver feedback and are explicitly not fixed-bitrate.

| Mode / actual encoder input | Size | Browser CPU | prtsd CPU | Encoded FPS | Encode ms |
| --- | --- | ---: | ---: | ---: | ---: |
| GPU / d3d11-zero-copy | 960x640 | 102.42% | 16.32% | 24.40 | 8.645 |
| CPU / cpu-nv12 | 960x640 | 113.37% | 21.15% | 24.25 | 8.376 |
| GPU / d3d11-zero-copy | 1920x1080 | 46.05% | 16.90% | 23.25 | 14.496 |
| CPU / software cpu-i420 | 1920x1080 | 64.98% | 57.55% | 20.17 | 25.478 |

The CPU1080p run lowered its target to332110 bps via REMB; Intel emitted1074000
bps and the existing measured guard switched to software. Its CPU figures are
not a native-QSV comparison. Both complete measurement E2Es passed (~40/42 s).

## Handle lifetime: root cause and fix (2026-10-20)

The earlier +3-handle result was real and also reproduced with the exact pre-M4
encoder. NtQuery handle enumeration identified one registry key under the Intel
adapter's `Control/Class/{4d36e968-e325-11ce-bfc1-08002be10318}/0000/Classes`, an
unnamed event and an unnamed WaitCompletionPacket per activation. Raw activation
without media types, frames or a DXGI manager still grows through 20 cycles:
316 to 371 (three per cycle with two unrelated handles closing late). Reusing
the same IMFActivate while calling ShutdownObject each cycle does not bound it.
MFStartup-only, DXGI-manager-only and MFTEnumEx-only were flat. Same-thread COM,
IMFShutdown, flush/end-streaming, manager detach and balanced MF shutdown had
already failed to fix it; these hypotheses were not repeated.

CDB breakpoints on NtNotifyChangeKey identify the owner precisely:

```text
mfplat!CMFTransformActivate::InstantiateTransform
  combase!CoRegisterDeviceCatalog
    combase!DeviceCatalogs::AddCatalog
      combase!DeviceCatalog::StartChangeNotification
        combase!wil::registry_watcher_t<...>::create_common
          KERNELBASE!RegNotifyChangeKeyValue
            ntdll!NtNotifyChangeKey
```

The watcher is created by **combase.dll**, requested by **mfplat.dll**, before
the Intel transform is configured. A second CDB experiment observed matching
CoRegisterDeviceCatalog/CoRevokeDeviceCatalog calls (20 of each) on raw activation,
yet handles continued growing. This isolates a runtime device-catalog watcher
lifetime defect on this host; it is not evidence that an Intel DLL directly
opens the leaked key. Tested versions: mfplat.dll 10.0.26100.9278 and combase.dll
10.0.26100.8875. The Intel MFT is mfx_mft_h264ve_64.dll from the installed
iigd_dch driver package. Logs: `C:/src/chromium/prts-m4leak-watch2.log` and
`prts-m4leak-catalog.log`; earlier NtQuery type/name logs are `prts-m4-mf-handles-*`.
The later `prts-m4leak-names.log` confirms the associated event is unnamed and
auto-reset and the WaitCompletionPackets have no names.

The fix retains hardware-only MFTEnumEx enumeration and Intel preference, reads
MFT_TRANSFORM_CLSID_Attribute, and uses CoCreateInstance(CLSCTX_INPROC_SERVER)
instead of IMFActivate::ActivateObject. The CLSID is discovered, not hardcoded
(on this host: 4BE8D3C0-0515-4A37-AD55-E4BAE19AF471). This bypasses the leaking
device-catalog registration path. No activation object is activated, so no
ShutdownObject call is needed. Transform flush/end-streaming/IMFShutdown and
COM/MF balance remain. Low latency, CBR, baseline, zero B pictures, IDR, GPU
manager/samples and bitrate restart behavior remain intact.

No pool, process-lifetime retained encoders, warmup allowance or gate relaxation
is required. If the enumerated CLSID cannot be created directly on another
driver/system, hardware initialization reports that error and existing software
fallback applies; it does not retry the leaking ActivateObject path. Device-only
COM registration and other vendors remain unqualified. This fix avoids the
runtime defect; it does not repair Windows' ActivateObject implementation.

Reproduce the raw differential with the ignored diagnostic test:
`PRTS_MF_PROBE=activate|cocreate cargo test --release --test mf_activation_diagnostic -- --ignored --nocapture`.
Use PowerShell environment assignment on Windows. This isolated test deliberately
prints raw counts rather than serving as an acceptance gate. CoCreateInstance
is flat for all 20 cycles (305, then 303 after unrelated cleanup).

The strict gate is
`cargo test --release --test mf_encoder hardware_encoder_creation_does_not_leak_handles -- --ignored --nocapture`.
It now checks **every one of 20 cycles** against the first sample with the
original +2 tolerance. Full encoder creation/drop passed with **299 on all 20
cycles**. Both hardware functional regressions also passed: 90/90 decoded,
requested nonperiodic IDR, native bitrate restart to 999998 bps at a 1 Mbps
target, and existing low-cap software fallback at 155057 bps, 150/150 decoded.
Log: `C:/src/chromium/prts-m4leak-mf-tests.log`.

The unchanged release `PRTS_FRAME_MODE=gpu browser_m4` strict three-cycle gate
also passes: **392 / 391 / 391 handles**, with 30/30 decoded QSV D3D11 samples
per cycle (8.32 / 8.32 / 8.08 ms averages), exact page color, two targets,
resize/DPR2, pacing, credit/resource/memory bounds and clean shutdown. Log:
`C:/src/chromium/prts-m4leak-browser.log`. No warmup allowance was added.

`PRTS_FRAME_MODE=gpu PRTS_TEST_ENCODER=qsv npm run e2e:browser` passes both
tests in 13.60 s with actual **gpu-texture + d3d11-zero-copy** stats, decoded
pixels/input, navigation, DPR2, target switching, two instances, async startup
failure and exact daemon cleanup. Log: `C:/src/chromium/prts-m4leak-e2e.log`.
The disconnected-desktop override was used; foreground focus is still
unverified. Chromium was neither modified nor rebuilt for this fix.

Final Rust checks after the fix: `cargo build --release --bins`, `cargo test`
(16 ordinary tests passed, five opt-in tests ignored by default),
`cargo clippy --all-targets -- -D warnings`, and `cargo fmt --check` all pass.
All three hardware tests were separately executed and passed as described above;
the additional ignored raw probe was executed in both modes. Logs are
`C:/src/chromium/prts-m4leak-release.log`, `prts-m4leak-cargo-test.log`, and
`prts-m4leak-clippy.log`. Source, tests, notes and journal are staged only;
no commit or Chromium rebuild was made. Both strict lifetime gates now pass.

Other gaps: physical device-removal injection; acquired-mutex/device-loss still
requires protocol restart; connected-desktop focus; Windows10/other-GPU and
AppContainer qualification; asynchronous browser GPU completion.

## Earlier cleanup verification (before the activation fix)

After removing temporary diagnostics, `cargo build --release --bins` passed
(release-final.log), final `cargo test` passed16 ordinary/four ignored, all-target
Clippy with -D warnings, fmt check and seven Node tests passed again. Chromium
build8 passed and all changes are committed on work. Final browser_m4 completed
all three functional capture/QSV/teardown cycles; its strict process-handle gate
still failed (362/362/367; unrelated runtime cleanup adds sampling variation).
Final recovery passed disabled GPU, nonexistent adapter LUID, real import
rejection, exact CPU generation3 pixels and shutdown. Final auto/QSV E2E passed
with actual GPU/zero-copy assertions. Final focused hardware lifetime test
remains FAIL (see prts-m4-final-lifetime.log). No signoff, no threshold increase.

These failures are historical; the activation fix and new verification are
recorded above and in journal-m4.md.
