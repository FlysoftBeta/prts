# PRTS v0.1.0-preview

PRTS streams a headless Windows browser over WebRTC. The viewer operates the
browser toolbar and page, including app menus, page context menus, native select
popups and omnibox suggestions. Browser application branding is PRTS; websites
receive consistent Chrome user-agent and Client Hints values.

## Downloads and quickstart

Download both Windows x64 archives and `SHA256SUMS.txt`. Extract both archives
into the same directory and install Node.js 22 or newer. Then run in PowerShell:

```powershell
cd prts-server-win-x64
npm.cmd --prefix server ci --omit=dev
powershell -ExecutionPolicy Bypass -File start.ps1
```

Open http://127.0.0.1:8080 and create a Chromium instance. The default mode streams
the whole browser window. Auto encoding uses the Intel Media Foundation hardware
encoder when available, with a software fallback. Create another instance for
another independently controlled browser stream.

Check each downloaded archive against `SHA256SUMS.txt` with
`Get-FileHash -Algorithm SHA256 <archive.zip>`. `BUILD.txt` records the source
revisions and Chromium PIN; `BUILD-ARGS.gn` records the non-component build flags.

## Preview scope

Qualified popup families are app menus, page context menus, native select popups
and omnibox suggestions. Touch gestures, other popup families and independent
video tracks for additional windows within one instance remain incomplete.

The hardware path uses D3D11 shared textures, GPU composition and Intel QSV via
Media Foundation. Direct oneVPL and NVENC are planned. Software-path latency is
report-only; decoded pixels, input, restoration and resource checks still apply.

This preview does not provide AppContainer/LPAC tenant isolation. Windows 10
22H2 and physical foreground/cursor behavior on a connected desktop remain
unqualified. Current desktop checks run in a disconnected session and verify
hidden/cloaked windows, no browser taskbar entry and native cursor counters.

## Validation

Before publication, require the complete acceptance suite against both the
component build and a clean extraction of these archives, including Chrome
identity in pages/workers, PRTS executable/About branding, CPU/GPU popup matrices,
input, lifecycle, resource limits, Intel hardware adapter/encoder and WebRTC.
Final evidence and any limitations are recorded in `docs/prts/STATUS.md`.
