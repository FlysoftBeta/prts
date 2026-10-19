# prtsd Phase 1 part A handoff

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
