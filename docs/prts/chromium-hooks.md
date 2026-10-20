# Chromium hook points for PRTS

**M5 CHANGE (2026-10-20):** the old page-surface/content-input recommendation
below is **SUPERSEDED** by whole-browser-window capture/input. The M5 design is
[phase2-m5-design.md](phase2-m5-design.md); the window protocol is the proposed
v2 section in [protocol.md](protocol.md#m5-proposed-v20-window-contract-design-not-implemented).
M1-M4 behavior is historical/implemented; the M5 hooks in the new section below
are source-validated proposals, not an implementation or runtime qualification.

Validated on 2026-10-19 by targeted `rg` searches and reading the local checkout
at `C:/src/chromium/src`. `chrome/VERSION` is **158.0.8111.0**;
HEAD is `caef9db47e2cf3fe8e9f07ddceeff6ddf6957385`, matching this repo's `PIN`.
All Chromium paths below are relative to that source root. Line numbers are
approximate anchors actually inspected in this revision, not build validation.
No Chromium build, runtime experiment, source modification, or commit was made.
The optional investigation repository was not used in the original Phase 0
survey. M5 thoroughly reviewed it (all four patches and documents), cloned at
`%TEMP%/prts/coi`, HEAD `7476579f2ce0413c2708068a590c60d0b5983023`, and maps its
hooks/patches to adopt/adapt/reject decisions in the M5 design.

## Historical Phase 0 recommendation (page/input scope superseded by M5)

Put browser integration in **`chrome/browser/prts/`**, not `headless/lib/`.
Use Chrome's ordinary headless windows/tabs and a UI-sequence controller, with
dedicated anonymous-pipe reader/writer threads. Capture each page's Viz frame
sink through `viz::ClientFrameSinkVideoCapturer`. First deliver CPU frames;
then select **ARGB or NV12 + `kPreferMappableSharedImage`**, receive the Windows
DXGI NT handle in the browser, and duplicate it into prtsd. Keep the capture
lease until prtsd finishes reading the texture. This reuses the existing GPU
allocation, conversion, capture pacing, and GPU-to-browser Mojo transport.

This is zero **CPU pixel transfer**, not zero GPU operations: Viz blits/scales
the composed surface into a pooled texture, and NV12 adds GPU color conversion.
Whether QSV can consume that very allocation without another GPU copy remains
a runtime/encoder integration question. Do not promise direct encoder import
from source inspection alone.

Two revision-specific traps:

* `kPreferSharedImageWithNativeHandle` exists, but the capturer's texture-result
  selection enables it only under `IS_LINUX`. NV12 explicitly CHECKs that the
  preference is `kPreferMappableSharedImage`. Do not choose the seemingly more
  appropriate native-handle enum on Windows.
* Windows unified headless creates hidden/cloaked HWNDs. It is not the old
  headless-shell offscreen embedder. No visible windows, desktop input, or DWM
  capture is needed, but a literal requirement of **no HWND creation** would
  need additional platform work and is not satisfied by stock `--headless=new`.

## 1. Startup, ownership, and sequences

| Source anchor | Observed behavior / hook |
| --- | --- |
| `chrome/app/chrome_main.cc:200–214`, Chrome main entry | Checks `headless::IsHeadlessMode()`, rejects more than one positional URL, calls `InitHeadlessMode()`, retains its handle. Multiple targets must be created after startup, not passed as multiple URLs. |
| `chrome/browser/headless/headless_mode_util.cc:21`, `IsHeadlessMode()` | Tests presence of the `headless` switch; no separate `new` implementation selector here. `--headless=new` follows unified Chrome startup. |
| `chrome/browser/headless/headless_mode_init.cc:40–120`, `HeadlessModeHandleImpl::Initialize/SetUpCommandLine` | Configures unattended operation, incognito default and temporary profile directory. Only the Linux block forces Ozone headless and its ANGLE defaults. This is too early for a WebContents/controller service. |
| `chrome/browser/chrome_browser_main.cc:850–853` | Registers `ChromeBrowserMainExtraPartsHeadless`. |
| Same file `:1514–1535`, `PreMainMessageLoopRun()` | Documents profile initialization, `PreBrowserStart`, browser startup, then `PostBrowserStart`; invokes extra-parts `PreMainMessageLoopRun()` afterward. |
| Same file `:2163`, `:2225–2256` | `PostBrowserStart()` is after browser startup; `PostMainMessageLoopRun()` invokes extra-parts shutdown. |
| `chrome/browser/headless/chrome_browser_main_extra_parts_headless.cc:17`, `PreBrowserStart()` | Existing small browser-lifecycle integration, currently installs the headless clipboard. Suitable delegation point for PRTS preparation. |

Suggested new `--prts-pipe-read-handle`, `--prts-pipe-write-handle`, and an
inherited prtsd process-handle switch are **proposed switches**, not upstream
features. Gate initialization on Windows, headless mode, those switches, and
an empty process type. prtsd creates two anonymous pipes and explicitly inherits
only the browser ends plus a process handle with `PROCESS_DUP_HANDLE` access
to itself. Clear inheritance in the browser after adopting these handles so
renderer/GPU children do not accidentally keep the pipes alive. Do not overload
Chrome's stdout or DevTools remote-debugging pipe; prtsd's Node stdio protocol
is a separate connection. Validate numeric handle arguments and ownership.

Prepare observation in `PreBrowserStart` if startup-target events must be caught;
start accepting commands in a new `PostBrowserStart()` override, with an initial
snapshot to cover already-created windows/tabs. Own a `PrtsController` on the
browser UI sequence. Stop it in a new `PostMainMessageLoopRun()` override;
detach observers and stop captures before profile/browser services disappear.
If last-window closure must leave the instance controllable, explicitly own a
browser/profile keep-alive and release it on the PRTS shutdown command.

Sequence design:

* UI: BrowserWindowInterface/Browser, tab strips, WebContents observers,
  navigation, focus, input, and HostFrameSinkManager calls.
* Dedicated pipe reader: blocking `ReadFile` loop, bounded framing/validation,
  posts decoded commands to UI using weak lifetime references.
* Dedicated writer: serialized `WriteFile` loop and bounded queue. Never block
  UI or the Chromium IO thread behind a slow encoder/pipe peer. Preserve control
  replies and releases; drop unsent video frames under backpressure.
* Initially keep the capture consumer and its Mojo remotes on UI too; this
  simplifies ownership. Handle duplication/pipe writes can run on the writer
  after transferring owned handles and immutable metadata. Post frame release
  back to the capture sequence. Later a separate capture sequence is possible;
  `ClientFrameSinkVideoCapturer` is sequence-bound.

Existing reference: `content/browser/devtools/devtools_pipe_handler.cc:56–95`
(`PipeIOBase`), `:135–219` (`PipeReaderBase`, Windows `ReadFile`, posts to UI),
and `:223–284` (`PipeWriterBase`, `WriteFile`). It uses separate threads with
IO message pumps and asynchronous shutdown rather than blocking browser IO.
Its fd/CRT setup and wire framing are DevTools-specific; reuse the architecture,
not its protocol. Design cancellation of synchronous pipe I/O before joining
threads; peer death and a stalled writer must not hang browser shutdown.

## 2. Targets, tabs, popups, size, and DPR

Unified headless uses Chrome browser UI objects. Prefer the current
`BrowserWindowInterface` APIs around the legacy `Browser` class. In particular,
this revision's `Browser::Create` takes **`BrowserWindowCreateParams`**, not the
older `Browser::CreateParams` (`chrome/browser/ui/browser.cc:231`).
`chrome/browser/ui/browser_window/public/create_browser_window.h:36–80,251–261`
declares the creation parameters and sync/async `CreateBrowserWindow` APIs.

The smallest creation implementation can follow
`chrome/browser/devtools/protocol/target_handler.cc:35–54,95–290`:
`CreateNavigateParams` sets `NEW_WINDOW` / `NEW_FOREGROUND_TAB` /
`NEW_BACKGROUND_TAB`; `TargetHandler::CreateTarget` resolves the profile and
browser, calls `Navigate(&params)` at about line 228, checks
`params.navigated_or_inserted_contents`, adjusts the window bounds, optionally
focuses the WebContents, and returns a DevTools target ID. This is a reference
for an internal C++ controller; no TCP DevTools server is required.

For existing targets, call their navigation controller's `LoadURL`/load-params
API on UI (`content/public/browser/navigation_controller.h:562` declares
`NavigationController::LoadURL`). Use normal Chrome tab insertion and closure semantics:
`chrome/browser/ui/tabs/tab_strip_model.h:269,288,337,599` declares
`AppendWebContents`, `InsertWebContentsAt`, `CloseWebContentsAt`, and
`AddWebContents`. Choose the ordinary close path so beforeunload handling remains
defined; report close-requested separately from destroyed. As another closure
reference, `content/browser/devtools/protocol/target_handler.cc:1297–1315`
resolves `Target.closeTarget` to `DevToolsAgentHost::Close()`;
`content/browser/devtools/web_contents_devtools_agent_host.cc:316` implements
the WebContents agent's `Close()` forwarding.

Observe all managed windows and tabs instead of replacing a WebContents delegate:

* `chrome/browser/ui/browser_window/public/global_browser_collection.h:27–36`
  provides `GlobalBrowserCollection::GetInstance()`; its base
  `browser_collection.h:54` has `ForEach`. `browser_collection_observer.h:16–19`
  has `OnBrowserCreated/Closed/Activated/Deactivated`.
* `chrome/browser/ui/tabs/tab_strip_model_observer.h:525` exposes
  `OnTabStripModelChanged`. Attach observers and enumerate each existing strip,
  deduplicating by WebContents identity. Handle insertion, removal, replacement,
  and moves between windows. A moved tab retains its PRTS ID.
* `content/public/browser/web_contents_observer.h:392,421–422,640,715,778`
  exposes `DidFinishNavigation`, `DidStartLoading`, `DidStopLoading`,
  `DidOpenRequestedURL`, `TitleWasSet`, and `WebContentsDestroyed`. Filter URL
  reporting to primary main-frame committed navigations, including same-document
  URL changes. Report load state independently from navigation success/failure.
  Read current title/URL for the initial snapshot. Do not treat every frame's
  navigation as a target navigation.

Assign a monotonically allocated PRTS target ID to each WebContents; retain an
opener ID and window ID separately. Never expose a raw pointer/routing ID as a
durable wire identity. Renderer process swaps should preserve the PRTS ID.
Reject input/commands for destroyed targets or stale instance generations.

Popups and `window.open` already pass through
`chrome/browser/ui/browser_web_contents_delegate/browser_web_contents_delegate.cc:661`,
`BrowserWebContentsDelegate::AddNewContents`; lines 691–695 explain that this
stage is beyond popup blocking and install `PopupTracker` where appropriate.
`WebContentsCreated` is in the same file at about 1057. Discover allowed popup
contents through window/tab observations, then attach PRTS observers/capture.
`DidOpenRequestedURL` provides additional relationship information, not a
universal replacement for tab discovery. Do not replace Chrome's delegate or
disable its popup blocker just to get notifications. If a popup policy or exact
creation-time opener metadata is needed, add a small observer notification at
`AddNewContents`/`WebContentsCreated`; that is an optional extra hook.

For independent sizes, initially use **one logical target per headless window**.
Tabs sharing a window share its layout bounds, and hidden/background tabs need
capture visibility management. Expose multi-tab bookkeeping, but do not claim
independent window bounds for tabs in the same strip.

* `chrome/browser/devtools/protocol/browser_handler.cc:200,251` uses
  `ui::BaseWindow::SetBounds`; `TargetHandler::CreateTarget:249` does likewise.
* `ui/views/win/hwnd_message_handler_headless.cc:193` (`SetSize`), `:517`
  (`SetBoundsInternal`) maintains synthetic bounds and notifies the delegate.
  `ui/views/widget/desktop_aura/desktop_window_tree_host_win.cc:1579` stores
  `aura::client::kHeadlessBoundsKey` in `HandleHeadlessWindowBoundsChanged`.
* Outer window bounds, CSS viewport size, physical capture size, and device scale
  factor are distinct. Browser chrome/client insets can affect the viewport;
  verify `innerWidth/innerHeight` and captured dimensions after every resize.
* Per-target DPR should follow device emulation, not a global screen scale
  switch. `content/browser/devtools/protocol/emulation_handler.cc:676–852`,
  `EmulationHandler::SetDeviceMetricsOverride`, builds emulation parameters
  including `device_scale_factor` and sets a device-emulation size.
  `SetVisibleSize:910–916` changes size alone; `UpdateDeviceEmulationState`
  calls frame-widget `EnableDeviceEmulation/DisableDeviceEmulation` around
  1180–1183. An internal DevTools session is a low-patch way to reuse these
  semantics; otherwise isolate the version-sensitive C++ equivalent in the
  PRTS adapter. Reapply after renderer/widget changes.

## 3. Input and cursor

Input runs on UI and addresses the current target's widget, never `SendInput`,
`PostMessage`, `SetForegroundWindow`, or physical desktop coordinates.

`content/browser/renderer_host/render_widget_host_impl.cc` implements:

| Function | Approximate line | Purpose |
| --- | --- | --- |
| `RenderWidgetHostImpl::ForwardMouseEvent` | 1697 | Accepts `blink::WebMouseEvent`; forwards with latency info. |
| `ForwardWheelEvent` | 1775 | Accepts `blink::WebMouseWheelEvent`. |
| `ForwardKeyboardEvent` | 1835 | Accepts `input::NativeWebKeyboardEvent`. |
| `ForwardKeyboardEventWithCommands` | 1848 | Supports associated edit commands. |
| `SetPageFocus` | 1519 | Renderer page focus machinery; use normal target/WebContents focus paths first. |

Treat `content/browser/devtools/protocol/input_handler.cc` as the correctness
reference, not just a list of forwarding calls:

* `InputHandler::DispatchKeyEvent:1186–1278` constructs a
  `input::NativeWebKeyboardEvent` without a native OS event, fills text,
  unmodified text, virtual/native key codes and DOM code/key. It resolves the
  focused widget through `GetFocusedRenderWidgetHost` for focused subframes.
  `skip_if_unhandled = true` at about 1275 avoids browser accelerator fallback
  for synthetic keys. Define key-down/up and character delivery separately to
  avoid duplicate text; IME/text insertion needs a dedicated command, following
  `InputHandler::InsertText:1281` rather than inventing OS keystrokes.
* `InputInjector::InjectMouseEvent:739–765`, `InjectWheelEvent:704–736`, and
  `InjectKeyboardEvent:768–797` call `Focus()` then forward, track acknowledgments,
  and guard against synchronous destruction. Wheel injection adds a phase-ended
  event. Mirror phases, modifiers, button state, click count, and timestamps.
* `InputHandler::HandleMouseEvent:1395–1433` asynchronously hit-tests via
  `GetRenderWidgetHostAtPointAsynchronously`. `OnWidgetForDispatchMouseEvent:1629–1650`
  converts to target widget and screen positions before forwarding. This matters
  for OOPIFs: blindly forwarding all coordinates to the main widget is incomplete.
  Wheel dispatch also inserts a visual-state callback before hit testing.

Define the protocol's pointer coordinates in the target's viewport CSS space;
convert from video pixels using content rectangle, crop/letterboxing, DPR,
emulation and page scale. Follow InputHandler's transformations rather than
applying DPR twice. Keep logical active target separate from capture selection.
Resolve widget pointers again after navigation; pending asynchronous callbacks
must tolerate destroyed targets. Dragging, pointer lock, IME, browser dialogs,
and HTML select popup widgets need explicit follow-up tests/policies.

Cursor changes do not have a ready WebContentsObserver callback here. The
requested `RenderWidgetHostViewBase::UpdateCursor` is not the concrete Windows
implementation: the interface is inherited from
`components/input/render_widget_host_view_input.h:229`, and Windows uses Aura:

1. `content/browser/renderer_host/render_widget_host_impl.cc:2257` invokes the
   view's `UpdateCursor`.
2. `content/browser/renderer_host/render_widget_host_view_aura.cc:1043–1050`,
   `RenderWidgetHostViewAura::UpdateCursor/DisplayCursor`, routes through
   `input::CursorManager`, then stores/displays the selected cursor.
3. `components/input/cursor_manager.cc:122–132`, `CursorManager::UpdateCursor`,
   calls the root view's `DisplayCursor`, selecting the cursor for the view
   under the pointer. Its header describes cross-widget/OOPIF aggregation.

Add an opt-in generic cursor observer at the root `DisplayCursor` path, bridged
to PRTS from browser code; do not introduce a content-to-Chrome dependency.
Send cursor type, hotspot and optional custom bitmap/scale over the pipe and
render locally in the browser client. Ensure synthetic mouse routing updates
the view-under-cursor bookkeeping; direct forwarding can bypass that step
(`components/input/render_widget_host_input_event_router.cc:731,770` updates
`CursorManager::UpdateViewUnderCursor`). Exporting every child widget's cursor
without choosing the hovered widget would produce wrong cursor changes.

## 4. Capture choices, ranked

### First: Viz frame-sink video capture with native Windows GMBs

Browser entry points are already present:
`components/viz/host/host_frame_sink_manager.h:247–257` has both
`HostFrameSinkManager::CreateVideoCapturer` overloads; prefer the one returning
`ClientFrameSinkVideoCapturer`, documented as resilient to Viz crashes.
`components/viz/host/client_frame_sink_video_capturer.h:30–38,90–130` documents
single-sequence use and exposes format/rate/size/target/start controls.
`content/browser/media/capture/frame_sink_video_capture_device.cc:587–603`
demonstrates hopping to UI to call `content::GetHostFrameSinkManager()`.

**SUPERSEDED BY M5:** the former decision was "Target the page's surface, not
the root browser window, to omit browser chrome." M5 instead targets the
browser window's root `ui::Compositor::frame_sink_id()` and composites separate
popup-host roots. The following WebContents tracking remains a record of M3/M4,
not the new capture-target choice.
`content/browser/web_contents/web_contents_impl.cc:12308–12324`,
`WebContentsImpl::GetCaptureTarget()`, resolves the current view's frame-sink ID.
`content/browser/media/capture/web_contents_frame_tracker.cc:349–383,471–502`
shows retargeting on frame-host/view/visibility/capture-target changes. Its
context at 149–156 calls `GetCaptureTarget` and retains the
`WebContents::IncrementCapturerCount` closure. That API is documented at
`content/public/browser/web_contents.h:891–929`; `stay_hidden=false` keeps
renderer visibility active while capturing, without showing a native window.
Replicate that lifetime and retargeting in a narrow adapter, including
`CaptureTargetChanged`, or reuse the existing tracker where practical. Internal
content includes need explicit GN/DEPS handling; isolate them in this adapter.

Validated GPU data chain:

| Source | Evidence |
| --- | --- |
| `services/viz/privileged/mojom/compositing/frame_sink_video_capture.mojom:17–61,106–117` | Per-frame `Done()` lease, `OnFrameCaptured(data, info, content_rect, callbacks)`, and buffer preferences. |
| `media/capture/mojom/video_capture_buffer.mojom:16–41,52–65` | Frame geometry/color metadata and `VideoBufferHandle` union, including `gpu_memory_buffer_handle` and a separate exported-SharedImage/sync-token alternative. |
| `components/viz/service/frame_sinks/video_capture/frame_sink_video_capturer_impl.cc:104–138,285–318,437–480` | Pool selection supports ARGB/NV12; NV12 requires a pool context and `kPreferMappableSharedImage`. |
| Same file `:1161–1188,1215–1241` | Windows selects texture results for mappable preference; creates a `BlitRequest` into the pooled SharedImage and `CopyOutputRequest` with `kSharedImage`. The native-handle preference extension is Linux-only. |
| `components/viz/service/frame_sinks/video_capture/mappable_shared_image_video_frame_pool.cc:65–74,82–98` | `CloneHandleForDelivery` returns `frame.GetGpuMemoryBufferHandle()` in the union; the pool's CPU-access flag does not imply that PRTS must map it. |
| `media/gfx/renderable_mappable_shared_image_video_frame_pool.cc:181–205,240–286` | Windows uses `SCANOUT_CPU_READ_WRITE`, creates a renderable SharedImage and wraps it in a VideoFrame. NV12 coded sizes round width to 4 and height to 2; preserve coded versus visible geometry. |
| `components/viz/service/frame_sinks/gmb_video_frame_pool_context_provider_impl.cc:65–83,115–134` | Creates SharedImages with the in-process interface, configured with `always_create_native_gmb_handle=true`. |
| `gpu/command_buffer/service/shared_image_interface_in_process.cc:184–215` | That setting calls `CreateNativeGpuMemoryBufferHandle` and imports the result. |
| `gpu/command_buffer/service/shared_image/shared_image_factory.cc:810–823` | Windows native allocation delegates to `D3DImageBackingFactory::CreateGpuMemoryBufferHandle`. |
| `gpu/command_buffer/service/shared_image/d3d_image_backing_factory.cc:351–419` | Allocates `D3D11_USAGE_DEFAULT` texture with shader/render-target binds, zero CPU access flags, `SHARED_NTHANDLE | SHARED_KEYEDMUTEX`; calls `IDXGIResource1::CreateSharedHandle` and returns `gfx::DXGIHandle`. |
| `ui/gfx/gpu_memory_buffer_handle.h:62–117,183–190` | `DXGIHandle` owns the Windows handle and a stable token; `Clone()` duplicates it. `GpuMemoryBufferHandle::dxgi_handle()` requires `DXGI_SHARED_HANDLE`. |
| `ui/gfx/mojom/native_handle_types_mojom_traits.h:114–128`, corresponding `.cc:149–162` | Serializes the DXGI resource through a Mojo platform handle; deserialization reconstructs a locally owned `gfx::DXGIHandle`. |

ARGB is a media format name: comments in the capturer at 85–100 explain the
RGBA/BGRA distinction. Mappable ARGB requests BGRA; use delivered format and
actual texture DXGI format, not an assumed channel order. NV12's conversion is
visible in `SkiaOutputSurfaceImplOnGpu::CopyOutputNV12`,
`components/viz/service/display_embedder/skia_output_surface_impl_on_gpu.cc:1352–1487`,
including `skia::BlitRGBAToYUVA`. Hardware/driver renderability still needs testing.

#### Exact implemented GPU ? browser ? prtsd handoff (M4)

This supersedes the original Phase 0 direct-lease/key-0 proposal. See
`phase2-m4.md` and normative `protocol.md` for the implemented contract.

1. Viz captures ARGB using `kPreferMappableSharedImage`, delivers a native BGRA
   DXGI GMB after GPU completion, and retains its lease until `Done()`.
2. The browser opens that local NT handle on the adapter matching both Chromium's
   active hardware GPU and HELLO's LUID. It acquires Viz's existing key-0 mutex.
3. It GPU-copies the visible image into a PRTS-owned shared NT/keyed texture
   acquired at key 0, waits for completion, releases the source at key 0 and
   export at key 1, then ends the Viz lease. The export pool uses M3's two-credit
   bound, registration/unregistration, retirement and passive refresh behavior.
4. prtsd duplicates the browser-local export handle through its retained browser
   process handle (`DuplicateHandle`, SAME_ACCESS, noninheritable). No prtsd
   process handle is inherited and no GPU-process handle value crosses the pipe.
5. It validates metadata and the actual texture descriptor and opens with
   `OpenSharedResource1` on the negotiated hardware adapter, acquires key 1,
   copies to a private GPU BGRA surface, waits for reads to finish, releases at
   key 0 and sends FRAME_RELEASE. Even coalesced/dropped frames perform this
   transition; private copies can be discarded independently.
6. QSV converts BGRA to private NV12 using the D3D11 video processor and submits
   an MF DXGI surface buffer/device manager. Software uses staging BGRA readback
   plus CPU I420 conversion, explicitly reported as `staging`. Import rejection
   before acquisition triggers sticky CPU fallback and a new target generation.

No upstream SharedImage mutex convention changes. GPU copies occur but no CPU
pixel readback occurs in the QSV texture path. The completion waits currently
run on the browser UI sequence (bounded) and serialized daemon/encoder callers;
an asynchronous capture worker would improve responsiveness under driver stalls.

#### Frame pacing, damage, and visibility

Set format before `Start`; request a minimum capture period (e.g. 1/30 s), fixed
resolution constraints initially, and a deliberate auto-throttling policy.
The defaults are 5 FPS and 640×360, per the Mojo interface around lines 141–183.
`FrameSinkVideoCapturerImpl::SetMinCapturePeriod:330–354` configures the oracle;
`OnFrameDamaged:710` vicinity feeds damage/timing into capture selection.
Metadata at 969–1037 includes frame duration, compositor sequence, scale and
capture update rectangle. Use it for telemetry and change detection, but each
delivered frame must remain a complete independently consumable texture. The
TODOs at 1183–1184 and 1233–1237 explicitly say capture is currently whole-frame,
not a patch of changed pixels. Static content need not produce constant frames;
`RequestRefreshFrame()` exists for initial display/recovery.

Use ordinary compositor BeginFrames initially. Keep capture visibility leases
alive for background targets and test occlusion/throttling on Windows.
`headless/lib/browser/protocol/headless_handler.cc:125–190` implements manual
`HeadlessHandler::BeginFrame` against headless-shell contents and requires its
BeginFrameControl mode. It is not a drop-in unified-Chrome hook, even though
Chrome's `TargetHandler::CreateTarget` signature has an optional
`enable_begin_frame_control` argument. A future deterministic mode would need
separate external BeginFrameController plumbing. Rate limiting capture is not
the same as driving rendering, and neither should wait for pipe writes.

### Second: custom GPU output device

This can avoid the capture blit if it renders directly into an exportable ring,
but has substantially more platform/backend and lifecycle coupling. It captures
a root display, potentially browser chrome, rather than naturally identifying
page surfaces. It needs a GPU→browser control interface, backpressure, resize,
presentation acknowledgment, texture lifetime and external synchronization.

What Windows unified headless actually selects is determined by surface handle
and graphics backend, not by the word “headless”:

* `ui/views/win/hwnd_message_handler.cc:427–433` chooses
  `HWNDMessageHandlerHeadless` when the screen is headless. Its `Init`,
  `ui/views/win/hwnd_message_handler_headless.cc:54–80`, creates a native window
  and applies `DWMWA_CLOAK`; `Show:233` updates synthetic visibility;
  `Activate:383` updates delegate state without foreground activation.
* `DesktopWindowTreeHostWin` still creates a compositor at
  `ui/views/widget/desktop_aura/desktop_window_tree_host_win.cc:303–304` and
  returns the HWND as its accelerated widget at 793–794.
* `SkiaOutputSurfaceDependencyImpl::IsOffscreen`,
  `components/viz/service/display_embedder/skia_output_surface_dependency_impl.cc:94–95`,
  tests whether the surface handle is null. A hidden/cloaked HWND does not by
  itself make that test true.
* `SkiaOutputSurfaceImplOnGpu::InitializeForGL:1970–2031` selects
  `SkiaOutputDeviceOffscreen` for a null surface; otherwise, on Windows with a
  presenter, `SkiaOutputDeviceDComp`, or `SkiaOutputDeviceGL` when no presenter
  is available (including DComp disabled). `InitializeForDawn:2140–2185` has
  corresponding offscreen, DComp and Dawn output-device branches. These are
  source-level alternatives; the active machine/backend was not runtime-probed.
* `components/viz/service/display_embedder/skia_output_device_offscreen.cc:72–98`
  (`Reshape/Present`) manages its own backbuffer and swap acknowledgments; it
  does not already expose a Windows texture export API.
* `components/viz/service/display_embedder/output_surface_provider_impl.cc:142–150`
  uses its `headless_` flag for the **software** output-device choice. This
  does not establish that GPU output is offscreen.

Linux's Ozone headless setup in `headless_mode_init.cc:97–120` is Linux-only;
patching `ui/ozone/platform/headless` is not the Windows solution.
The recommended page-surface capture obtains pixels inside Viz before desktop
composition and never samples DWM. Stock Chrome may still present its hidden
root through DComp. If “no DWM” also means removing that presentation entirely,
add a separate PRTS-only null-root-surface/offscreen experiment with scheduler
and overlay tests; do not silently equate that with the capture requirement.

### Third: explicit CPU readback fallback

Use `RenderWidgetHostView::CopyFromSurface` for the first smoke test:
`content/public/browser/render_widget_host_view.h:252–256` declares it;
`RenderWidgetHostViewAura::CopyFromSurfaceImpl` is at
`content/browser/renderer_host/render_widget_host_view_aura.cc:1201`.
`DelegatedFrameHost::CopyFromCompositingSurface`,
`content/browser/renderer_host/delegated_frame_host.cc:153–180`, constructs an
RGBA `CopyOutputRequest` with `kSystemMemory`, returning bitmap data.
This is GPU readback, not desktop capture. Keep only one request in flight,
copy bytes out of callback-owned results, and send actual stride/format metadata.

For continuous M3 video, prefer the same video-capturer adapter with I420 or
ARGB + `kDefault`, which uses shared memory rather than inventing a second
pacing system. The bitmap CopyFromSurface API is a CPU interface; the existence
of `CopyOutputRequest::kSharedImage` does not make its returned bitmap a GPU
handle. CPU readback is an explicit fallback and does not meet the final fast
path requirement.

## 5. Minimal patch set and implementation order

### New files (proposed, not created in Chromium by this task)

| File/group | Responsibility |
| --- | --- |
| `chrome/browser/prts/prts_switches.h` | Windows browser-only bootstrap switch names. |
| `chrome/browser/prts/prts_controller.{h,cc}` | Lifecycle owner, UI command dispatch, keep-alive and instance state. |
| `chrome/browser/prts/prts_pipe_client_win.{h,cc}` | Adopt inherited handles; bounded framing; dedicated read/write threads and cancellation. |
| `chrome/browser/prts/prts_target_registry.{h,cc}` | Browser/tab/WebContents observation, stable IDs, events, opener metadata. |
| `chrome/browser/prts/prts_input.{h,cc}` | Hit testing, coordinate conversion, focus, synthetic mouse/wheel/keyboard. |
| `chrome/browser/prts/prts_capture.{h,cc}` | ClientFrameSinkVideoCapturer consumer, retargeting/visibility leases, formats and pacing. |
| `chrome/browser/prts/prts_texture_export_win.{h,cc}` | Browser-local to prtsd NT handle duplication; per-frame lease/release accounting. |
| `chrome/browser/prts/BUILD.gn` | Transport source set and a `.gni` source list or carefully layered integration target. |
| `chrome/browser/prts/*_browsertest.cc` | Control/input/capture regression cases added as implementation proceeds. |

### Existing files to touch

| File | Small change |
| --- | --- |
| `chrome/browser/headless/chrome_browser_main_extra_parts_headless.h` | Own PRTS controller and declare startup/shutdown overrides. |
| Corresponding `.cc` | Delegate PRTS preparation/start/stop under Windows and explicit switches. |
| `chrome/browser/headless/BUILD.gn:92–105` | Wire lifecycle header dependencies or move extra-parts compilation into the integration owner to avoid a GN cycle. |
| `chrome/browser/BUILD.gn:999` (`:core`) | Include PRTS browser implementation sources/dependencies under `is_win`; this is already the owner of Chrome main code and depends on `headless_extra_parts` at 1554. |
| `content/browser/renderer_host/render_widget_host_view_aura.cc:1047` plus appropriate generic observer declarations | Optional opt-in cursor notification at selected root `DisplayCursor`, consumed by browser PRTS code. |
| `chrome/browser/ui/browser_web_contents_delegate/browser_web_contents_delegate.cc:661,1057` | Optional small creation notification if strip observations cannot supply needed opener metadata. |
| Relevant `DEPS` and browser test source lists | Declare narrow internal-content includes and register tests in the established targets. |

No Viz/GPU changes should be needed for the first GPU capture implementation
using the validated GMB path. Reserve a separate patch for any measured backend
gap or root-presentation suppression; do not modify all output devices up front.

GN details: `chrome/browser/headless/BUILD.gn` separates `:headless`,
`:initializer`, and `:headless_extra_parts`. Keep PRTS out of the early
initializer and the old headless-shell targets. Likely dependencies include
`//base`, `//content/public/browser`, `//components/viz/host`,
`//services/viz/privileged/mojom/compositing`, `//media/capture/mojom`,
`//media/base`, `//ui/gfx`, and relevant browser-window/tab headers. The host
target is confirmed in `components/viz/host/BUILD.gn:8`; its Mojo dependency is
visible at 59. `chrome/browser/ui/browser_window/BUILD.gn:7–26` deliberately
keeps the public browser-window interface independent of other Chrome code.

Do not blindly make `:headless_extra_parts → :prts → //chrome/browser:core`:
`:core` already depends on extra parts. A conservative first layout compiles
browser-heavy PRTS `.cc` sources with `:core` using a source-list `.gni`, gives
extra parts only the controller interface, and keeps transport in its own
low-level source set. Alternatively use a factory callback registered by core
so extra parts has no link dependency back to core. Resolve exact owners with
GN checks during implementation; no dependency graph/build was run here.

### Milestones and acceptance checks for later implementation

1. **M1 pipe + targets:** bootstrap under `--headless=new`, handshake, list,
   create, navigate, close, popup discovery, title/URL/load events; two windows
   with distinct sizes. Verify partial frames, EOF, peer death, last-window
   behavior and clean shutdown. Verify another desktop application's focus is
   unchanged and Chrome's native windows stay hidden/cloaked.
2. **M2 input + cursor:** click/type/scroll in the main frame and a cross-origin
   iframe; compare keyboard/character events and focus. Test DPR 1 and 2,
   resize, cursor changes, navigation swaps and target closure during input.
3. **M3 CPU frames:** one CopyFromSurface smoke image, then paced I420/ARGB
   shared-memory capture through the common adapter. Check two targets,
   background rendering, static refresh, resize and bounded queues against a
   deliberately slow prtsd consumer. Mark this mode explicitly as fallback.
4. **M4 GPU frames:** start ARGB/BGRA GMB capture; log actual handle type,
   texture description and adapter identity. Open the duplicated NT handle in
   prtsd and verify keyed-mutex/lease behavior using a known color animation.
   Then test NV12 and QSV import, alignment/color correctness, asynchronous
   encoder completion, backpressure, GPU restart and handle-count stability.
   Confirm via tracing/instrumentation that no pixel Map/readback occurs.
5. **M5 deployment validation:** Win10 22H2, real hardware acceleration and
   chosen isolation configuration; repeat GPU/capture/encode checks after
   sandbox/Job changes. Confirm adapter matching and detect software GPU
   fallback. Only consider a custom output device if measured capture cost or
   a strict presentation requirement justifies it.

Principal risks are adapter mismatch, NV12 render/encoder compatibility,
confusing encoder submission with completed reads, forgotten leases/handles,
capture retargeting across process swaps, hidden-page throttling, synthetic
input bypassing hit-test/cursor state, and GN cycles. These are runtime/build
gates, not claims established by this source survey.

### Patch workflow in this infrastructure repo

`scripts/cr.ps1` was read. It treats Chromium `main` as upstream at `PIN` and
`work` as upstream plus `patches/*.patch`. `cr apply` forcibly recreates `work`
from `main` and applies sorted patches via `git am --3way --keep-cr`;
`cr export` rewrites the patch directory using `git format-patch main..work`.
`cr sync` exports, syncs the pinned source/dependencies, and reapplies patches.
Future implementation should use small milestone commits on the Chromium work
branch and export them under orchestrator control. Do not run `apply` over
uncommitted work. This documentation task ran none of those mutating commands,
did not build Chromium, and made no commits.

## M5 window hooks (2026-10-20, proposed; source validated)

Validated against the same base PIN plus patches 0001-0019, Chromium `work`
HEAD `7ea7adb8afc725e49f8990b2683a9dff40fa609f`. These supersede the old
page-target/input/DSF decisions above; the M4 texture export protocol remains.
Full ownership, synchronization, protocol and acceptance details are in
[phase2-m5-design.md](phase2-m5-design.md), including the mandatory
**chromium-output-investigation mapping** of all relevant hooks/patches.

| Validated location | M5 use / narrow change |
| --- | --- |
| `chrome/browser/headless/chrome_browser_main_extra_parts_headless.cc:22,39,47` | Reuse PRTS prepare/start/stop and M1 keepalives. Add new window registry/host tracker; no second bootstrap or named-pipe service. |
| `chrome/browser/ui/views/frame/browser_view.h:523,692`; `ui/aura/env.h:152`, `env.cc:267-273` | Resolve Browser native Aura window -> host, enumerate/observe all hosts. Browser identity owns a window stream; tabs become metadata. Initial host callback is provisional, not permission to use partially initialized focus/capture clients. |
| `ui/views/widget/desktop_aura/desktop_window_tree_host_win.cc:317,339`; `desktop_native_widget_aura.cc:687,768` | New generic opt-in UI-sequence metadata/readiness hook: Widget, type, parent/context, opacity, corner/shadow/z policy. Add Widget/Aura observations; stop on destroying before compositor death. Do not use test-only AnyWidgetObserver passkeys. |
| `content/browser/media/capture/aura_window_video_capture_device.cc:80-99`; `ui/compositor/compositor.h:472`; `components/viz/host/host_frame_sink_manager.h:256` | Root FrameSinkId capture precedent and CreateVideoCaptureLock. Create client capturer and explicitly ChangeTarget(root id), without a subtree crop; retain lock while subscribed. |
| `chrome/browser/prts/prts_capture.cc:45-108,266-401` | Replace WebContents Source/renderer sink with host source; reuse M4 native BGRA -> keyed export -> daemon private copy. ARGB+kPreferMappableSharedImage on Windows; CPU kDefault fallback. Resource identity is source id under v2. |
| `chrome/browser/ui/views/chrome_views_delegate_win.cc:124-160`; `ui/views/controls/menu/menu_host.cc:129-168`; `ui/views/bubble/bubble_dialog_delegate_view.cc:183-221` | Preserve normal DesktopNativeWidgetAura/NativeWidgetAura selection. Capture additional root hosts only, with explicit owner, visibility, rect and virtual z metadata. Same-host UI appears once. |
| `content/browser/renderer_host/render_widget_host_view_aura.cc:490-529`; `ui/views/widget/desktop_aura/desktop_native_widget_aura.cc:108-170,233-275` | Renderer menu/select/page popup -> separate desktop wrapper host; context/transient parent resolves owner even without Widget parent. Unlike headless_shell, unified Windows does not automatically parent all menus into the main root. |
| `chrome/browser/ui/views/omnibox/omnibox_popup_view_views.cc:108-114,758`; `ui/views/corewm/tooltip_aura.cc:201-215`; `chrome/browser/ui/views/permissions/permission_prompt_bubble_base_view.cc:199-231` | Dropdown, tooltip and permission bubble creation paths. Observe bounds/show/hide/reuse and real popup pixels, not just WebContents/new Browser events. |
| `ui/views/widget/desktop_aura/desktop_window_tree_host_win.cc:444-452,585-590`; `ui/views/win/hwnd_message_handler_headless.cc:229-231` | Notify virtual stacking requests; native headless StackAbove/StackAtTop do nothing. Show/activation tie-breaking, owner/transient graph and explicit levels replace creation-order guesses. |
| `ui/views/widget/desktop_aura/desktop_screen_win_headless.cc:108-128`; `ui/views/controls/menu/menu_controller.cc:3754` | Delegate headless Screen window-at-point queries to the same virtual order used by input and composition. Chrome menu internals otherwise use creation-order host enumeration and can disagree on reused widgets. |
| `ui/views/widget/desktop_aura/desktop_window_tree_host_win.cc:607`; `chrome/browser/ui/views/chrome_views_delegate.cc:184`; `ui/views/widget/widget.h:1213`; `ui/views/window/dialog_delegate.cc:328-330` | Virtualize IsStackedAbove queries too. A PRTS-only pre-init decoration adapter selects custom Views frames for Chrome-owned dialogs whose system-painted titlebar would otherwise be absent from capture; preserve their parent/modal/input semantics. |
| `chrome/browser/prts/prts_capture.cc:374-381`; `gpu/command_buffer/common/shared_image_info.h:48-63`; `components/viz/service/display_embedder/skia_output_surface_impl_on_gpu.cc:1135-1159` | Preserve premultiplied popup alpha in CPU copies; validate native texture alpha. Daemon blends before NV12, with deterministic replacement for platform-only rounded corners/shadows. Final video remains opaque. |
| `ui/views/widget/desktop_aura/desktop_window_tree_host_win.cc:1387-1394,1554-1556`; `ui/aura/window_event_dispatcher.cc:293-297,510-522` | Inject ui mouse/wheel/scroll/key events through host SendEventToSink. Input wire coordinates are root DIP; transform selected-host DIP to source pixels once, Aura inversely transforms. Use ordinary Views/OOPIF routing. |
| `ui/views/widget/desktop_aura/desktop_capture_client.cc:33-113`; `ui/views/controls/menu/menu_pre_target_handler_aura.cc:25-67` | Logical global capture and menu pretarget input are available without native capture. Route popup/capture input, hover transitions and modal focus; restrict control to one logical input window per instance. |
| `ui/aura/window_event_dispatcher.cc:1093-1112`; `ui/views/widget/focus_manager_event_handler.cc:25`; `chrome/browser/ui/accelerator_table.cc:59,68,69` | Explicit synthesized ui KeyEvents bypass Windows server IME queue while retaining Views accelerators. Ctrl+T/L/W must operate real Chrome UI. Remove M2 unconditional skip_if_unhandled from v2's normal content route. |
| `content/public/browser/render_widget_host.h:242-276`; `content/browser/renderer_host/render_widget_host_view_aura.cc:1658-1700`; `ui/views/controls/textfield/textfield.cc:1776-1816` | Preserve M2 ordering through completion observation, not duplicate RWH dispatch. Text resolves current host InputMethod/TextInputClient; guard TYPE_NONE; single char/IME commit exactly once in omnibox or content. |
| `ui/views/win/hwnd_message_handler_headless.cc:54-104,233-292,383-440`; `hwnd_message_handler.cc:1596-1605` | Keep native HWNDs hidden/cloaked. Widget/Aura logical activation and stored Views focus, never OS foreground activation or forced content focus on every input. |
| `ui/views/widget/desktop_aura/desktop_window_tree_host_win.cc:900-930`; `ui/views/win/hwnd_message_handler.cc:1046-1052` | New cursor observer/consume hooks must suppress native SetCursor, ShowCursor and SetCursorPos. Export selected hovered/capture-host cursor, including Views/popup changes; desktop native cursor manager broadcasts are not separate pointer states. |
| `ui/display/headless/headless_screen_manager.h:42-63`; `ui/display/win/screen_win_headless.cc:411-476,603-627`; `ui/aura/window_tree_host.cc:594-617,649-662` | One virtual display/work area per browser window using existing screen manager mutations; full UI/renderer DSF together. No page emulation override in v2. Resize suspends publication, reconciles actual root/client geometry, changes window generation and drains old resources. |
| `ui/views/widget/desktop_aura/desktop_window_tree_host_win.cc:807-862`; `ui/views/win/hwnd_message_handler_headless.cc:113-142,509-539` | Root capture size is client compositor pixels; translate window bounds/insets, not raw outer HWND size. Publish accepted DIP extent/DSF and test odd/fractional dimensions. |
| `ui/views/widget/desktop_aura/desktop_window_tree_host_win.cc:352,657`; `desktop_drag_drop_client_win.cc:132` | PRTS-only no-OLE drag client and native move-loop guard. Full system drag/drop/pointer-lock is separate work; ordinary Aura selection/capture remains. |
| `chrome/browser/chrome_browser_main.cc:1813-1818`; `components/headless/select_file_dialog/headless_select_file_dialog.cc:39-52` | Existing PIN already overrides native file dialog with cancellation. Keep safe cancellation in M5; remote file chooser/download transfer is a separate capability. Unsupported OS UI must be prevented before launch, not closed after becoming visible. |

Chosen popup approach is separate-host composition in prtsd (D3D11 and CPU),
with window scene revisions, bounded private caches and no cross-root atomic-frame
guarantee. Extend two-resource capacity to negotiated resource/byte budgets while
retaining global descriptor credits/key0/1; otherwise menus would starve behind
two window bases. Per-window encoders/tracks replace the singleton active-tab
video; initial signaling binds one viewer session to one window/track.

Investigation P1 null GPU widget, P7 external BeginFrames and P8 custom shared
offscreen output device are deferred optimizations. No Windows Ozone port,
headless_shell replacement, DWM capture or DLL injection. M5 is source study
only: alpha, popup pixels, focus/accelerators, DSF and connected-desktop
invisibility remain implementation acceptance gates, not results of this task.
