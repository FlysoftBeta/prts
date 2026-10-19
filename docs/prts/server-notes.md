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
Create a synthetic instance, then select it to connect a controlling viewer.
Uncheck Request control and reconnect for view-only access. Each WebSocket has
at most one viewer session; disconnecting releases its daemon session. Instance
management is shared between local operators. An instance remains alive after
its viewer disconnects; use Close to destroy it.

## Configuration and lifetime

- `PORT`: HTTP/WebSocket port, default 8080 (0 selects a free test port).
- `PRTSD_PATH`: executable path; default is the absolute repository-relative
  `prtsd/target/release/prtsd.exe`. No shell is used, including for paths with spaces.
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
