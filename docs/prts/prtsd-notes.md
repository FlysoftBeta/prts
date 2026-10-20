# prtsd Phase 1 part A handoff

Current M4 status: native GPU capture/DXGI QSV input and CPU/software recovery
work, but signoff is blocked by the reproduced Intel MFT handle lifetime leak.
See [phase2-m4.md](phase2-m4.md) and the M4 section below; earlier milestone
sections are historical.

Implemented and verified on Windows 11 x64, 2026-10-19. No Chromium build,
Chromium launch/source changes, server/ changes, or git commits in this task.
Windows 10 22H2 runtime qualification remains outstanding.

## Build and run

Rust was absent from PATH. Installed Rustup 1.29.1 with winget and selected
`stable-x86_64-pc-windows-msvc` (rustc 1.99.1). Cargo automatically found VS 18
Community MSVC 14.51 and the installed Windows SDK; no vcvars workaround was
needed. A newly opened terminal should inherit Cargo's PATH. The executable is
`prtsd/target/release/prtsd.exe`.

```powershell
cd C:\Users\Flysoft\Desktop\chromium\prtsd
cargo build --release
cargo test --release -- --nocapture
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

If an existing terminal has stale PATH, invoke
`& "$env:USERPROFILE\.cargo\bin\cargo.exe"` instead of `cargo`.
The daemon is a stdio child, not an HTTP service. A raw console does not provide
the framing. The standalone integration test is the quickest real media check.
The control plane must keep stdout drained, send hello first, and close stdin
to shut down. Logs and native encoder diagnostics go to stderr only.

## Architecture and dependencies

Single Rust 2024 crate; Cargo.lock pins the resolved dependency graph. Direct
versions were selected from crates.io's latest stable releases:

- `webrtc` **0.21.1**, and its matching `rtc` **0.21.1** protocol types. This is
  the new async wrapper over the Sans-I/O core, not the older webrtc 0.14 API.
  Tokio runtime, ring crypto, host ICE, H.264 video, no audio/STUN/TURN configured.
- `openh264` **0.9.9**, built from bundled source through the MSVC C++ toolchain.
  Baseline/constraint_set1, realtime camera mode, bitrate RC, frame skipping,
  low complexity, two threads, periodic IDR every two seconds; no B frames.
- Official `windows` **0.62.2** for the current Win32 process diagnostic.
  Synthetic sources need no native browser handles or Job Objects. Actual pipe,
  Job, D3D11 and Media Foundation integration is deferred, not simulated.
- Tokio **1.53.3**, serde **1.0.229**, serde_json **1.0.154**, anyhow **1.0.104**,
  URL **2.5.8**, font8x8 **0.3.1**, plus bytes and async-trait.

`transport.rs` uses separate blocking stdin/stdout workers with exact partial
read/write loops. The JSON allocation limit is 4 MiB. The stdout writer is
serialized, bounded to 1024 messages and 8 MiB including an in-flight write;
stats may coalesce, lifecycle/response overflow triggers shutdown. A malformed
record/version or duplicate request ID terminates the connection and closes its
instances. Request IDs are retained for uniqueness; one connection has a finite
one-million-request lifetime budget. Commands have five-second deadlines.

`daemon.rs` is the state actor. It serializes mutation/event ordering, tracks
eight instances, up to 256 targets each and 32 total viewers, runs startup and
load completion ticks, validates ownership/input and emits interval stats.
Creation returns starting before the loading target.created/instance.ready
events. Initial/navigation loading completes on a subsequent tick. Closing an
active tab activates the lowest surviving ID; IDs are never reused. Closing the
last tab leaves the instance/sessions alive and sends target=null.

`video.rs` runs one encoder worker per instance. It produces only with a
connected viewer and active target. There is one transient CPU-owned BGRA frame
and one latest encoded-frame slot, with no catch-up queue. I420/encoder reads
complete before BGRA storage is released. Watch receivers keep only the newest
encoded access unit per viewer. An epoch check discards encode work made stale
by resize/activation; video dimensions change on the same track. The synthetic
adapter therefore has bounded ownership/credit semantics without implementing
the future Windows file-mapping/texture wire protocol.

The viewport has a moving marker/grid, ID, generation, frame/time labels, pointer
test dot, click flash, buttons/focus/last key/wheel state and committed text.
Each target retains its own text/input state; switch/blur/owner loss releases
held keys/buttons. Committed text keeps the last 256 Unicode scalars. Font8x8
unsupported glyphs display as U+XXXX, so non-ASCII commits remain observable.
The central third rectangle switches cursor shape. Synthetic input indicators
are test content, separate from browser cursor overlays.

`encoder.rs` exposes a VideoEncoder trait and software implementation. BGRA to
I420 is explicit BT.601 limited-range conversion, including chroma averaging.
Instance/network/REMB maxima are combined with min(); the configured minimum
never raises a smaller simulation/congestion cap. Bitrate changes reinitialize
the encoder, producing a fresh IDR. Rate control is an encoder target, not a
strict packet token bucket: short IDR bursts can exceed the requested average.
Even device dimensions 16..4096/axis and at most 8,294,400 device pixels are
advertised and validated before allocation. CSS dimensions/DPR are validated
separately; DPR transforms input only at rendering. No letterboxing is added.

`viewer.rs` gives each viewer its own PeerConnection, SSRC space, RTP packetizer,
bounded DataChannels and media task. prtsd always offers. Remote trickle ICE is
queued up to 256 entries before the answer; ufrags filter old candidates. Local
ICE includes mid, index and usernameFragment. Only one offer may be outstanding.
Reliable ordered `prts.control.v1` carries ready/target/cursor/input/errors;
unordered `prts.feedback.v1`, maxRetransmits=0, carries telemetry. Both use
protocol `prts-json-v1`. Discrete input never uses the unreliable channel.
Only one session owns control; view-only input is rejected. Disconnect releases
ownership; failed/closed transport or channel closes the session.

A near-wire interceptor counts RTP bytes including retransmissions, handles
PLI/FIR by forcing an IDR and consumes REMB into the encoder cap. Slow media
submissions time out instead of holding capture resources. Stats use interval
counters/timings: actual fps/encoded bitrate, conversion+encode time, capture to
encode/send latency, captures/drops, per-viewer RTP bitrate, ICE RTT when sampled,
REMB and validated viewer feedback. Missing QP/estimates/latency samples are null.

## Protocol decisions

Followed protocol.md's offerer role and DataChannel names despite the task's
informal references to accepting a client offer and an "input" channel.
Only three small additive/clarifying edits were made to protocol.md:

1. Added optional InstanceOptions.fps, default 30, Phase 1 range 1..60, with
   effective value/range advertised. The task required configurable FPS but
   the options schema did not previously expose it.
2. Added optional stats.encoder.encode_ms_avg, conversion+encoder time excluding
   rendering. Existing capture_to_encode timing includes rendering.
3. Clarified that the section 5 synthetic pointer indicator is test content,
   consistent with the general rule that the browser cursor is not baked in.

Smaller software size/resource limits are allowed by section 1 and advertised
in hello capabilities. Source/encoder capabilities list only implemented paths;
source=chromium and explicit qsv return clear unsupported errors. Auto resolves
to software. job records policy and does not claim synthetic process isolation.
Nonzero latency/loss is rejected atomically with unsupported, leaving previous
settings intact; zero-delay/loss bitrate-cap settings work.

## Verification

`cargo build --release`: success with no errors.
`cargo clippy --all-targets -- -D warnings`: success.
`cargo test --release -- --nocapture`: four tests pass:

- Byte-at-a-time and coalesced records, UTF-8 byte lengths, malformed/truncated
  length limits and EOF.
- Real OpenH264 encode/decode at default 1280x720; verifies SPS profile_idc=66
  and constraint_set1, and first IDR decodes to the requested dimensions.
- Separate daemon child and webrtc-rs viewer over actual UDP/DTLS/SRTP/SCTP:
  hello/create ordering, unsupported paths, ownership conflict, offer/answer,
  trickle/end-of-candidates, PLI followed by IDR within 1.5 seconds, decoded
  pointer/text pixel changes (including a non-ASCII commit), cursor event,
  feedback, stale generation error, actual encode/RTP stats, renegotiation,
  ICE restart with new ufrag, network cap/unsupported shaping, two targets,
  navigate/activate/resize with decoded dimension change, target=null, new target
  ID, session/instance close and child exit. A passing run received 37 RTP
  packets and decoded 18 frames; counts vary with scheduling.
- Separate child runs prove duplicate IDs, wrong v and zero-length framing
  terminate the daemon while emitting cleanup events; EOF exits successfully.

No Node-server dependency or Chromium build is involved. The test validates real
decoded video, not just SDP syntax or a mocked track. A real browser/Node viewer
and Windows 10 deployment have not been exercised in this job.

## Known gaps / next work

- **Chromium, browser pipe protocol, D3D11/NT texture import, QSV, zero-copy and
  Job/AppContainer/LPAC are unimplemented.** Explicit unsupported paths never
  pretend these capabilities are working. The encoder trait is the QSV seam.
- **No SRTP/SRTCP latency/loss shaper** yet. Add a bounded 4 MiB/session packet
  layer with seeded loss, delay, retransmission coverage and queue flush on
  settings replacement/close. RTP interceptors are before SRTP and are not used
  to fake the contract. No playout-delay extension is currently sent.
- Host ICE only; no TURN or remote-network/firewall qualification. Offer SDP uses
  constrained-baseline profile-level-id=42e01f with level asymmetry. Negotiated
  receiver-level enforcement for high resolution/FPS needs browser qualification
  before using the maximum software viewport limits; 720p30 and the test resize
  are verified. REMB is supported; a full TWCC/GCC adaptation loop is not.
- webrtc/rtc 0.21.1 remote DataChannel accessor metadata loses
  max_retransmits (reports None). Source inspection confirms DCEP/SCTP retains
  partial reliability; local creation uses Some(0). The test verifies channel
  protocol/order and feedback receipt, not retransmission behavior under loss.
- Synthetic committed text is an input display, not a complete text editor or
  IME preedit implementation. Custom PNG cursors and scripted popups are absent.
  Repeated invalid input closes the session; stale-state errors are rate limited.
- No claim of exhaustive adversarial SDP fuzzing, browser codec interoperability,
  long-run resource/soak testing or precise glass-to-glass latency measurements.

## Phase 1 part C: real browser integration

The real release daemon now passes `cd server; npm run e2e` through the real
Node control plane and frontend on Windows 11 with installed headless Microsoft
Edge 155.0.4283.62 (no Chromium build). No Rust source changes were required.
`cargo build --release` and all four `cargo test --release -- --nocapture`
integration tests also pass in this integration job.

The Puppeteer test verifies increasing decoded video frames, decoded white
pointer/red click pixels and committed text pixels after trusted mouse/keyboard
events through the frontend. It also exercises tab create/resize/switch/close on
the same video track, two independent synthetic instances and three simultaneous
viewers, including two viewers of one encoder. Actual receiver getStats fields
and daemon encoder/frame/session stats must appear in the UI. A 50 kbps target
cap reduced measured encoded bitrate from about 554 kbps to 123 kbps in one run;
the test averages multiple complete intervals and requires a measured reduction
of at least 20%, then confirms video frames still advance.

OpenH264's target is not a strict bandwidth limit: at this very low cap the
synthetic 720p source drops frames and overshoots the target. QP and true
capture-to-display latency remain unavailable. Packet latency/loss shaping was
not added; nonzero values still return unsupported, and no RTT/loss shaping
claim is made. Windows 10 qualification, QSV and Chromium capture remain open.

The test closes live instances through the UI, closes the browser and uses Node
IPC disconnect to request graceful server shutdown on Windows. It verifies both
server exit code zero and disappearance of the exact daemon PID, including on
test failure. The screenshot is `docs/prts/phase1-e2e.png`; browser override and
run instructions are in the root README and server notes.

## Phase 1 part D: hardware encoding, motion under caps, packet shaping

2026-10-19, Windows 11 development machine: **Intel(R) UHD Graphics 630**,
PCI VEN_8086 / DEV_9BC5; no NVIDIA adapter enumerated. No Chromium build or
source changes. Earlier sections describe historical milestones; this section
supersedes their QSV/shaping unsupported statements.

`encoder=auto` and `qsv` enumerate hardware H.264 MFTs with
MFT_ENUM_FLAG_HARDWARE, preferring Intel vendor 8086 / Intel Quick Sync names.
`software` selects OpenH264 directly. Initialization, dynamic bitrate or encode
failure switches to software and emits stderr plus a scoped warning log event;
instance.ready and interval stats carry the actual backend name. The requested
preference remains in instance options. A fallback sticks for that instance,
including resize, rather than repeatedly retrying a broken device.

The MF implementation owns COM/MF on a dedicated MTA thread. It unlocks the
async transform and consumes NeedInput/HaveOutput events with bounded polling
and input/output deadlines. Intel's initial output-type change is negotiated
before continuing. Codec controls require low latency, CBR, zero B pictures,
a two-second GOP, mean bitrate, and requested IDR. Output is baseline H.264
Annex B. Bitrate changes call ICodecAPI, then recreate the MFT with matching media-type
bitrate because the UHD 630 driver ignores successful live property updates.
The replacement emits fresh SPS/PPS and IDR; public RTP time stays continuous.
BGRA converts to BT.601 limited-range I420 then interleaves UV into NV12 in an
MF memory sample. `cpu_sample` is the future device-manager/DXGI surface seam;
**D3D11 upload, GPU conversion, texture import and zero-copy are deferred**.
See Microsoft's [hardware MFT contract](https://learn.microsoft.com/en-us/windows/win32/medfound/hardware-mfts)
and [asynchronous MFT event model](https://learn.microsoft.com/en-us/windows/win32/medfound/asynchronous-mfts).

OpenH264 now disables frame skipping at caps >=150,000 bps, allowing its full
QP range (0..51) to reduce quality first; below that it may skip. The cap is
still an encoder average target, not a strict network token bucket. In the
180-frame moving synthetic 1280x720 release encoder regression:

| Target bps | Encoded bps | Encoded FPS | Conversion + encode ms/frame |
| ---: | ---: | ---: | ---: |
| 4,000,000 | 155,458 | 30.00 | 9.70 |
| 300,000 | 206,473 | 30.00 | 15.16 |
| 150,000 | 147,826 | 30.00 | 11.73 |
| 50,000 | 50,841 | 14.83 | 6.53 |

These deterministic-source rates are not directly comparable to wall-clock
browser runs (frame/time text and animation differ). OpenH264 prints a generic
frame-skip-disabled RC warning; measured low-cap output is checked rather than
assuming strict control. No QP statistic is fabricated.

Packet shaping wraps each viewer's actual encrypted UDP socket through
webrtc-rs Runtime::wrap_udp_socket. RFC7983 RTP/RTCP first-byte classification
selects SRTP/SRTCP, including retransmissions. ICE/STUN, DTLS/SCTP, inbound
traffic and input bypass shaping. A seeded SplitMix64 loss decision is made
per outgoing datagram, including seed zero. GSO is disabled at this wrapper.
The queue retains at most 4 MiB per session including an in-flight datagram;
overflow discards oldest queued packets. Replacement reseeds and flushes;
close flushes and aborts the tracked worker. Drops are cumulative per session
and reported separately; no inbound loss or whole-frame delay is simulated.
Socket tests exercise actual UDP delay/control bypass, deterministic loss,
queue bounds/oldest-drop behavior, replacement and close.

The browser test defaults to explicit software; set PRTS_TEST_ENCODER=qsv or
auto to exercise selection. It measures encoder/receiver stats at baseline,
150 kbps and 50 kbps, requires >=15 encoded FPS at 150 kbps without resizing,
checks increased receiver transition jitter with 400 ms added delay and rising
receiver packetsLost plus shaper drops with 15% packet loss, then verifies
recovery. ICE RTT intentionally bypasses media shaping. These are transport,
decode and publication measurements, not glass-to-glass capture latency.

Two rate-control feedback bugs were corrected during browser qualification:
encoded-frame sequence numbers now exclude deliberate capture/encoder skips,
so skips do not request unnecessary IDRs; RTP duration follows elapsed capture
time per viewer across skipped/coalesced frames, preventing false transit jitter
and collapsing REMB estimates. Software bitrate updates now use OpenH264
ENCODER_OPTION_BITRATE and RC_FRAME_SKIP in place, preserving prediction and
rate-control history. A regression changes bitrate on 30 consecutive frames
and verifies continued decoding with only the initial IDR. The synthetic click
indicator remains red while a button is held, then retains its short flash,
so decoded-input checks observe persistent button state under scheduling load.

Hardware qualification: the opt-in release MF smoke test activates **Intel?
Quick Sync Video H.264 Encoder MFT**, decodes 90/90 1280x720 frames, checks
baseline SPS profile_idc=66, changes bitrate 2,000,000 ->150,000 bps at frame 45,
and observes a requested nonperiodic IDR at frame 25 without latching every
frame to IDR. Conversion+encode averages were approximately 11-13 ms/frame
(12.19 ms in the parent verification run). NVIDIA/NVENC was not available to
qualify. Real device-removal/driver failure and a machine lacking any hardware
MFT remain untested physically; error paths fall back to software. Windows 10
22H2, Chromium capture and production isolation remain qualification gaps.

Final software browser acceptance (release prtsd + Node + headless Edge,
2026-10-20) passed in 61.88 s with two instances and three viewers:

| Target | Measured bps | Encoded FPS | encode_ms_avg |
| --- | ---: | ---: | ---: |
| No simulation cap | 136,134 | 28.44 | 10.43 |
| 150 kbps, unchanged 1280x720 | 133,389 | 25.65 | 12.80 |
| 50 kbps, unchanged 1280x720 | 50,228 | 14.16 | 13.72 |

The receiver's peak transition jitter rose 8->34 ms with 400 ms outgoing delay.
With 15% loss it reported packetsLost 0->peak 2 (final 0 after retransmission
repair), while cumulative daemon shaper drops rose 0->5. Loss is measured at
the receiver as well as the sender; NACK repair may revise cumulative lost
packets downward. Restoring settings resumed video, and instance/server/exact
daemon-PID teardown passed. Mouse/button/text decoded pixel checks and target
create/switch/resize/close passed on the same WebRTC track.

Final browser run with decoded-FPS measurements (2026-10-20):

| Requested encoder / setting | Active encoder | Measured bps | Encoded / decoded FPS | encode_ms_avg |
| --- | --- | ---: | ---: | ---: |
| software / no cap | OpenH264 | 128,619 | 23.90 / 23.22 | 12.65 |
| software / 150 kbps | OpenH264 | 120,872 | 22.83 / 22.44 | 12.69 |
| software / 50 kbps | OpenH264 | 47,990 | 12.07 / 12.65 | 12.79 |
| qsv / no cap | Intel Quick Sync MFT | 3,081,133 | 28.30 / 28.14 | 13.92 |
| qsv / 150 kbps | OpenH264 after warning/fallback | 137,883 | 20.35 / 19.23 | 13.19 |
| qsv / 50 kbps | OpenH264 | 29,262 | 10.92 / 8.28 | 11.85 |

Both full browser suites passed: software in 68.80 s, requested qsv in 58.63 s.
All low-cap runs retain 1280x720. With the qsv instance, 400 ms added delay
raised peak receiver jitter 6->50 ms; 15% loss raised packetsLost 0->peak 2
(final 0 after repair) and daemon drops 1->20. The software run measured
10->29 ms jitter, packetsLost 0->peak/final 1, and drops 2->7. Restore and exact
process cleanup passed. Encoded/decoded FPS use different sampling windows,
so decoded FPS can briefly exceed interval encoded FPS.

**Intel driver limitation:** native 150 kbps QSV qualification failed. Live
ICodecAPI SetValue succeeds without changing output, so reconfiguration now
recreates the MFT. A native 1 Mbps setting then measures 999,998 bps (from a
4,000,000 bps baseline), but rich 720p30 at a 150 kbps target still measures
about 440,120 bps even with max QP 51 and a small buffer. Low-delay VBR returns
E_INVALIDARG on this driver. After one second of media settles, a one-second
output window exceeding 150% of target raises an encoder error; the existing
warning + software fallback path takes over. The rich-source regression
measures 155,057 bps after fallback with 150/150 frames decoded. This is an
explicit compatibility fallback, not native low-bitrate QSV verification.
Native MFT restart adds a reconfiguration IDR and startup cost; seamless live
hardware rate changes need driver-specific qualification. Sustained low-cap
overshoot is bounded by media frames rather than a strict wire token bucket.

Final verification: cargo build --release passed; cargo test passed (eight
ordinary tests, two hardware tests intentionally ignored by default); explicit
cargo test --release --test mf_encoder -- --ignored --nocapture passed both
hardware tests (native smoke 90/90 decoded, 12.03 ms average; rich rate-control
regression 10.49 ms average across native and fallback phases). Clippy with
-D warnings, rustfmt check, seven Node tests and both software/qsv npm run e2e
runs passed. git diff --check is clean. No prtsd process remains after tests.
No Chromium build, changes outside authorized paths, or git commit.


## Phase 2 M3: browser CPU fallback

`instance.create.options.source="chromium"` launches the patched PRTS browser;
`"synthetic"` remains available without it. Configure the trusted executable via
`PRTS_CHROME_PATH` (default `C:/src/chromium/src/out/Default/chrome.exe`). Startup
runs off the control actor, with a separate bounded DLL-loader deadline and
cancellation on close/EOF. A kill-on-close Job Object contains the process tree;
job assignment is part of CreateProcess attributes, before the main thread runs.
This is lifetime containment, not AppContainer/LPAC security qualification.

`frame_source::FrameSource` selects synthetic or browser capture behind the
existing encoder/WebRTC path. Browser capture subscribes only while viewers are
connected and follows the active browser target. Browser target snapshots are
control-plane authority; DataChannel CSS coordinates and committed text use the
M2 input API, with a per-instance monotonically increasing wire sequence. Cursor
events come from Chromium. Input ownership loss sends focus=false.

This is explicitly a **CPU readback fallback**: Viz ARGB shared memory -> reusable
BGRA section -> bounded latest daemon CPU frame -> I420/OpenH264 or CPU NV12/QSV.
At most two section resources exist across retired/current generations; each
published frame holds a credit until the daemon has copied it.
CPU ingress coalesces per target, with at most two pending copies bounded by
registered sections; retiring a target's last section removes its unread copy.
This preserves a static background subscription when an active frame arrives.
Static refresh, subscription and PLI/keyframe requests use FRAME_REFRESH. Slow WebRTC viewers do
not hold sections. Capture-to-encode/send metrics start at daemon section receipt;
they exclude browser readback and viewer decode/display latency.

Standalone capture acceptance: `cargo run --release --bin browser_m3`.
Real page/WebRTC acceptance: `cd server; npm run e2e:browser`.
Existing synthetic acceptance: `npm run e2e`, `cargo test`, Clippy and rustfmt.
Details, measurements and M4 handoff: phase2-m3.md.

## Phase 2 M4: native texture input

`PRTS_FRAME_MODE=auto|gpu|cpu` is trusted daemon configuration (default `auto`).
`gpu` prefers textures but retains CPU negotiation/recovery; to require GPU,
check actual stats as the acceptance programs do. No extra Chromium launch
switch is needed: the HELLO offer selects modes and adapter LUID. `cpu` offers
only sections, preserving the unchanged M3 acceptance and encoder CPU inputs.

`d3d.rs` creates a hardware Intel D3D11 device (never WARP), validates imported
NT resources and their adapter LUID, and checks raw AcquireSync HRESULTs so
WAIT_TIMEOUT/WAIT_ABANDONED cannot be treated as successful Rust `Result<()>`.
Each wire frame copies to a private GPU BGRA image, waits for GPU reads, returns
key 0, and ACKs before queueing a per-target latest frame. Coalescing remains
bounded by the two registered resources, independently across targets.

The MF worker attaches an IMFDXGIDeviceManager before media-type negotiation
when the MFT advertises MF_SA_D3D11_AWARE. GPU frames convert through an
ID3D11VideoProcessor to private NV12 textures; MFCreateDXGISurfaceBuffer retains
each texture for the async MFT. Each sample owns its surface; an MFT-retained
sample can never be overwritten by the browser. This path performs GPU copies
and conversion but no CPU pixel readback. Encoder selection/fallback/bitrate
recreation remain unchanged. OpenH264 texture input uses staging BGRA readback
then CPU I420 conversion and explicitly reports `staging`.

Actual interval stats: `frames.backend` (`gpu-texture`, `cpu-section`,
`synthetic`) and `encoder.input_path` (`d3d11-zero-copy`, `staging`, `cpu-nv12`,
`cpu-i420`). These fields persist across intervals without newly encoded frames.
Use them together with the actual encoder name; requested QSV is not evidence
of hardware or zero-copy operation. Recoverable registration/import rejection
logs once per target and switches Chromium to a new CPU generation. Acquired
mutex failure/uncertain GPU completion still requires connection restart under
the protocol; physical device removal has not been injected.

Qualification commands and measurements: `phase2-m4.md`. Intel UHD 630 native
capture logs vendor 8086/device 9bc5, hardware enabled, SwiftShader false, LUID
0x000000000000e369 matching prtsd on this boot. No other vendor is qualified.

MF activation lifetime fix (2026-10-20): hardware-only MFTEnumEx still chooses
the encoder, but its MFT_TRANSFORM_CLSID_Attribute is instantiated with
CoCreateInstance rather than ActivateObject. CDB traced the former +3 handles
per encoder/restart to mfplat's CoRegisterDeviceCatalog path and combase's WIL
registry watcher on the adapter Classes key; matching catalog revoke calls and
all MFT shutdown variants did not reclaim it. Raw activation grows through 20
cycles even without a DXGI manager or media types. Direct CLSID creation is
flat, and the full encoder lifetime test passes 20 cycles at exactly 299 handles.
This removes growth across resize, bitrate restart and reconnect without a
pool or a relaxed handle gate. Low-latency codec settings and software fallback
remain. A driver whose hardware CLSID cannot be created directly reports an
initialization error and falls back to software; no ActivateObject retry is
made. Other vendors/device-only COM registration remain unqualified. See
phase2-m4.md for the runtime versions, stack, differential probe and results.
The unchanged browser_m4 strict three-cycle gate passes at 392/391/391 handles;
QSV browser E2E passes with gpu-texture and d3d11-zero-copy. Release build,
16 ordinary tests, all three opt-in hardware tests, all-target Clippy with
warnings denied and fmt check pass. No Chromium change or rebuild was needed.
