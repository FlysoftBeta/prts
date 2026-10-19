# PRTS wire protocol v1 (Phase 1 and later)

Status: implementation contract; no Chromium build is required to implement Phase 1.
Normative words MUST, SHOULD and MAY describe requirements, defaults and options.
The server baseline is Windows 10 22H2, x64; development on Windows 11 is supported.
An instance is one isolated browser process tree or one synthetic source.
A target is a tab or popup with its own viewport; no desktop window is created.
Each instance has one active target and one video stream, shared by its viewers.
Activating a target switches that stream for every viewer. Independent views require
separate instances in v1. Input is injected into content, never through OS input APIs.

## 1. Shared conventions

All binary integers and IEEE-754 floats are little-endian; no implicit padding.
`u8/u16/u32/u64/i32/f64` have their usual widths; `bool` is u8, exactly 0 or 1.
`str` is `u32 byte_length` followed by valid UTF-8 without a terminator.
`bytes` has the same encoding without UTF-8 validation; `T[]` is `u32 count` + items.
`T?` is `bool present` followed by T only if present. Struct fields are positional,
in the order shown; nesting adds no header. Unknown enum values are invalid.
Strings are at most 64 KiB except SDP (1 MiB); arrays at most 4096 elements unless
specified otherwise. Validate lengths, arithmetic and nesting before allocation.
Binary payloads MUST be consumed exactly, including empty payloads.

IDs are scoped to a connection/instance and never reused during its lifetime.
Binary target/buffer/frame/generation IDs are nonzero u64; zero means absent only
where explicitly allowed. JSON represents those u64 IDs as decimal strings.
Instance and session IDs are opaque ASCII strings, 1..128 bytes, assigned by prtsd.
JSON integers other than those strings MUST be safe integers (at most 2^53-1).
`null` is allowed only where documented; omitted optional fields use defaults.
Unknown JSON object fields are ignored; unknown commands return `unsupported`.

`timestamp_us` is capture time in microseconds since the browser connection's
HELLO_ACK, using a monotonic clock. Synthetic sources use instance readiness as
their origin. prtsd maps this to its clock for metrics; never subtract wall clocks
or viewer `performance.now()` from it. Capture timestamps need not be unique.
Frame IDs strictly increase per instance, including across target switches.
Control-plane `time_ms` is prtsd monotonic milliseconds since daemon start.

`Size = {width:u32,height:u32,dpr:f64}` describes CSS viewport pixels and device
pixel ratio. Width/height are 1..8192; DPR is finite, 0.5..4. Device size is
`ceil(width*dpr)` by `ceil(height*dpr)`, each at most 16384 and at most 64M pixels.
Implementations MAY impose smaller limits, reported in capabilities.
`Rect = {x:u32,y:u32,width:u32,height:u32}` uses device pixels, top-left origin.
All frames are complete top-down BGRA8 images, sRGB, opaque alpha 255. No HDR.
Damage rectangles are hints, clipped to bounds; an empty array means no damage.
The first frame of a generation has full damage. Consumers may ignore damage.

## 2. Browser <-> prtsd anonymous pipes

### 2.1 Launch and channel layout

prtsd creates three anonymous byte pipes and starts Chromium with `--headless=new`:

| Channel | prtsd retains | Browser inherits | Contents |
| --- | --- | --- | --- |
| command | write end | read end (`cmd`) | Requests, input, responses to browser requests, frame ACKs |
| event | read end | write end (`evt`) | Responses, target events, registration requests |
| frame | read end | write end (`frame`) | Frame descriptors only; no pixel bytes |

