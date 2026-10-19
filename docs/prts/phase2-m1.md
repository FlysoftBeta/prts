# Phase 2 M1: pipe and targets

Implementation and runtime qualification on 2026-10-20, Chromium
158.0.8111.0 at PIN caef9db47e2cf3fe8e9f07ddceeff6ddf6957385, Windows x64.

## Implementation

Chromium adds `chrome/browser/prts/prts_switches.h`, `prts_protocol.h`,
`prts_pipe_client_win.{h,cc}`, `prts_controller.{h,cc}`,
`prts_target_registry.{h,cc}`, `BUILD.gn`, and `prts_sources.gni`.
Existing edits are in `chrome/app/chrome_main.cc`, `chrome/BUILD.gn`,
`chrome/browser/BUILD.gn`, and `chrome/browser/headless/BUILD.gn` plus
`chrome_browser_main_extra_parts_headless.{h,cc}`.

ChromeMain validates browser-only `--headless=new --prts-pipe=...`, rejects
child process types, validates pointer-sized distinct real pipe handles, and
clears inheritance before ContentMain can start subprocesses. PreBrowserStart
prepares the UI controller and observations; PostBrowserStart removes the
ordinary startup tab and starts command processing; PostMainMessageLoopRun
stops transport/observation before browser services disappear.

Dedicated blocking reader and serialized writer threads process exact LE
length-prefixed PRTS envelopes. Incoming and outgoing queues each cap at
1024 records / 8 MiB. UI never waits on pipe I/O. Shutdown marks workers
stopped, repeatedly cancels synchronous I/O to cover the check/ReadFile race,
and joins off the UI sequence. Partial records are assembled exactly; framing,
version, flags, direction, ID, EOF and queue failures terminate the connection.
Target callback failures schedule controller teardown to avoid deleting the
registry reentrantly inside an event callback.

HELLO negotiates v1.0, CPU-section bit 2, up to three credits, LUID zero
(unavailable), maximum CSS dimensions 8192, 64M device pixels, popup feature.
No frames or events precede ACK. Frame pipe stays open without descriptors.
PING echoes its cookie; SHUTDOWN flushes OK before teardown. Target mutations
and snapshots follow protocol section 2.3, with events queued before responses.

The target registry observes GlobalBrowserCollection, tab strips, and
WebContents; renderer swaps, tab moves and tab replacements retain IDs.
Explicit targets get independent windows. An internal DevTools session applies
per-target CSS viewport/DPR emulation without a TCP debugging listener.
Creation uses the public `CreateBrowserWindow(BrowserWindowCreateParams)`:
contrary to the source survey, `Browser::Create` itself is private at this PIN;
the public wrapper delegates to it. Process/original-profile keepalives permit
zero windows and later recreation of an incognito profile. Ordinary tab closure
waits for actual destruction; a blocked beforeunload relies on prtsd's five-second
request deadline to fail the instance. Popup discovery preserves Chrome policy.

GN compiles browser-heavy sources and the Windows headless lifecycle TU in
`//chrome/browser:core`; headless extra parts sees only the controller interface.
Transport is a low-level source set. This avoids the extra-parts/core cycle.

Rust adds `prtsd/src/browser.rs`, `browser_wire.rs`, and `bin/browser_m1.rs`,
exports Windows modules in `lib.rs`, and expands official windows crate features.
The synthetic source and full daemon video path remain intact. Launcher uses
CreatePipe and STARTUPINFOEX with exactly three child handles, retains the
CreateProcess process handle (including PROCESS_DUP_HANDLE), closes child ends
in the parent, drains event/frame independently, and exposes the six target
operations, ping/shutdown and ordered events. See `phase2-m1-rust.md` for API
and standalone acceptance details.

## Build and acceptance log

All Chromium builds were detached incremental `autoninja -C out/Default chrome`
with DEPOT_TOOLS_WIN_TOOLCHAIN=0. No clean, deletion of out/Default, standalone
gn gen, cr apply/sync/export, or patches directory changes.

- First compile failed after 40.0s on Chromium's raw_span safety plugin; fixed.
- Next full incremental chrome build succeeded in 61.3s.
- Diagnostic controller relink succeeded in 25.7s.
- Final controller/registry incremental build succeeded in 38.8s (exit 0).
- gn check passed for prts transport/interface, browser:core, and chrome:chrome_dll.
- Registry also passed clang syntax validation with core's actual flags/plugins.
- Rust cargo build/test/check/all-target Clippy succeeded; ten ordinary tests,
  two opt-in hardware tests ignored. Existing synthetic tests pass.

Initial runtime attempts hit the five-second HELLO deadline before Chrome DLL
startup completed. Noninvasive cdb inspection found the main thread in
NtOpenKey / SaferpIsV2PolicyPresent / LdrpCodeAuthzCheckDllAllowedSrpV2 /
LdrLoadDll / LoadLibraryExW / MainDllLoader::Launch, before ChromeMain/PRTS.
Browser remained alive with no HWND or logging initialized. A complete cold
startup took 171.96s, followed by HELLO_ACK in 371.51ms. Subsequent warm startup
took 0.50s followed by HELLO_ACK in 320.29ms. The launcher now waits for
WaitForInputIdle before sending HELLO, with a separate bounded startup deadline
(300s default, configurable via PRTS_BROWSER_STARTUP_TIMEOUT_SECS); every HELLO
and command still has the normative five-second response deadline.

The first runtime test stopped at a test-observation issue: changing DPR while
keeping CSS dimensions does not require a page resize event. The page's size
reporter was changed to refresh its title periodically as well as on resize.
No Chromium workaround or system policy/security setting change was needed.

