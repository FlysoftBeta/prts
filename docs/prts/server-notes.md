# Node sample control plane and viewer

Requires Node.js 22+ (Node LTS 24.20.0 was installed on the development machine).
From the repository root:

```powershell
cd server
npm ci
cd ..
node server
```

Open `http://127.0.0.1:8080`. `npm start` inside `server/` also works.
If PATH has not refreshed, use `C:\Program Files\nodejs\node.exe` and `npm.cmd`.
The server binds to IPv4 loopback; localhost with the same port is also accepted.
Create a Chromium instance (CPU fallback), then select it to connect a controlling
viewer. Enter an http/https URL; Synthetic remains selectable for tests without
Chromium. The source dropdown defaults to Chromium.
Uncheck Request control and reconnect for view-only access. Each WebSocket has
at most one viewer session; disconnecting releases its daemon session. Instance
management is shared between local operators. An instance remains alive after
its viewer disconnects; use Close to destroy it.

## Configuration and lifetime

- `PORT`: HTTP/WebSocket port, default 8080 (0 selects a free test port).
- `PRTSD_PATH`: executable path; default is the absolute repository-relative
  `prtsd/target/release/prtsd.exe`. No shell is used, including for paths with spaces.
- `PRTS_CHROME_PATH`: trusted patched browser executable path, inherited by prtsd;
  default `C:/src/chromium/src/out/Default/chrome.exe`. This is distinct from
  `PRTS_BROWSER`, which selects the headless **viewer** in acceptance tests.
- `PRTSD_ARGS`: optional JSON array of separate arguments, default `[]`.
  For the mock, set `PRTSD_PATH` to the full Node executable and `PRTSD_ARGS` to
  `["C:/Users/Flysoft/Desktop/chromium/server/test/mock-prtsd.js"]`.

Stderr is inherited; stdout is exclusively framed JSON. The server sends hello
first, then configures one-second daemon stats. Requests have a five-second
deadline; timeout/framing errors kill the connection because mutation state is
uncertain. Daemon exit closes connected viewers with WS code 1011. HTTP remains
available and subsequent requests report failure; restart Node after fixing the
daemon. There is no automatic restart or replay of mutations. SIGINT/SIGTERM
closes stdin for graceful daemon shutdown and allows five seconds before killing
it. Abrupt Node termination relies on prtsd's stdin-EOF shutdown contract.

## WebSocket API

`/ws` uses **text JSON messages**, one complete protocol section 3 object per WS
message, without the four-byte pipe prefix. Request/response fields, method names,
parameters, errors and daemon event fields match protocol.md. A WS client can
request `hello` to receive the cached daemon hello. Node assigns separate stdio
request IDs and restores each original WS ID on replies. IDs must be unique per
WS connection. Unknown methods return unsupported. Malformed/binary requests or
duplicate IDs close the WS. Limits: 4 MiB/message, 64 pending requests/client,
8 MiB outgoing queue, 100000 lifetime IDs/client; reconnect at the ID limit.

Node replaces `session.create.viewer_id` with its own random connection identity.
Only the owning WS may issue commands for a session; only a controlling viewer
may explicitly activate targets. Session events and per-session stats are routed
only to their owner; instance/target events and encoder stats go to all local
operators. ICE emitted before an offer response is buffered (maximum 256) until
the offer response is sent. SDP and candidates are otherwise relayed unchanged.
The frontend also buffers ICE until its remote description is installed and
filters candidates from old ICE generations.

The sample authorizes the local operator through loopback reachability and exact
Host/Origin checks. This is a shared local administration UI, not a multi-user
authentication system. Remote deployment needs HTTPS/WSS and application-specific
authentication/instance authorization. No raw stdio endpoint is exposed.

## Viewer behavior

prtsd offers video and creates both specified DataChannels. The frontend never
creates a sending track. Input stays on reliable `prts.control.v1`; feedback stays
on `prts.feedback.v1`. The latest DataChannel target snapshot is the authority for
input generation/size. A null target hides old video. Pointer mapping excludes
object-fit bars, uses target CSS pixels without viewer DPR, and permits bounded
captured drags outside the viewport. Wheel modes are normalized. Clicking video
focuses a hidden text input that captures committed Unicode/IME/paste text once;
key down text remains informational. Escape returns keyboard focus to the UI.
Blur/switch/disconnect clears held input through focus=false and daemon lifecycle.
Custom cursors scale dimensions/hotspots by remote DPR. Receiver delay hints are
set to zero when supported; browsers may ignore them.

Quality measurements use getStats interval deltas: decoded FPS, received bitrate,
loss count (which can be negative), jitter, decode time/frame, jitter buffer
delay/frame, dropped frames and codec. RTT comes from the selected ICE pair.
Unavailable measurements display as unavailable; render and end-to-end latency
are not inferred. Encoder/frame/session stats are displayed from daemon events.
Network settings are sent unchanged and success is shown only after acceptance;
unsupported packet shaping is surfaced as an error.

## Verification and remaining integration

