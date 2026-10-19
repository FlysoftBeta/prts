# M1 Rust launcher handoff

Added `prtsd/src/browser_wire.rs`, `prtsd/src/browser.rs`, and
`prtsd/src/bin/browser_m1.rs`. Exported the modules from `src/lib.rs`; expanded
official `windows` crate feature flags in `Cargo.toml`. No synthetic source,
daemon media path, server, Chromium source, or patch directory changes here.
Infra changes are left uncommitted for the orchestrator.

`Browser::launch(DEFAULT_CHROME)` creates three synchronous anonymous pipes,
clears inheritance on the parent ends, and calls CreateProcessW using
STARTUPINFOEX/PROC_THREAD_ATTRIBUTE_HANDLE_LIST with exactly the three browser
ends. The returned process handle retains full access including
PROCESS_DUP_HANDLE. Parent copies of browser pipe ends close immediately.
Separate blocking event/frame readers and serialized writer keep the channels
independent. CPU-section HELLO offers v1.0, daemon PID, LUID zero, three credits.
HELLO_ACK and target snapshots are decoded positionally with bounds/UTF-8/enum
validation and exact payload consumption. Odd request IDs increase monotonically.
Public create/list/navigate/close/resize/activate methods preserve event ordering;
`drain_events()` returns ordered lifecycle events and `receive_event(timeout)`
provides independent event consumption. An idle worker sends PING
every five seconds. Request deadlines are five seconds; uncertain responses,
framing errors, EOF, and queue overflow fail and terminate the browser. Control
queues are bounded to 1024 messages and 8 MiB. Teardown terminates a remaining
peer and calls CancelSynchronousIo before joining the pipe workers. M1 accepts
no frame descriptors; the frame reader remains open and detects failure.

Windows component-build startup is bounded separately from the protocol
deadline: `WaitForInputIdle` precedes HELLO, using a 300-second startup limit
(`PRTS_BROWSER_STARTUP_TIMEOUT_SECS`, range 5..600). The HELLO_ACK request still
has its normative five-second deadline. Initial direct-HELLO probes failed
before browser logging initialized; cdb showed MainDllLoader loading component
DLLs through ntdll LdrpCodeAuthzCheckDllAllowedSrpV2/NtOpenKey. DLL enumeration
continued progressing, rather than showing a fixed controller deadlock. No
Windows policy/security settings were changed. `PRTS_BROWSER_LOG=<path>` adds
Chromium logging/VLOG switches and reports startup and HELLO_ACK timings.
`browser_m1.exe --raw-startup-probe` is explicitly diagnostic and sends HELLO
immediately with a separate long diagnostic wait; its timing is not used to
claim the five-second protocol deadline passed.

Run after patched chrome is built:

```powershell
cd C:\Users\Flysoft\Desktop\chromium\prtsd
& C:\Users\Flysoft\.cargo\bin\cargo.exe build --bin browser_m1
.\target\debug\browser_m1.exe
```

The driver verifies no initial targets; two independently sized targets;
create/list/resize/activation and generation changes; navigation/title/load;
actual page-reported CSS dimensions (640x480 and 960x540, then 800x600) and
per-target DPR 2 using a document title updated from innerWidth, innerHeight
and devicePixelRatio on load/resize;
event-before-response mutation ordering; active-target replacement on close;
last-window keep-alive and idle heartbeat; SHUTDOWN->OK and process exit;
fragmented successful HELLO; truncated record with EOF; post-handshake EOF;
and actual launcher process death without executing Rust destructors.
Additional runtime cases cover missing/duplicate/invalid/overflowed handle
switches, renderer/GPU process rejection, old headless mode rejection; unknown
wire type, wrong envelope version, nonzero flags, even/reused/decreasing IDs,
oversized/undersized prefixes, wrong direction, absent HELLO deadline,
nonoverlapping version ERROR then EOF, and a deliberately stalled event reader.
An isolated popup qualification uses the explicit test-only switch
`--disable-popup-blocking`, then requires CREATED(kind=2, opener=<opener ID>)
without activation theft. Default launcher policy remains unchanged. These new
cases require the parent's patched Chromium runtime; the ordinary cargo tests
do not pretend to cover browser behavior.
`PRTS_M1_SKIP_EXTENDED=1` skips the additional malformed/bootstrap/stalled-reader
cases when exceptionally slow component startup makes exhaustive launches
impractical; core target/viewport/desktop/partial-EOF/peer-death/popup cases
remain enabled. The full default runs all cases.

Desktop checks launch a temporary Notepad document, establish its foreground
window, sample GetForegroundWindow every 10 ms, enumerate browser-owned
top-level HWNDs and log IsWindowVisible/DWMWA_CLOAKED/window styles. Actual
Explorer taskbar UIAutomation task-list buttons are snapshotted before startup
and after target creation/activation/closure; new or changed buttons fail.
This verifies cloaked/nonactivating HWND behavior, not literal absence of HWNDs.

