# PRTS v0.1.0-preview — Windows x64

PRTS streams a headless browser window, including its tab strip, address bar,
toolbar, app/context menus, select popups and address suggestions, through a
Rust daemon and a Node.js/WebRTC viewer. Mouse and keyboard operate the real
browser UI. Each browser instance has its own stream; multiple viewers can
share an instance.

Download both ZIPs and `SHA256SUMS.txt`. Extract both archives side by side.
Install Node.js 22 or newer, then run in PowerShell:

```powershell
cd prts-server-win-x64
npm.cmd --prefix server ci --omit=dev
powershell -ExecutionPolicy Bypass -File start.ps1
```

Open `http://127.0.0.1:8080`, enter a URL and create a PRTS browser instance.
Choose `auto` for hardware encoding with software fallback. Click the video
to interact with the page and browser UI. Stats show the actual capture and
encoder path. Create another instance from a second viewer tab.

The Chromium archive uses a non-component release build. The server archive
contains prtsd, the sample control plane/frontend and install/start instructions;
it does not include `node_modules`. `start.ps1 -ChromePath C:/path/chrome.exe`
can select another extracted PRTS binary. `BUILD.txt` records the source commit
and Chromium PIN. Verify downloaded ZIPs with `Get-FileHash -Algorithm SHA256`.

This preview uses Intel hardware H.264 through Media Foundation and D3D11
textures, with CPU/OpenH264 fallbacks. GPU/QSV flyout latency is gated;
fallback latency is reported. Application branding is PRTS, while web-facing
UA and Client Hints identify as Chrome for the pinned Chromium major version.

Known gaps: touch gestures; other popup families; independent tracks for
additional windows in one instance; AppContainer/LPAC and tenant isolation;
Windows 10 22H2 runtime qualification; connected-desktop physical foreground
and cursor qualification. Direct oneVPL QSV and NVENC are later milestones.
The server currently binds to loopback. See `docs/prts/lessons-flyouts.md` in
the source repository for measured results and investigation findings.
