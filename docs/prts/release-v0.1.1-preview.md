# PRTS v0.1.1-preview

This preview adds real touch input to the streamed Windows browser. Touch
devices send separate contact IDs through the frontend and WebRTC control
channel to Chromium's Aura input and native gesture recognizer. Supported
operations include tap, long-press context menu, one-finger scroll/fling,
pinch zoom, multiple simultaneous contacts and touching Chrome's app menu.
The video prevents local browser gestures and does not emulate mouse input.
Losing control or closing a viewer cancels held contacts.

Target title/load updates now preserve held input when the window, generation
and size stay the same. Previously those updates released active input and
could interrupt a gesture or drag.

## Downloads and quickstart

Download all browser archive parts (`.zip.001`, `.zip.002`, and so on), the
server ZIP, `SHA256SUMS.txt`, `SHA256PARTS.txt` and `JOIN.ps1` into one directory.
Reconstruct the browser ZIP in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -File .\JOIN.ps1
```

JOIN verifies every part and the reconstructed archive against SHA256 manifests.
Extract both ZIPs into the same directory and install Node.js 22 or newer:

```powershell
cd prts-server-win-x64
npm.cmd --prefix server ci --omit=dev
powershell -ExecutionPolicy Bypass -File start.ps1
```

Open http://127.0.0.1:8080 and create a Chromium instance. On a touch device,
operate the page and browser UI directly on the video. Auto encoding uses
Intel QSV through Media Foundation where available, with a software fallback.
Create another instance for an independently controlled browser stream.
`BUILD.txt` identifies source revisions; `BUILD-ARGS.gn` records the
non-component build flags. The browser product is PRTS; web-facing UA and
Client Hints retain Chrome identity.

## Known issues and scope

Flyout appearance can exceed 250 ms on GPU capture and Intel hardware encoding.
Omnibox popup checks are intermittent, including missing selected-row geometry.
These remain report-only preview known issues, deferred by the project plan.
Software-path latency is also report-only. Other pixel, input, lifecycle,
hardware and resource checks remain required.

Additional flyout families and independent video tracks for extra windows
within an instance remain incomplete. AppContainer/LPAC isolation is not
included. Direct oneVPL and NVENC remain future work; this release retains
the qualified Media Foundation and software encoders.

Windows 10 22H2 and physical foreground/cursor behavior on a connected desktop
remain unqualified. Desktop probes run in a disconnected session and check
hidden/cloaked HWNDs, taskbar entries and native cursor counters.

## Qualification

The touch acceptance gate drives the actual frontend with touch events and
observes trusted PointerEvent(pointerType=touch)/TouchEvent records in the
remote page, native gesture results, decoded app-menu/Newtab pixels, cancellation
and reconnect. It covers GPU/QSV and CPU/software at display scales 1, 1.25
and 2. Full and clean-extract results are recorded in `docs/prts/STATUS.md`
before publication.