Environment limitation discovered during preflight: `query session` reports
Flysoft session 2 **Disc**, console session 3 **Conn**. GetForegroundWindow
returns NULL; even temporary test-harness AttachThreadInput and
SetForegroundWindow cannot establish an interactive foreground baseline.
The default acceptance invocation fails explicitly in this situation.
`$env:PRTS_M1_ALLOW_DISCONNECTED_DESKTOP='1'` allows the remaining checks while
printing foreground verification **UNVERIFIED**. It does not claim focus passed.
Interactive foreground preservation must be rerun after reconnecting session 2.

The standalone `browser_m1.exe --desktop-probe` preflight was actually run:
default mode correctly rejected missing foreground; explicit disconnected mode
passed HWND enumeration and Explorer taskbar automation. Explorer reported
Taskbar.TaskListButtonAutomationPeer entries for Claude, Microsoft Store,
Explorer, and the test Notepad, unchanged across the preflight. Temporary
Notepad windows were closed afterwards. This preflight did not launch Chromium.

Validation completed: cargo build; cargo test (10 ordinary tests passed,
2 hardware tests intentionally ignored); cargo check --all-targets;
cargo clippy --all-targets -- -D warnings; cargo fmt. Existing synthetic WebRTC,
rate-control and EOF integration tests still pass. Chromium runtime acceptance
and build results are owned by the parent agent and recorded in phase2-m1.md.

First full browser run reached startup input-idle after **171.96 seconds**;
HELLO_ACK arrived **371.51 ms** after HELLO, within the separate five-second
protocol deadline. Initial zero-target list, two targets, page-reported
640x480@1 and 960x540@1, resize to 800x600@1, CREATED/CHANGED ordering and
Explorer taskbar/hidden HWND checks passed before a test-observation issue:
DPR-only emulation changes need not emit window.resize when CSS dimensions
stay fixed. The viewport test now refreshes its title every100 ms as well as
on resize. The first driver was deliberately terminated; its browser exited
on pipe EOF within two seconds without independently killing Chrome. A new
required-case run includes the fixed DPR observation and fresh-target creation
after last-window closure. Its logs are
`C:/src/chromium/prts-m1-acceptance-required.log` and `.err.log`.

Actual first-run browser HWND results: ordinary target windows had
`IsWindowVisible=false`, `DWMWA_CLOAKED=1`, style0x06CF0000 and
extended style0x00200100. Several auxiliary browser/IME HWNDs existed with
visibility false and cloak0. No visible/uncloaked top-level window appeared.
Explorer UIAutomation task-list buttons matched the prelaunch snapshot exactly;
there was no Chromium taskbar entry. Foreground verification remains explicitly
UNVERIFIED because this Windows session is disconnected.

The fixed **required suite completed successfully** in approximately21 seconds
(`acceptance-required.log`, 04:42:26..04:42:47 local). Warm startup input-idle
was0.50s and HELLO_ACK320.29ms. Both actual CSS viewport sizes, resize, DPR2,
activation, navigation/title/load, event-before-response ordering, closing the
active target, last-window survival/heartbeat, new-target recreation with a
fresh monotonic ID, SHUTDOWN->OK/process exit, fragmented HELLO, truncated EOF,
post-handshake EOF, real abrupt launcher process death, and allowed popup
kind/opener/no-logical-activation-theft checks passed. Popup qualification used
the explicit test-only popup-policy override; default launcher policy is intact.
Hidden/cloaked HWND and actual Explorer taskbar checks passed at each stage,
including popup creation. Foreground remains UNVERIFIED as stated above.

The separate **extended suite completed successfully** in approximately22
seconds (`acceptance-extended.log`, 04:43:20..04:43:42 local): all bootstrap,
framing, magic/version/flags/types/ID/direction checks; version no-overlap
ERROR then exit; missing-HELLO deadline; and stalled event-reader pressure with
20,000 PING requests all passed. No browser or acceptance process remained
after either suite. The final binary has wording that explicitly separates
visibility/cloaking/taskbar success from the separately reported foreground
status.

Final combined runtime on the final rebuilt Rust source passed all required
and extended cases in one invocation, with no cases skipped:
`C:/src/chromium/prts-m1-acceptance-final.log` and `.err.log`. The concluding
line explicitly says foreground status is reported separately. No Chrome or
acceptance process remained. Final validation after source completion passed
cargo build, cargo check --all-targets, cargo clippy --all-targets -- -D warnings,
cargo fmt -- --check and the full cargo test suite (10 ordinary tests passed;
2 opt-in hardware tests ignored; rate-control regression87.90s).
