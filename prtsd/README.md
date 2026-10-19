# prtsd (Phase 1 synthetic daemon)

Windows x64, Rust stable MSVC, Visual Studio C++ tools and Windows SDK:

```powershell
cd prtsd
cargo build --release
cargo test --release -- --nocapture
.\target\release\prtsd.exe
```

`prtsd.exe` reads **binary length-prefixed JSON** from stdin and writes the same
protocol to stdout. It is not an interactive line-oriented program. Send `hello`
first; continuously drain stdout. Diagnostics go only to stderr. EOF closes all
instances and WebRTC sessions. No Chromium executable is needed.

The [protocol](../docs/prts/protocol.md) defines commands, events, SDP/ICE, and
DataChannels. `prtsd` is the WebRTC **offerer**. Create an instance, wait for
`instance.ready`, create a session, relay `session.offer` to the viewer, then
relay its answer and ICE candidates back. Default video is 1280x720 at 30 FPS;
`options.fps` accepts 1..60. Software encoding is OpenH264 constrained baseline.

Use `source:"synthetic"`, `encoder:"auto"` or `"software"`, and
`isolation_profile:"job"`. Chromium, explicit QSV, nonzero packet latency and
packet loss return `unsupported`. Network bitrate caps are supported. Capabilities
report size limits (even device dimensions, 16..4096/axis, 8,294,400 pixels).

`tests/end_to_end.rs` is a standalone self-loop client: it launches the daemon,
uses real UDP/DTLS/SRTP/SCTP, decodes video and checks input pixels, target
switch/resize, stats, PLI, renegotiation/ICE restart and cleanup. It does not
depend on `server/`. See [implementation notes](../docs/prts/prtsd-notes.md).