Required acceptance passed against the patched chrome.exe (2026-10-20 04:42
Asia/Singapore): initial list empty; first target active even activate=false;
two independent 640x480 and 960x540 CSS viewports confirmed by page JavaScript;
list snapshots; resize to 800x600; per-target DPR 2 with CSS 640x480 confirmed by
JavaScript; activation/generation changes; navigate acceptance followed by
title/URL/load completion events; ordered CREATED/CHANGED/CLOSED before mutation
responses; active-close selects lowest survivor; last-window closure stays alive
with idle heartbeat; fresh target recreation after zero windows; SHUTDOWN->OK and
process exit. Allowed window.open discovery passed kind=2, opener metadata and
no logical activation theft, using test-only --disable-popup-blocking.

Fragmented valid HELLO and shutdown passed. Truncated record + EOF,
post-handshake command EOF, and actual launcher process death without Rust
destructors all exited the browser within ten seconds. An interrupted earlier
driver also caused its browser to exit on pipe EOF in under two seconds.

Logs outside the repos: C:/src/chromium/prts-m1-acceptance-required.log and
required.err.log; Chrome diagnostic log prts-m1-chrome-required.log.
The acceptance binary's explicit disconnected-desktop override was set;
its overall pass does not cover the foreground check described below.

Extended acceptance also passed (`browser_m1.exe --extended-only`): reject
missing headless before window startup; missing/duplicate/null/nonhex/overflowed
handle switches; renderer/GPU/old or empty headless mode; unknown type, bad
magic/version/flags, even/reused/decreasing request IDs, short/oversized lengths,
wrong direction; nonoverlapping version returns correlated unsupported ERROR
before EOF; missing HELLO expires; flooding requests with an unread event pipe
cannot hang browser shutdown. Log: prts-m1-acceptance-extended.log and .err.log
under C:/src/chromium. No required or extended runtime test reported a failure.

The final combined run on the final Rust binary also passed every required and
extended case, without test skips, in 42s (04:44:57..04:45:39 local). Warm
startup was 0.57s and HELLO_ACK 354.20ms. Final logs are
`C:/src/chromium/prts-m1-acceptance-final.log` and `.err.log`.
Final cargo build, cargo check --all-targets, cargo clippy --all-targets --
-D warnings, cargo fmt -- --check and cargo test all passed: ten ordinary
tests, two hardware tests intentionally ignored; rate-control regression 87.90s.
No Chrome or browser_m1 process remained after completion.

To rerun from the infra root on a connected desktop:

```powershell
& "$env:USERPROFILE/.cargo/bin/cargo.exe" build --manifest-path prtsd/Cargo.toml --bin browser_m1
& ./prtsd/target/debug/browser_m1.exe
```

Only for a disconnected session, set
`$env:PRTS_M1_ALLOW_DISCONNECTED_DESKTOP='1'`; this intentionally leaves
foreground preservation UNVERIFIED. Optional `--extended-only` runs the
bootstrap/protocol/backpressure cases without a Notepad desktop baseline.

## Desktop qualification environment

The Flysoft session (2) is disconnected; console session 3 is connected.
GetForegroundWindow returns NULL, and a separately launched Notepad cannot
establish an interactive foreground baseline. Default acceptance correctly
fails this precondition. Explicit PRTS_M1_ALLOW_DISCONNECTED_DESKTOP=1 runs
remaining tests and prints foreground preservation UNVERIFIED. Reconnecting
session 2 is required for the complete focus acceptance; a user question was
left pending while independent work continued.

The desktop preflight actually enumerated Notepad and Explorer taskbar buttons
through UIAutomation. Baseline entries were Claude, Store, Explorer, Notepad;
preflight snapshots were unchanged. This preflight did not launch Chromium.
The actual browser run enumerated every browser-PID top-level HWND at startup,
after two targets, resize/activation, all-target closure, and allowed popup
discovery. It inspected IsWindowVisible, WS_VISIBLE, DWMWA_CLOAKED and styles.
With two targets it found seven HWNDs: both target HWNDs had visible=false,
DWMWA_CLOAKED=1, WS_STYLE=0x06CF0000 (WS_VISIBLE clear), WS_EXSTYLE=0x00200100;
the five auxiliary/IME HWNDs were hidden with cloak=0. Every snapshot had zero
visible/uncloaked windows and zero visible taskbar-eligible windows.
Actual Explorer UIAutomation task-list buttons remained identical to the
Notepad/Claude/Store/Explorer baseline at every checkpoint: no Chromium taskbar
entry appeared. Popup discovery also preserved this result.

Foreground was sampled every 10ms, but remained NULL in the disconnected
session; it is **UNVERIFIED**, not a pass. The test creates its own Notepad
sentinel and removes it afterwards. The test harness uses foreground APIs only
to try to establish this baseline; production browser code never injects OS
input or calls foreground activation APIs. Cloaked HWND creation is confirmed
and acceptable; literal no-HWND operation remains outside M1.

## Commit inventory and handoff

Chromium `work` commits (all carry the required coauthor trailer):

- 1d38a8c3f4: target registry and viewport control.
- a77f9b5111: bounded parsing and generation details.
- 28cc48d743: public browser creation API and safety checks.
- 34fb1841e6: tab replacement retains target identity.
- 84b3acd5c6: same-window tab activation observations.
- 0a2128d838: transport and protocol/controller interfaces.
- cfd4dc5266: bootstrap, controller and GN lifecycle integration.

Infra changes will be left staged for the orchestrator; no patch export was
performed. Complete foreground preservation needs a connected desktop rerun.
Input/cursor, frames/CPU sections, GPU textures, encoder integration, and
deployment isolation remain M2 through M5. CPU mode in HELLO_ACK advertises the
v1 fallback contract; M1 deliberately produces no frame resources/descriptors.
LUID zero is explicitly unavailable, not a claim of adapter qualification.
