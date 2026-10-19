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

Use `source:"synthetic"`, `encoder:"auto"`, `"qsv"` or `"software"`, and
`isolation_profile:"job"`. Auto/qsv try hardware Media Foundation H.264 MFTs,
prefer Intel QSV, and fall back to OpenH264 with a warning on hardware failure.
Stats report the actual encoder. Chromium remains unsupported. Network bitrate
caps and outgoing encrypted media packet latency/loss are supported. Capabilities
report size limits (even device dimensions, 16..4096/axis, 8,294,400 pixels).

`tests/end_to_end.rs` is a standalone self-loop client: it launches the daemon,
uses real UDP/DTLS/SRTP/SCTP, decodes video and checks input pixels, target
switch/resize, stats, PLI, renegotiation/ICE restart and cleanup. It does not
depend on `server/`. See [implementation notes](../docs/prts/prtsd-notes.md).

The hardware smoke test is opt-in (requires a hardware encoder):
`cargo test --release --test mf_encoder -- --ignored --nocapture`.
Run browser acceptance with `cd ../server; npm run e2e`; set
`PRTS_TEST_ENCODER=qsv` or `auto` to exercise hardware selection (default software).
