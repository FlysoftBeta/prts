# MVP Media Foundation latency qualification

The MF backend uses the Intel hardware H.264 MFT, with D3D11 NV12 surface input
on the GPU path and CPU NV12 as the fallback input path. This is the interim
backend; direct oneVPL remains priority 3 after MVP completion.

## Configuration

- Mandatory CODECAPI low-latency mode, CBR, zero B pictures, baseline profile,
  a two-second GOP, and explicit IDRs for startup, viewer feedback and bitrate
  restart. Intra refresh is not enabled; recovery uses those IDRs.
- H.264 HRD buffer: `max(ceil(bitrate_bps / 80), 32768)` **bytes**. This targets
  100 ms with a 32 KiB IDR floor: 50,000 bytes / 100 ms at 4 Mbps;
  32,768 bytes / 262.1 ms at 1 Mbps. The old `bps / 4` meant two seconds.
  [Microsoft documents bytes for H.264 HRD](https://learn.microsoft.com/en-us/windows/win32/directshow/avenccommonbuffersize-property),
  unlike MPEG VBV, which uses bits.
- Buffer, GOP, B-picture count, rate-control mode and low latency are read back
  after media-type negotiation. An incompatible MFT fails initialization instead
  of silently claiming that the requested configuration was applied. Optional
  DisplayRemoting scenario hints remain best effort.
- One synchronous request at a time on the dedicated COM MTA. Every request
  waits for its own output sample (two-second failure deadline); output timestamps
  must equal that input's timestamp. The old 100 ms empty-output return could
  allow a subsequent input to accumulate inside the MFT. No hidden startup queue
  is permitted by the new contract. The daemon retains its latest-frame slots.
- Bitrate changes recreate the MFT and reapply/read back the settings. Intel's
  accepted live bitrate property alone does not reliably change actual output.

At 150 kbps, a literal 100 ms HRD request (1,875 bytes) was accepted and read
back by Intel but caused the existing encode/decode regression to fail after
bitrate restart. A 32 KiB floor restores all 90/90 decoded pictures. That floor
corresponds to 1.75 seconds at 150 kbps; low-cap native hardware rate control
remains subject to the existing measured overshoot guard and software fallback.
Property readback is evidence of configuration, not a measurement of delay.

## Reproduction and measurements

From repo root, run `powershell -File scripts/test.ps1 -Suite mf`. This runs all
hardware tests sequentially with a five-minute deadline. The new
`hardware_low_latency_has_no_frame_queue` gate submits 180 changing 1280x720
pictures through each CPU/GPU path, including 4 Mbps -> 1 Mbps restart, forced
IDR at frame 25, and periodic GOP checks. Each call must immediately decode
exactly one picture; backend timestamp checks reject reordering. The gate prints
mean/p50/p95/p99/max latency and requires p95 below 100 ms.

Timing includes synchronous dispatch to the COM thread, CPU BGRA conversion or
GPU BGRA->NV12, asynchronous MFT polling and output byte copy. Source image
generation/GPU upload, decoder work, WebRTC and client display are excluded.
These are encoder request latencies, not glass-to-glass latency.

Measurements on 2026-10-21, Intel UHD Graphics 630 (LUID `0xe369`), release
build, immediate submission without wall-clock pacing (30 fps media timestamps):

| Input | Decoded immediately | Mean | p50 | p95 | p99 | Max |
| --- | --- | --- | --- | --- | --- | --- |
| CPU BGRA -> NV12 | 180/180 | 10.656 ms | 10.138 ms | 11.862 ms | 38.424 ms | 38.712 ms |
| D3D11 BGRA -> NV12 surface | 180/180 | 9.281 ms | 9.095 ms | 10.737 ms | 11.769 ms | 13.964 ms |

Both paths emitted IDRs at frames 0, 25, 85, 90 and 150. The separate dynamic
bitrate gate decoded 90/90; native 1 Mbps measured 993,961 bps. At 150 kbps the
unchanged overshoot guard rejected 461,344 bps and software fallback produced
155,057 bps with 150/150 decoded. Strict encoder lifetime: 301 process handles
on all 20 cycles. Browser M4 lifetime: 393/392/392 across three cycles.

Measurement logs: `C:/src/chromium/prts-mvp-mf-verified-mf-hardware.err.log`;
browser lifetime: `prts-mvp-mf-verified-m4.log`. These logs are local build/test
artifacts outside the repository. See journal-mvp.md for the exact commands,
exit statuses and final integration qualification.

Final integration: all 19 gates in the full run passed, including M1-M5 GPU/CPU,
recovery, odd/fractional sizes, four MF hardware tests, Rust19/Node8, v1/v2
GPU/QSV and CPU/software browser E2E, and synthetic E2E. The additional
`-Suite gpu` gate passed through the production launcher in default auto frame
mode. Chromium's active capture GPU reports vendor 32902 (`0x8086`), device
39877, LUID 58217 (`0xe369`), SwiftShader=0 on all four captured sources, alongside
decoded WebGL/video and DXGI QSV stats. That gate is included in future full
runs. Logs: `C:/src/chromium/prts-mvp-gpu-verified-gpu-adapter.log` and
`prts-mvp-gpu-verified-gpu-adapter-browser.log`.

Native desktop gates report no visible/uncloaked or taskbar-eligible browser
windows. Connected-desktop focus/physical cursor and Windows 10 qualification
remain unverified; this run explicitly used the disconnected-session override.
