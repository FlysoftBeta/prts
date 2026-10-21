# PRTS v0.1.0-preview

PRTS streams a headless Windows browser over WebRTC. The viewer operates the
browser toolbar and page, including app menus, page context menus, native select
popups and omnibox suggestions. Browser application branding is PRTS; websites
receive consistent Chrome user-agent and Client Hints values.

## Downloads and quickstart

Download the seven browser archive parts (`.zip.001` through `.zip.007`),
the server ZIP, `SHA256SUMS.txt`, `SHA256PARTS.txt` and `JOIN.ps1` into one
directory. In PowerShell, reconstruct the browser ZIP:

```powershell
powershell -ExecutionPolicy Bypass -File .\JOIN.ps1
```

The script verifies every part and the final ZIP against their SHA256 manifests.
Extract the resulting browser ZIP and the server ZIP into the same directory
and install Node.js 22 or newer. Then run in PowerShell:

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
The browser ZIP is split to avoid interrupted large uploads; rejoining produces
the exact clean-extract-qualified archive, without changing any browser files.

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

## Known issues

Flyout appearance can exceed 250 ms even with GPU capture and Intel hardware
encoding. Omnibox popup checks are intermittent: a second suggestion popup or
its selected-row geometry can be missing after typing a URL. These failures
are recorded as report-only for this preview and deferred for investigation.
Other decoded-pixel, input, lifecycle, hardware and resource gates remain required.

Clean-extract popup results: all app/context/select variants passed; both CPU
omnibox variants passed, while both GPU omnibox variants failed the second
selected-row geometry probe at DSF1.25. Their faults and popup-host lists were
empty. Across24 GPU/QSV appearance samples, decoded latency was72.55–239.49ms;
across24 CPU/software samples it was50.38–278.02ms (one over250ms).

## Validation

Qualification runs the acceptance suite against the component build and a clean
extraction of these archives, with the known issues above reported separately.
Required checks include Chrome
identity in pages/workers, PRTS executable/About branding, CPU/GPU popup matrices,
input, lifecycle, resource limits, Intel hardware adapter/encoder and WebRTC.
Final evidence and any limitations are recorded in `docs/prts/STATUS.md`.