The switch is `--prts-pipe=cmd:0xHEX,evt:0xHEX,frame:0xHEX` with unsigned,
pointer-sized handle values in the child's inherited handle table. These are
real handles, never CRT descriptors or pseudo-handles. Reject missing/duplicate
keys, invalid handles, or a switch in a renderer/GPU child process.
Use STARTUPINFOEX + PROC_THREAD_ATTRIBUTE_HANDLE_LIST containing exactly these
three inheritable ends, `bInheritHandles=TRUE`, and EXTENDED_STARTUPINFO_PRESENT.
The parent ends are non-inheritable. Close the parent's copies of child ends
immediately after launch; clear inheritance on the child's copies before it
launches subprocesses. Chromium diagnostics use separate stderr, never these pipes.
Handle-list inheritance is documented in [Microsoft's process creation guide](https://learn.microsoft.com/en-us/windows/win32/procthread/creating-processes).

prtsd retains the browser process handle with PROCESS_DUP_HANDLE and lifecycle
rights. No prtsd process handle is inherited. Any GPU-process export handle MUST
first be brokered into the browser process; the wire source handle is always in
that process, never a PID supplied in a message.
Anonymous pipes are synchronous: use dedicated blocking I/O workers, independent
readers and serialized writers, never the UI/compositor/encoder thread. Reads and
writes may be partial; loop until the entire record is transferred. Bound queues.

### 2.2 Binary envelope and handshake

Each record is `u32 length` followed by exactly `length` bytes:

| Offset after length | Field | Meaning |
| --- | --- | --- |
| 0 | magic:u32 | 0x53545250 (wire bytes `PRTS`) |
| 4 | major:u16, minor:u16 | v1.0 = 1,0 |
| 8 | type:u16, flags:u16 | Flags MUST be zero in v1 |
| 12 | request_id:u64 | Nonzero for requests/responses; zero for notifications |
| 20 | payload | Schema selected by type |

Length excludes its own four bytes; valid range 20..1,048,576 (1 MiB).
The same envelope is used on all three pipes. Type on wrong channel/direction,
bad framing/version or duplicate outstanding request ID is a fatal connection
error. Close all pipes and fail the instance; do not attempt stream resync.
Only known type IDs are permitted in v1.0; extensions require a negotiated minor.
Requests initiated by prtsd use odd IDs; browser requests use even IDs. IDs are
strictly increasing per initiator. Each request gets exactly one response of
the listed type or ERROR, echoing its ID, on the reverse control pipe.
Notifications use zero. Responses may arrive out of order; preserve mutation
order per target, and preserve event order on the event pipe.

prtsd first sends HELLO, browser replies HELLO_ACK within 5 seconds. No other
messages or frames may precede ACK. Both handshake envelopes use 1.0; HELLO
selects the highest mutually supported version. No overlap returns ERROR then
closes. v1.0 is mandatory. Browser verifies the parent's PID for diagnostics,
but PID alone is not authorization; possession of inherited pipes is the boundary.

| Type | Channel/direction | Payload, in wire order |
| --- | --- | --- |
| 0x0001 HELLO | command D->B, request | min_major:u16,min_minor:u16,max_major:u16,max_minor:u16,daemon_pid:u32,adapter_luid:u64,frame_modes:u32,max_inflight:u32 |
| 0x0002 HELLO_ACK | event B->D, response | selected_major:u16,selected_minor:u16,browser_pid:u32,adapter_luid:u64,frame_modes:u32,max_inflight:u32,max_width:u32,max_height:u32,max_pixels:u32,features:u32 |
| 0x0003 ERROR | reverse control, response | code:u32,message:str,retryable:bool |
| 0x0004 PING | command D->B, request | cookie:u64 |
| 0x0005 PONG | event B->D, response | cookie:u64 |
| 0x0006 SHUTDOWN | command D->B, request | empty |
| 0x0007 OK | reverse control, response | empty |
| 0x0008 FAULT | either control direction, notification | code:u32,message:str,fatal:bool |

Here B=browser, D=prtsd. Adapter LUID is the raw 64-bit Windows LUID bit pattern;
zero means unavailable. frame_modes bits: 1=D3D11 keyed mutex, 2=CPU section;
ACK returns the supported intersection. CPU sections are mandatory. max_inflight
is 1..3, ACK returns min(local limit, offered value). features bits: 1=popups,
2=custom cursor; zero means neither. Target/input operations below are mandatory.
Matching adapter LUIDs are required for the GPU mode. prtsd configures its D3D
and encoder device to match or chooses CPU mode; no silent WARP claim as QSV.
SHUTDOWN stops production, drains resources and returns OK before exiting.

### 2.3 Targets and lifecycle

`Target = {id:u64,kind:u8,opener:u64,url:str,title:str,load:u8,
size:Size,generation:u64,active:bool}`. kind: 1=tab, 2=popup; opener=0 if none.
load: 0=idle, 1=loading, 2=complete, 3=failed. Popup is a logical target.
The browser starts with no targets; prtsd creates the initial target after ACK.
At least the active target renders; inactive targets MAY throttle or suspend.

| Type | Direction | Payload / response |
| --- | --- | --- |
| 0x0100 TARGET_CREATE | D->B request | url:str,size:Size,activate:bool -> TARGET_RESULT |
| 0x0101 TARGET_CLOSE | D->B request | target:u64 -> OK |
| 0x0102 TARGET_NAVIGATE | D->B request | target:u64,url:str -> OK |
| 0x0103 TARGET_LIST | D->B request | empty -> TARGET_LIST_RESULT |
| 0x0104 TARGET_RESIZE | D->B request | target:u64,size:Size -> TARGET_RESULT |
| 0x0105 TARGET_ACTIVATE | D->B request | target:u64 -> TARGET_RESULT |
| 0x0180 TARGET_RESULT | B->D response | target:Target |
| 0x0181 TARGET_LIST_RESULT | B->D response | targets:Target[] (max 256) |
| 0x0190 TARGET_CREATED | B->D event | target:Target |
| 0x0191 TARGET_CLOSED | B->D event | target:u64,reason:str |
| 0x0192 TARGET_CHANGED | B->D event | target:Target |
| 0x0193 CURSOR_CHANGED | B->D event | target:u64,generation:u64,cursor:Cursor |

These travel on command/event pipes as appropriate. Each successful mutation
emits its event before the response; CREATE emits CREATED, resize/activate emit
CHANGED. Title, URL (including redirects/history), load state and activation
changes emit full CHANGED snapshots. Popups emit CREATED with kind/opener set;
they do not steal activation in v1. Navigation OK means accepted, not loaded;
load failure is CHANGED(load=failed), not a late response failure.
The first target becomes active even if activate=false. Activating another
emits CHANGED for both. Closing the active target selects the lowest surviving
ID and emits CHANGED; closing the last leaves the instance alive without video.
Closing an unknown target returns not_found; list is a serialized snapshot.

Generation starts at 1 and increases on resize, DPR change, activation into a new stream, or
frame mode/device reset; it never decreases per target. Events may cross frames
on the separate pipe: consumers key by target/generation and drop stale frames.
After an activation/resize response no new old-generation descriptors are
submitted; already queued ones still require release. Video changes dimensions
without creating a new WebRTC session; force a keyframe on each stream switch.
If a descriptor arrives before its generation snapshot, release/drop it rather
than waiting with a pool credit held. On target=null, the viewer hides old video.

### 2.4 Input payloads

`InputHeader = {target:u64,generation:u64,sequence:u64,modifiers:u32}`.
sequence increases per control owner. Modifiers bits: 1=Shift, 2=Ctrl, 4=Alt,
8=Meta, 16=CapsLock, 32=NumLock; all remaining bits zero.
Coordinates and wheel deltas are finite f64 CSS pixels in the target viewport.
Button enum: 0=none, 1=left, 2=middle, 3=right, 4=back, 5=forward.
buttons mask: 1=left, 2=right, 4=middle, 8=back, 16=forward (DOM convention).

| Type | D->B notification payload |
| --- | --- |
| 0x0200 MOUSE_MOVE | InputHeader,x:f64,y:f64,buttons:u32 |
| 0x0201 MOUSE_DOWN | InputHeader,x:f64,y:f64,button:u8,buttons:u32,click_count:u8 |
| 0x0202 MOUSE_UP | InputHeader,x:f64,y:f64,button:u8,buttons:u32,click_count:u8 |
| 0x0203 MOUSE_WHEEL | InputHeader,x:f64,y:f64,delta_x:f64,delta_y:f64,buttons:u32 |
| 0x0204 KEY_DOWN | InputHeader,vk:u32,code:str,key:str,text:str,location:u8,repeat:bool,is_system:bool |
| 0x0205 KEY_UP | Same fields as KEY_DOWN; text empty, repeat=false |
| 0x0206 KEY_CHAR | InputHeader,text:str |
| 0x0207 FOCUS | InputHeader,focused:bool |

click_count is 1..3 on down/up and computed by the viewer; buttons is state
after this event. Wheel positive x scrolls right, positive y scrolls down;
prtsd converts CSS pixel deltas to Chromium's wheel representation once.
vk is Windows virtual-key code 0..255; code/key are DOM KeyboardEvent strings;
location is DOM 0..3. is_system marks Alt/system-key routing, never OS injection.
prtsd derives vk from code/key for clients without Windows VK; unsupported keys
return an input error. Empty code is allowed only for unidentified keys.
KEY_DOWN text is informational and MUST NOT independently insert text.
KEY_CHAR inserts its UTF-8 text exactly once via content character/text events;
the viewer sends it from committed text input, including IME commits. Preedit
composition is out of scope for v1; named editing keys are key events.
Text is at most 4096 bytes/event. Reject embedded NUL and malformed Unicode.
On blur, owner/session loss or activation, prtsd clears held buttons/keys and
focuses content appropriately; focus never changes the Windows desktop focus.
Stale generation/inactive-target input is dropped, with rate-limited FAULT
(stale_state, fatal=false); invalid input produces FAULT(invalid_argument).

### 2.5 Registered frame resources

Use a bounded pool, normally three buffers per active generation. No handles or
pixels appear per frame. Browser registers each resource on the event pipe,
waits for OK, then may publish frames referencing it. Registration is:

| Type | Direction/channel | Payload |
| --- | --- | --- |
| 0x0300 BUFFER_REGISTER | B->D request, event | target:u64,generation:u64,buffer:u64,kind:u8,source_handle:u64,width:u32,height:u32,format:u8,stride:u32,byte_length:u64,adapter_luid:u64 |
| 0x0301 BUFFER_UNREGISTER | B->D request, event | buffer:u64 |
| 0x0310 FRAME_TEXTURE | B->D notification, frame | FrameHeader |
| 0x0311 FRAME_CPU | B->D notification, frame | FrameHeader |
| 0x0320 FRAME_RELEASE | D->B notification, command | buffer:u64,frame:u64,disposition:u8 |
| 0x0321 FRAME_CONFIG | D->B request, command | target:u64,mode:u8,max_inflight:u32,max_fps:u32 -> TARGET_RESULT |

`FrameHeader = {target:u64,generation:u64,buffer:u64,frame:u64,
timestamp_us:u64,width:u32,height:u32,format:u8,damage:Rect[]}` (max 64 rects).
kind/mode: 1=D3D11 texture, 2=CPU section; format: 1=BGRA8 only.
Mode starts as CPU; FRAME_CONFIG selects a negotiated mode, FPS 1..120, and
1..negotiated max_inflight. Applying it allocates a new generation. Old resources
drain before unregister; production may pause. Resize uses the same transition.
The total number of submitted/unreleased descriptors across all generations
MUST NOT exceed negotiated max_inflight; retired pools do not create credits.
Reject duplicate buffers, inconsistent metadata, or references before OK.

prtsd calls DuplicateHandle from its retained browser process handle into its
own process, with bInheritHandle=FALSE and DUPLICATE_SAME_ACCESS, never
DUPLICATE_CLOSE_SOURCE. The browser retains the original handle until register
response; on OK each side owns its handle until unregister/teardown. On ERROR
prtsd closes any partially imported objects and browser destroys the resource.
Only resource handles on authenticated instance pipes may be imported; validate
object type and actual resource description against metadata before OK.
This direction uses the parent's existing authority; Chromium needs no right
to duplicate handles into prtsd. [DuplicateHandle](https://learn.microsoft.com/windows/win32/api/handleapi/nf-handleapi-duplicatehandle)
requires PROCESS_DUP_HANDLE on the participating process handles.

For GPU buffers create single-sample, single-mip, single-array BGRA8 D3D11
textures with SHARED_NTHANDLE | SHARED_KEYEDMUTEX, obtain a real NT handle using
IDXGIResource1::CreateSharedHandle (READ|WRITE), and import with OpenSharedResource1.
stride and byte_length are zero for textures. CPU adapter_luid is zero. Texture
adapter LUID MUST match HELLO_ACK and the daemon's selected adapter.
Legacy GetSharedHandle handles are forbidden; [Microsoft distinguishes them
from NT handles](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiresource-getsharedhandle).
The exported image excludes the cursor; encode-side BGRA->NV12 conversion is
allowed on the GPU. Retain ownership until all encoder/conversion reads finish.

v1 uses keyed mutexes, not shared fences. Every buffer repeats this sequence:

1. Producer owns a free credit, AcquireSync(key=0), writes the complete image,
   and ReleaseSync(key=1) after ordering writes on its D3D context; publish frame.
2. Consumer AcquireSync(key=1), reads/converts/encodes (or intentionally drops),
   waits until GPU reads are complete, then ReleaseSync(key=0).
3. Consumer sends FRAME_RELEASE; only then may producer reuse that credit.

Keys 0/1 are constant because at most one frame can occupy a buffer. Even a
dropped texture frame MUST acquire key 1 and release key 0 before its ACK.
Check exact AcquireSync return codes; WAIT_TIMEOUT is not success. A timeout
(1000 ms maximum), WAIT_ABANDONED or device removal invalidates the generation:
send fatal FAULT and restart the instance; never forge a release or reuse it.
[AcquireSync documents initial key 0 and abandoned-resource handling](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgikeyedmutex-acquiresync).
Fences can be added only as a future negotiated mode, with explicit values.

CPU fallback uses an unnamed pagefile-backed file-mapping section. Browser maps
read/write; prtsd duplicates and maps read-only. stride is a multiple of 4 and
at least width*4; byte_length=stride*height, at most 256 MiB. Pixels start at
offset zero, with no header. Publish only after synchronous readback/copy and
a release memory barrier; consumer applies an acquire barrier after descriptor
receipt. The section MUST NOT be written again before FRAME_RELEASE, which
means all CPU/encoder reads have completed. No keyed mutex is used for sections.

FRAME_RELEASE disposition: 0=consumed, 1=dropped; both return exactly one credit.
ACK means resource access is complete, not that video was sent or displayed.
Browser unregisters only after every submitted frame for that buffer is released.
prtsd drops its maps/textures/handles before OK; browser then destroys its copies.
Unknown release, duplicate release or unregister-while-busy is a protocol error.
Close/resize never bypass this rule; on pipe/process failure close local handles
and destroy pools, and terminate the instance rather than reuse uncertain data.

### 2.6 Backpressure and failure

Latest-frame-wins applies at capture and encode ingress, not to arbitrary pipe
bytes. Keep at most one pending unsent descriptor and one pending encode frame;
replace older pending work with newer work. An unpublished GPU frame is locally
reclaimed by acquiring key 1/releasing key 0; it needs no wire ACK. Once any
record bytes are written, finish that record. Each published descriptor consumes
a pool credit until release, even if stale, inactive or superseded.
With no free credit, skip captures; never block compositor rendering. FPS limits
are ceilings, not a requirement to queue frames. Slow viewers drop outgoing video
at the sender; they never hold texture resources. No video viewers means capture
may pause, with one fresh full frame/keyframe requested when a viewer joins.
Do not drop key transitions, focus, resource operations, responses or lifecycle
events. Coalesce consecutive pending mouse moves/wheels only before the next
discrete input event. Drain both incoming pipes independently to avoid ACK stalls.
Control queues are capped at 1024 messages/8 MiB; overflow fails the instance.
Use a 5-second request deadline (navigation acceptance only) and idle PING every
5 seconds; absent response for 10 seconds or any pipe EOF fails the instance.
Request timeout leaves state uncertain: fail the instance instead of retrying a
mutation. prtsd emits instance.failed and closes its sessions/resources.

## 3. prtsd <-> Node control plane (stdio)

### 3.1 Framing, correlation and errors

stdin and stdout each carry `u32 little-endian UTF-8 byte length` + one JSON object,
no BOM/newline required. Length is 1..4,194,304 bytes. Node uses Buffer byte lengths
and an incremental parser; one read is not necessarily one message. stdout is
exclusively protocol; stderr is unframed diagnostic text. Serialize writes and
honor Node stream backpressure. Embedded newlines in JSON strings are escaped.

```json
{"v":1,"id":"r1","method":"hello","params":{"versions":[1]}}
{"v":1,"id":"r1","result":{"version":1,"daemon":"prtsd","capabilities":{"sources":["synthetic","chromium"],"encoders":["software"],"isolation_profiles":["job"],"max_instances":8,"max_width":8192,"max_height":8192,"max_pixels":64000000}}}
{"v":1,"id":"r2","error":{"code":"not_found","message":"Unknown instance","retryable":false,"details":{"instance_id":"i-missing"}}}
{"v":1,"event":"instance.ready","seq":1,"time_ms":35,"data":{"instance_id":"i1","source":"synthetic","active_target_id":"1","encoder":"software","isolation_profile":"job"}}
```

The first request is hello; no commands/events precede its response. Major v=1
must be supported by both sides. A request has unique string id (1..128 bytes),
method and object params. Response echoes id and has exactly one result/error.
IDs cannot be reused on a live stdio connection. Events have no id, and globally
increasing safe-integer seq; time_ms is monotonic. Ordering is stdout record order.
Unknown methods/invalid parameters receive errors. Malformed framing/JSON/v, or
duplicate request IDs close stdio and terminate the daemon's instances. Node EOF
causes graceful instance/session shutdown; force termination after 5 seconds.
Node must keep draining stdout even when it is not issuing commands.

Errors: `{code:string,message:string,retryable:boolean,details?:object}`.
The shared stable code mapping to binary ERROR/FAULT is:

| Binary | JSON code | Meaning |
| --- | --- | --- |
| 1 | invalid_argument | Bad type, range, URL or state-independent value |
| 2 | not_found | Unknown instance/target/session |
| 3 | unsupported | Version, feature, encoder, profile or method unavailable |
| 4 | invalid_state | Operation incompatible with lifecycle/signaling state |
| 5 | resource_exhausted | Capacity, buffer or queue limit |
| 6 | permission_denied | Policy/ownership denied |
| 7 | timeout | Deadline expired |
| 8 | device_lost | D3D/encoder resource lost |
| 9 | internal | Other implementation failure |
| 10 | stale_state | Old target generation/input sequence |

No stack traces, handles, tokens or credentials in public errors/logs. A failed
command emits one error response, not a second successful response later.
Async failure after an accepted command uses the corresponding lifecycle event.

### 3.2 Commands and object shapes

Fields without `?` are required. Every operation returns an object.
JSON size is `{width:number,height:number,dpr?:number=1}` with section 1 limits.
Target JSON mirrors Target using `target_id`, `opener_target_id` (null for none),
`kind:"tab"|"popup"`, `load:"idle"|"loading"|"complete"|"failed"`, `size`,
`url`, `title`, `generation` and `active`. Binary id/opener map to these names.
Instance JSON is `{instance_id,source,state,active_target_id,options}`; state is
`starting|ready|closing|failed`, active_target_id is a decimal string or null,
and options contains effective values, including the resolved encoder/profile.

| Method | params | result |
| --- | --- | --- |
| hello | versions:number[] | version,daemon,capabilities (example above) |
| instance.create | options:InstanceOptions | instance:Instance |
| instance.close | instance_id:string | closed:true |
| instance.list | empty | instances:Instance[] |
| target.create | instance_id,url?:string="about:blank",size?:Size,activate?:bool=true | target:Target |
| target.close | instance_id,target_id:string | closed:true |
| target.navigate | instance_id,target_id,url:string | accepted:true |
| target.list | instance_id | targets:Target[] |
| target.resize | instance_id,target_id,size:Size | target:Target |
| target.activate | instance_id,target_id | target:Target |
| session.create | instance_id,viewer_id:string,control?:bool=false | session_id,polite:false,ice_servers:object[],control:bool |
| session.offer | session_id,ice_restart?:bool=false | description:{type:"offer",sdp:string} |
| session.answer | session_id,description:{type:"answer",sdp:string} | accepted:true |
| session.ice | session_id,candidate:IceCandidate or null | accepted:true |
| session.close | session_id | closed:true |
| network.set | instance_id,settings:NetworkSettings | settings:NetworkSettings |
| stats.configure | interval_ms:number (0 or 250..10000; default 1000) | interval_ms |

InstanceOptions fields: source=`"synthetic"|"chromium"` (default synthetic),
initial_url:string (default about:blank), size:Size (default 1280x720,dpr=1),
fps?:number (synthetic production ceiling, default 30; 1..60 in Phase 1;
effective value and supported range are reported in options/capabilities),
encoder=`"auto"|"qsv"|"software"` (default auto), bitrate_caps=
`{min_bps?:number=100000,max_bps?:number=4000000}` with 1<=min<=max<=100000000,
isolation_profile:string (default `"job"`). Auto prefers QSV, then software;
explicit qsv attempts the hardware H.264 MFT too. Hardware initialization or
encoding failure falls back to software and emits a warning log; stats and
instance.ready report the actual backend name. Optional profiles `appcontainer`
and `lpac` are accepted only if advertised and GPU compatibility is validated.
`job` means managed process-tree Job Object containment, not a security sandbox.
Synthetic uses the same validated options; isolation records policy but there is
no browser process to isolate. Unknown/unimplemented profiles return unsupported.
Size omission on target.create inherits instance default, not another tab's size.

create returns starting immediately after reserving an ID; later instance.ready
contains the initial active target. Failure is instance.failed with error.
The create response precedes target.created and instance.ready. A failed instance
retains its ID/list entry until closed, so cleanup and diagnostics remain possible.
Target/session commands require ready. Close cancels startup if necessary,
drains work, emits closed events then responds; unknown/already-closed IDs return
not_found. Lists omit closed instances. URL policy initially permits absolute
http/https/about:blank; reject other schemes unless explicitly enabled by policy.

### 3.3 WebRTC signaling and network simulation

Each session has one outgoing video track, no audio, and the two DataChannels
specified below. prtsd is always offerer; Node relays SDP/ICE unchanged to the
authenticated viewer. viewer_id is an application identity, not authentication.
The Node app authorizes instance access and never exposes raw stdio publicly.
Only one session per instance may own control; control=true fails with
permission_denied if occupied. Ownership ends at session.close/disconnect.
View-only sessions may report stats but cannot inject input or activate targets.

session.offer sets local description before returning SDP; trickled local ICE
is emitted as session.ice (it may precede the offer response; Node buffers it).
Viewer sets remote offer, creates/sets local answer, calls session.answer, and
trickles its ICE through session.ice. IceCandidate is
`{candidate:string,sdpMid:string|null,sdpMLineIndex:number|null,usernameFragment?:string}`;
at least mid or index is non-null. null signals end-of-candidates in either
direction. prtsd queues up to 256 remote candidates until answer is installed.
Only one outstanding offer is allowed; answers in the wrong state are rejected.
Repeat session.offer after stable for renegotiation (ice_restart=true for restart); local events
for an offer follow its acceptance and precede the next offer. Ufrag distinguishes
ICE generations; discard old candidates. v1 peers must include usernameFragment
on non-null candidates; null applies to the current offer. Close releases the
PeerConnection and ownership; terminal ICE failure emits session.closed.

NetworkSettings has `bitrate_cap_bps:number|null` (default null),
`added_latency_ms:number` (0..2000, default 0), `loss_percent:number`
(finite 0..100, default 0), and `seed:number` (u32, default 1).
network.set replaces the entire settings object; all fields are required.
Settings apply to every session of that instance. Effective encoder maximum is
min(instance max, simulation cap, congestion estimate); minimum never overrides
a lower cap. Added latency is a one-way outbound RTP/RTCP packet delay; loss is
independent Bernoulli loss using a seeded PRNG per session. Retransmissions pass
through the same shaper. ICE, DTLS, SCTP/DataChannels and inbound traffic bypass
the shaper so input and connectivity remain usable. Use SRTP/SRTCP packet-layer
hooks rather than delay frames; unsupported shaping returns unsupported, never
pretends success. The packet queue is at most 4 MiB/session, drops oldest packets
on overflow, and is flushed on replacement/close. Report shaping drops separately.

### 3.4 Events, stats and examples

All event data except daemon/log events includes instance_id; session events also
include session_id. Target events mirror section 2; Node translates binary enums.

| event | data fields in addition to scope |
| --- | --- |
| instance.ready | source,active_target_id,encoder:string,isolation_profile:string |
| instance.failed | error:Error |
| instance.closed | reason:string |
| target.created / target.changed | target:Target |
| target.closed | target_id,reason:string |
| cursor.changed | target_id,generation,cursor:CursorJSON |
| session.ice | candidate:IceCandidate or null |
| session.state | state:"connecting"|"connected"|"disconnected" |
| session.closed | reason:string |
| stats | interval_ms,encoder:object,frames:object,sessions:object[] |
| log | level:"debug"|"info"|"warn"|"error",message:string,instance_id?:string,fields?:object |

stats.encoder fields: name:string, fps:number, bitrate_bps:number,
qp_avg:number|null (codec-specific quantizer, null if unavailable).
Optional encode_ms_avg:number|null measures conversion plus encoder time/frame,
excluding synthetic rendering; capture_to_encode below includes rendering.
stats.frames fields: captured:number,encoded:number,dropped:number (interval
counts), capture_to_encode_ms_avg:number|null, capture_to_encode_ms_p95:number|null,
capture_to_send_ms_avg:number|null. Latency ends at encode completion/first RTP
submission respectively; no cross-host subtraction. Session entries contain
session_id,rtt_ms:number|null,available_outgoing_bps:number|null,
rtp_sent_bps:number,shaper_dropped_packets:number,viewer_feedback:object|null.
shaper_dropped_packets is cumulative over the session, including loss, overflow,
settings replacement flushes and socket-send failures.
Counter/rate units are bytes*8/second excluding IP overhead; rates cover the stated
interval, not lifetime. Unavailable values are null, not zero. No samples means
latencies/qp are null. Node may combine these with browser getStats for display.
A daemon can coalesce stats and logs under stdout pressure; seq numbers are
assigned when records are emitted. Do not drop responses/lifecycle events.
Bound stdout queues at 8 MiB; continued overflow shuts down managed instances.

Examples (each line below is a JSON body with its own binary length prefix):

```json
{"v":1,"id":"c1","method":"instance.create","params":{"options":{"source":"synthetic","initial_url":"https://example.org/","size":{"width":1280,"height":720,"dpr":1},"encoder":"auto","bitrate_caps":{"min_bps":100000,"max_bps":3000000},"isolation_profile":"job"}}}
{"v":1,"id":"c1","result":{"instance":{"instance_id":"i1","source":"synthetic","state":"starting","active_target_id":null,"options":{"source":"synthetic","initial_url":"https://example.org/","size":{"width":1280,"height":720,"dpr":1},"encoder":"software","bitrate_caps":{"min_bps":100000,"max_bps":3000000},"isolation_profile":"job"}}}}
{"v":1,"event":"instance.ready","seq":1,"time_ms":35,"data":{"instance_id":"i1","source":"synthetic","active_target_id":"1","encoder":"software","isolation_profile":"job"}}
{"v":1,"id":"t1","method":"target.navigate","params":{"instance_id":"i1","target_id":"1","url":"https://example.net/"}}
{"v":1,"id":"t1","result":{"accepted":true}}
{"v":1,"id":"s1","method":"session.create","params":{"instance_id":"i1","viewer_id":"viewer-a","control":true}}
{"v":1,"id":"s1","result":{"session_id":"s1","polite":false,"ice_servers":[],"control":true}}
{"v":1,"id":"s2","method":"session.offer","params":{"session_id":"s1"}}
{"v":1,"id":"s2","result":{"description":{"type":"offer","sdp":"v=0\r\n..."}}}
{"v":1,"id":"s3","method":"session.answer","params":{"session_id":"s1","description":{"type":"answer","sdp":"v=0\r\n..."}}}
{"v":1,"id":"s4","method":"session.ice","params":{"session_id":"s1","candidate":{"candidate":"candidate:...","sdpMid":"0","sdpMLineIndex":0,"usernameFragment":"abc"}}}
{"v":1,"id":"n1","method":"network.set","params":{"instance_id":"i1","settings":{"bitrate_cap_bps":800000,"added_latency_ms":80,"loss_percent":2.5,"seed":42}}}
```

SDP/candidate ellipses above are illustrative, not valid signaling payloads.

## 4. Viewer <-> prtsd DataChannels

prtsd creates `prts.control.v1` (ordered, reliable) and `prts.feedback.v1`
(unordered, maxRetransmits=0), protocol string `prts-json-v1`, negotiated=false.
SCTP IDs are assigned by WebRTC. Each message is one UTF-8 JSON text message,
at most 64 KiB; no pipe prefix. Both use `{v:1,type:string,...}`. Unknown types
receive error on the control channel; invalid v/oversize/repeated abuse closes
the session. Input and cursor use control; only stats uses feedback. Never
silently move discrete input onto the unreliable channel.

| type / direction | Fields after v,type |
| --- | --- |
| ready D->V | instance_id,session_id,control:bool,target:Target or null |
| target D->V | target:Target or null (active snapshot on switch/resize/close) |
| mouse.move V->D | CommonInput,x,y,buttons |
| mouse.down / mouse.up V->D | CommonInput,x,y,button,buttons,click_count |
| mouse.wheel V->D | CommonInput,x,y,delta_x,delta_y,buttons |
| key.down / key.up V->D | CommonInput,vk?:number,code,key,text,location,repeat,is_system |
| key.char V->D | CommonInput,text |
| focus V->D | CommonInput,focused:bool |
| stats V->D | sample_seq:number,interval_ms:number,decoded_fps:number,received_bps:number,rtt_ms:number|null,jitter_ms:number|null,packets_lost:number,decode_ms_avg:number|null,render_ms_avg:number|null |
| cursor D->V | target_id,generation,cursor:CursorJSON |
| error D->V | sequence?:number,error:Error |

CommonInput is `target_id:string,generation:string,sequence:number,modifiers:number`.
sequence is strictly increasing per session (safe integer, begins at 1); no
response per successful input. JSON enums/masks match binary input; prtsd validates
ownership, active target and generation before forwarding, translating sequence
to binary u64. Buttons/keys must match the tracked state; stale/duplicate sequence
is ignored. Send ready when control opens, then target before input for a new
generation is accepted. Release held state on switch/blur/disconnect. Session
control is authority from session.create, never a viewer-supplied boolean.

The video includes only viewport content, with no letterboxing or cursor baked
in. The synthetic input indicator in section 5 is test content, not the browser
cursor. Viewer maps pointer location through the actual displayed video content
rectangle, excluding CSS object-fit bars, to `[0,width)` / `[0,height)` CSS
coordinates from the latest target snapshot. Do not multiply by the viewer's
DPR. Exported device pixels equal CSS coordinates times the target DPR; browser
content injection uses CSS coordinates. Outside-content downs/wheels are ignored;
captured drags/up events may extend beyond bounds (clamp to [-32768,32767]).
Normalize DOM wheel deltaMode: pixels unchanged, lines*16, pages*viewport height
(horizontal pages use width). Viewer prevents local scrolling for captured input.

Cursor wire struct is `{shape:u8,hot_x:u32,hot_y:u32,width:u32,height:u32,
png:bytes}`. Shape codes 0..8: default,pointer,text,crosshair,move,ew-resize,
ns-resize,none,custom. Built-in shapes have zero dimensions/hotspot and empty png.
Custom cursor is max 128x128 device pixels, hotspot within bounds, PNG <=32768
bytes; unlike video its alpha is preserved. CursorJSON uses `shape` as the enum
name, and for custom adds `width,height,hot_x,hot_y,png_base64`; built-ins use only
shape. Render custom cursor at dimensions/hotspot divided by target DPR; ignore
stale generations. Lack of custom-cursor capability falls back to default.

```json
{"v":1,"type":"mouse.down","target_id":"1","generation":"2","sequence":17,"modifiers":1,"x":120.5,"y":64,"button":1,"buttons":1,"click_count":1}
{"v":1,"type":"key.down","target_id":"1","generation":"2","sequence":18,"modifiers":0,"vk":65,"code":"KeyA","key":"a","text":"a","location":0,"repeat":false,"is_system":false}
{"v":1,"type":"key.char","target_id":"1","generation":"2","sequence":19,"modifiers":0,"text":"a"}
{"v":1,"type":"cursor","target_id":"1","generation":"2","cursor":{"shape":"pointer"}}
```

Viewer stats describe local interval deltas; packets_lost may be negative when
late packets revise WebRTC counters. Use null when a measurement is unavailable.
sample_seq increases; accept only the newest feedback. Clamp rates/intervals to
plausible ranges and treat feedback as untrusted telemetry, never resource policy.
No absolute viewer timestamp is needed. decode_ms_avg is decode time/frame;
render_ms_avg is local decode-to-display if measurable. End-to-end capture-to-
display latency is unavailable in v1; do not label decode or RTT as that metric.

## 5. Phase 1 synthetic source

`source:"synthetic"` creates no Chromium process and opens no browser pipes.
An in-process adapter implements the identical target/input/frame interfaces,
control-plane lifecycle, encoding, WebRTC, shaping and statistics. A browser
binary is not required. Use CPU-owned BGRA buffers with the same bounded pool/
release semantics; optionally use D3D textures to exercise QSV. No fake claims
about GPU acceleration or isolation: capabilities/stats show actual backends.

After starting, create fake target `1`, generation `1`, active=true, kind=tab,
with the requested initial URL/size and title `PRTS synthetic: <URL>`; emit
target.created then instance.ready. Initial creation and each navigation emit
load=loading then load=complete on the next scheduler tick; no network fetch.
The initial created snapshot is loading. target.navigate accepts the same URL
policy, changes URL/title, emits changed, then completes within 100 ms.
target.create allocates increasing IDs and follows the same activation rules.
close/list/resize/activate work exactly as section 2; no scripted popups exist.
Activation/resize allocates a new generation and requests a full keyframe.

Default production is 30 FPS while a viewer is connected; skip ticks under
backpressure, with no catch-up queue. Generate a moving high-contrast marker,
color bars/grid, target ID, frame counter and elapsed time. Draw the most recent
pointer location, button state, focus and committed text in the frame so input
can be checked visually; keep at most the latest 256 Unicode scalar values.
Pointer over a central test rectangle changes cursor to pointer; elsewhere use
default, emitting a cursor event only on a change. Each target keeps independent
URL, size and input state. Closing the last target pauses frames and sends
DataChannel target=null; creating a target resumes without recreating sessions.
Stats count actual captures/drops/encodes and timings, never fixed placeholder
rates. Encoder/network errors use the same failure events as Chromium sources.

## 6. Rationale and implementation checks

Three pipes keep bulk frame descriptors independent of control traffic while
preserving straightforward anonymous-pipe launch semantics. Positional binary
records avoid JSON/handle precision ambiguity inside Chromium. Registered pools
amortize DuplicateHandle/import costs; ACK and mutex ownership solve different
problems (pool credit and GPU access). CPU sections preserve the same lifetime
contract for Phase 1 and software encoding. Length-prefixed JSON on stdio is
simple to parse in Node and bounds allocation before parsing; it also avoids
line splitting and accidental stdout log ambiguity. Reliable ordered viewer
input preserves key/button transitions; feedback can tolerate loss.

Before implementation is called conformant, exercise fragmented/coalesced pipe
records, version mismatch, response correlation, popup/active-target races,
resize with outstanding frames, dropped-frame mutex release, delayed encoder
completion, handle cleanup on crash, DPR mapping and non-ASCII committed text.
Phase 1 acceptance: create synthetic instance, connect viewer through trickle
ICE, see animated video, type/click, create/switch/resize/close two targets,
apply shaping, observe measured stats, and close with no retained resources.