`cd server; npm test` runs Node's test runner without Chromium or prtsd. The mock
implements instance/target/session lifecycle and zero-work stats, intentionally
splits pipe prefixes and coalesces records, and emits ICE before offer responses.
Its SDP/candidate are deliberate stubs, its encoder name is `mock-no-encoder`,
and it rejects latency/loss shaping. It cannot show video or validate media/input.
Seven tests cover framing bounds/UTF-8/version/truncation, serialized writes under
backpressure, WS/HTTP operation, session ownership and disconnect cleanup,
early ICE, target operations, stats routing, DPR/letterbox mapping and rate math.
Input tests also cover focus cleanup, wheel modes, target generation changes and
single committed Unicode/IME text delivery; daemon exit is tested separately.

Live animation, actual DataChannel input/IME behavior in a browser, QSV/software
codec negotiation, dynamic video sizing, shaping and low-delay behavior still
require end-to-end testing with the concurrent Rust implementation. The UI
supports one video viewer at a time and manual reconnect; no TURN configuration
UI or automatic ICE restart is provided (daemon-provided ice_servers are used).
No changes to protocol.md were needed.

## Phase 1 part C: integrated browser acceptance

`npm run e2e` uses the new dev dependency `puppeteer-core` and launches the real
release `prtsd.exe`, the real server on an ephemeral loopback port and an
installed browser in headless mode. It never uses the mock and never downloads
or builds Chromium. Build `prtsd` first, then `npm ci` in `server/`. Selection is
`PRTS_BROWSER`, installed Chrome, installed Edge, then the existing Chromium
`out/Default/chrome.exe`. The integrated run passed on Windows 11 with Microsoft
Edge 155.0.4283.62. All seven existing Node tests and all four Rust integration
tests also pass.

Coverage includes increasing rendered frame counts and H264 getStats, actual
mouse move/down/up and keyboard typing through the frontend handlers, decoded
pointer/click/text pixels, tab creation/resize/switch/close, two instances and
three simultaneous viewers (one sharing an existing instance view-only), UI
RTT/FPS/bitrate/loss/jitter/decode stats, daemon encoder stats, and a measured
encoder bitrate reduction after applying a cap. Very low encoder targets can
overshoot; the test compares averaged measured output rather than expecting a
hard packet bandwidth ceiling. Continued playback after the cap is checked.

Integration fixes:

- When another operator creates an instance, its ready event now refreshes the
  full instance list. Previously existing pages silently omitted that instance.
- Closing a selected instance first disconnects its local viewer; requested
  session closure no longer appears as an error. A control-channel closure is
  shown in session status, allowing later lifecycle events to clear it normally.
- A server launched with Node IPC sends its URL and daemon PID to its supervisor
  and shuts down on IPC disconnect. This gives the browser test a graceful
  Windows shutdown path without adding an HTTP shutdown endpoint. The test
  verifies exit code zero and that the exact daemon PID is gone, even on failure.

Background pages can suspend animation-frame callbacks after their track closes.
The test brings the closing page forward and polls lifecycle DOM state with a
timer; video checks still require actual advancing video frames. It fails on
browser exceptions or frontend errors and saves the small full-page PNG
`docs/prts/phase1-e2e.png`. Packet delay/loss shaping remains explicitly
unsupported; Windows 10 and remote-network/TURN qualification remain open.


## Phase 2 M3: real Chromium source

Build the patched Chromium incrementally and `cargo build --release` in prtsd,
then run `node server`. The existing source selector now defaults to Chromium
(CPU fallback). No arbitrary executable path or switches are accepted from
viewer requests. `npm run e2e:browser` starts a local color/animation/input
fixture and verifies decoded real-page pixels, click/text reactions, two targets,
resize/DPR2, switching/static refresh, close, two instances and displayed stats.
`npm run e2e` explicitly chooses Synthetic and remains Chromium-independent.
Browser creation returns starting immediately; cold component DLL loading can
take much longer than a normal five-second command deadline. Close/EOF cancels
startup. See phase2-m3.md for frame lifetimes and final acceptance results.

## M4 frame mode and path diagnostics

Start the server with `PRTS_FRAME_MODE=auto` (default), `gpu`, or `cpu` in its
environment; the spawned daemon inherits this setting. It is trusted process
configuration, not tenant-controlled Chromium arguments. `gpu` prefers native
textures with mandatory CPU recovery. To require the fast path, inspect the
actual interval stats displayed in the encoder panel: `frames.backend` must
be `gpu-texture` and QSV `encoder.input_path` must be `d3d11-zero-copy`.
Software/OpenH264 with texture capture reports `staging`; forced CPU reports
`cpu-section` and `cpu-nv12` for QSV or `cpu-i420` for OpenH264.

`npm run e2e:browser` now asserts the expected actual backend/input path in
addition to decoded pixels/input/lifecycle. Set `PRTS_TEST_ENCODER=qsv` for
native GPU QSV qualification, and `PRTS_FRAME_MODE=cpu` for CPU regression.
`npm run e2e` continues to select the synthetic source. M4 acceptance and
comparable capture/encode measurements are documented in `phase2-m4.md`.
