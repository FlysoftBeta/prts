# PRTS Windows instance isolation

Design date: 2026-10-19. Target: Windows 10 22H2 (build 19045), used as
the server OS; development on Windows 11 is not sufficient qualification.
This is a design and source review, not a record of a successful runtime test.
No Chromium build or launch was performed for this document. Hardware GPU,
outer AppContainer, and nested-job compatibility remain release gates below.

## Decision and trust boundary

Start with a per-instance outer Job Object and Chromium's existing child
sandboxes. Keep the outer job's UI restrictions at zero. Introduce a unique
AppContainer for the **whole browser tree** as an experimental second stage;
qualify ordinary AppContainer before attempting LPAC. Neither disabling the
Chromium sandbox nor accepting software graphics is a compatibility fix.

An instance contains one browser, its GPU/network/utility processes, and all
its tabs. Tabs in the same browser are not separate tenants. prtsd and the
Node control plane stay outside tenant jobs and containers. They are trusted
brokers: a browser compromise must not confer their process handles, files,
network authority, or another tenant's texture handles. Chromium renderer
isolation protects against web content; an outer boundary also needs to
contain a compromised browser/GPU process.

`job` alone provides lifecycle and resource control, **not a same-user tenant
security boundary**. Distinct profile directories do not change that. Until
the AppContainer profile passes qualification, use separate Windows accounts
with ACLs, or separate VMs for mutually hostile tenants. All profiles share
the Windows kernel and GPU driver; jobs do not partition GPU scheduling or
VRAM. GPU driver compromise and hard GPU availability isolation require a
stronger deployment boundary.

PRTS remains `--headless=new`, sends input through RenderWidgetHost, and
exports D3D11 textures through anonymous pipes to prtsd. There is no desktop
capture, DWM capture, OS input injection, or encoder inside the container.
QSV/software **encoding** selection is independent of hardware **rendering**:
software encoding in development must not disguise SwiftShader/WARP rendering.

## 1. Per-instance Job Object

### Launch and lifetime

1. `CreateJobObjectW(NULL, NULL)` creates an unnamed job. Retain a
   non-inheritable handle exclusively in prtsd; do not pass it to Node or
   Chromium. Set limits before any process joins.
2. Use `SetInformationJobObject(JobObjectExtendedLimitInformation)` with
   `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`. Always set
   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Neither
   `JOB_OBJECT_LIMIT_BREAKAWAY_OK` nor
   `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK` is allowed.
3. Create the browser with `CreateProcessW`, `CREATE_SUSPENDED`, and
   `EXTENDED_STARTUPINFO_PRESENT`; use `CreateProcessAsUserW` if launching
   under a separately provisioned user/token. Assign it using
   `AssignProcessToJobObject`, check success, then `ResumeThread`.
   `PROC_THREAD_ATTRIBUTE_JOB_LIST` is an alternative for assignment during
   creation on this OS. Never allow browser instructions to run before job
   membership is established. Terminate the suspended process on failure.
4. Children inherit membership. Verify browser, renderer, GPU, network and
   utility membership with `IsProcessInJob(process, instance_job)`; do not
   infer it from parent PIDs. Inspect any job inherited by prtsd itself
   (CI/service managers can install one), since its limits also apply.
5. On shutdown, request graceful browser exit with a bounded timeout, then
   `TerminateJobObject`. Closing the final job handle is the crash cleanup
   mechanism. A leaked/inherited job handle defeats kill-on-close. Keep the
   process handles required for waits, and close pipe ends deterministically.

Win32 semantics: [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects).

### Resource policy

Limits are configuration, not hard-coded capacity promises. An initial
qualification configuration could allow 64 processes, 4 GiB process commit,
8 GiB aggregate commit and 50% CPU; measure real multi-tab workloads and
scale down from a generous baseline.

| Control | API/fields | Operational meaning |
| --- | --- | --- |
| Aggregate memory | `JOB_OBJECT_LIMIT_JOB_MEMORY`, `JobMemoryLimit` | Caps job commit, not total resident memory or GPU VRAM. |
| Per-process memory | `JOB_OBJECT_LIMIT_PROCESS_MEMORY`, `ProcessMemoryLimit` | Allocation failure can kill a renderer/GPU; allow startup and restart peaks. |
| Process count | `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`, `BasicLimitInformation.ActiveProcessLimit` | Includes browser, spare renderers, site-isolated frames and utilities. A limit of 1 breaks Chromium. |
| CPU | `JobObjectCpuRateControlInformation`, `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION` | Use `JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP`; `CpuRate=5000` is 50%, in hundredths of a percent. CPU throttling can increase streaming latency. |
| Accounting | `QueryInformationJobObject` | Query basic accounting, process list, extended limits and peak memory; reconcile with actual process handles. |
| Events | `JobObjectAssociateCompletionPortInformation` with `CreateIoCompletionPort` / `GetQueuedCompletionStatus` | Observe process exits and limit violations; most job notifications are not guaranteed, so also query state. |

Use hard cap, weight, or min/max CPU modes deliberately; do not combine their
incompatible flags. A limit-setting failure is a launch failure for a profile
that promises that limit. Disk bytes, pipe queues, retained textures, prtsd
encoder allocations, bandwidth and GPU work need separate quotas. Job commit
does not include all resources retained by prtsd on the tenant's behalf.
See [extended limits](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_extended_limit_information)
and [CPU rate control](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_cpu_rate_control_information).

### UI restrictions and nested Chromium jobs

Do **not** set `JobObjectBasicUIRestrictions` on the outer job, including
seemingly harmless clipboard restrictions. Microsoft documents constraints
on forming nested jobs with UI limits. Chromium creates its own per-target
jobs, including restrictive renderer jobs; an outer job cannot be assumed
compatible simply because this OS supports nesting. Preserve Chromium's
policies, check every child launch, and test the actual hierarchy. Never
enable breakaway or disable Chromium's sandbox to repair failed assignment.
See [Nested Jobs](https://learn.microsoft.com/en-us/windows/win32/procthread/nested-jobs).

The GPU-specific reason is concrete: in [C2],
`GpuSandboxedProcessLauncherDelegate::InitializeConfig` selects
`JobLevel::kLimitedUser` but exempts `SYSTEMPARAMETERS`, `DESKTOP`,
`EXITWINDOWS`, and `DISPLAYSETTINGS`. Together with [C3]'s fallthrough logic,
this leaves its UI restriction mask zero. The source explicitly warns that
UI restrictions break access to windows outside the job. Imposing an outer
mask would undo that choice.

| `JOB_OBJECT_UILIMIT_*` | Effect and Chromium risk if applied to the whole tree |
| --- | --- |
| `HANDLES` | Restricts external USER handles/windows, hooks and broadcasts. This is not a restriction on kernel `DuplicateHandle` or DXGI NT handles. High compatibility risk for browser/GPU/driver window interactions. |
| `GLOBALATOMS` | Gives a job a separate global atom table; can break USER/OLE/driver integration. Renderer lockdown uses it; do not generalize to browser/GPU. |
| `READCLIPBOARD`, `WRITECLIPBOARD` | Break OS clipboard reads/writes, including pages needing those features. PRTS can broker an explicit tenant clipboard over its protocol instead. Still unsuitable as outer UI limits. |
| `DESKTOP` | Blocks desktop creation/switching. Can conflict with Chromium creating its own sandbox desktops. |
| `DISPLAYSETTINGS` | Blocks display mode changes. Not a direct D3D11-device ban, but Chromium explicitly removes this restriction for its GPU job. |
| `SYSTEMPARAMETERS` | Blocks system parameter changes; Chromium removes it for GPU compatibility. |
| `EXITWINDOWS` | Blocks logoff/shutdown APIs. Removing shutdown privileges from the base user is preferable to an outer UI mask. Chromium also exempts this for GPU. |

These are documented effects and source-based risks, not evidence that every
flag individually fails on every driver. [C3] intentionally uses restrictions
inside Chromium's own sandboxes; PRTS must not edit those policies as part of
adding an outer job. See the [UI restriction definitions](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_ui_restrictions).

## 2. Chromium sandbox layering

Source review is pinned to Chromium **158.0.8111.0**, git
`caef9db47e2cf3fe8e9f07ddceeff6ddf6957385`, in
`C:\src\chromium\src`. Paths [C1]–[C12] below are relative to that root.

* **Renderer:** [C1] installs low initial integrity, delayed untrusted
  integrity, `USER_RESTRICTED_SAME_ACCESS` startup and `USER_LOCKDOWN`
  permanent tokens, an alternate window station/desktop, default DACL
  lockdown, a lockdown job and Win32k lockdown. [C4] can add a renderer
  LowBox policy if the embedder supplies a SID. `kRendererAppContainer` is
  disabled by default in [C5]; it is not an outer-browser switch.
* **GPU:** [C2] overrides the default token/desktop policy, uses startup
  `USER_RESTRICTED_SAME_ACCESS`, permanent `USER_LIMITED`, low integrity
  (possibly delayed for desktop initialization), and the special job above.
  The source says `USER_RESTRICTED` breaks both DirectX and OpenGL. Do not
  replace this with renderer lockdown or apply renderer Win32k lockdown to
  the whole tree.
* **AppContainer support:** [C1]'s `SetupAppContainerProfile` and
  `IsAppContainerEnabledForSandbox` enable profiles for selected services,
  including network and media CDM, **not GPU**. [C5] has no
  `kGpuAppContainer`; searches of `sandbox`, `content`, `gpu`, `chrome`,
  `services`, and `base` found no such symbol. Do not copy a
  `--enable-features=GpuAppContainer` recipe from another revision and treat
  it as effective. [C6] gates supported AppContainer sandbox operations at
  Windows 10 RS5, which 22H2 satisfies.
* **Network:** this revision's policy adds `internetClient`,
  `privateNetworkClientServer`, `enterpriseAuthentication`,
  `lpacIdentityServices`, and `lpacCryptoServices` to its network profile.
  `kWinSboxNetworkServiceSandboxIsLPAC` defaults off [C1, C5]. This internal
  profile does not establish tenant network isolation for the browser.
* **Sandbox bypass:** `--no-sandbox` takes the unsandboxed launch path [C1];
  `--disable-gpu-sandbox` changes the GPU delegate's type to `kNoSandbox`
  [C2]. Both are forbidden for tenant profiles. An outer job/AppContainer
  does not replace renderer-to-browser isolation.

Whole-browser AppContainer is **not supported merely by stacking launch
attributes**. Child token creation begins with the current browser token
([C7], [C8]); some children then request new LowBox/profile identities,
impersonation tokens, jobs and alternate desktops. Windows AppContainers
are not arbitrarily nestable containers, and Chromium's service identities
must not escape or collapse the tenant boundary. Expect possible token,
desktop, IPC or process-creation failures. Validate each child token's
effective authority, not just the browser's `TokenIsAppContainer` bit.

If this cannot work with the existing broker, stop promotion of the profile.
A future Chromium change would need to preserve tenant identity/capability
restrictions while creating child sandboxes, or use a tightly scoped external
launcher broker. Such a broker is a separate security design, not permission
to hand out unrestricted process creation or a prtsd process handle. No such
source change is included here.

Recommended layering: non-admin launch identity, immutable Chromium files,
per-instance job, Chromium's existing child policies, per-instance state and
pipe allowlist; then a qualified outer AppContainer plus enforced egress.
LPAC follows only after ordinary AppContainer passes all the same tests.

## 3. Browser AppContainer / LPAC launch profile

### Identity, attributes and ACLs

Use `CreateAppContainerProfile` with a generated name such as
`prts.<opaque-instance-id>` under the intended launch user. On
`HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`, use
`DeriveAppContainerSidFromAppContainerName` only for a profile whose ownership
and lifecycle prtsd recognizes; do not adopt attacker-selected names. A unique
SID per concurrent instance simplifies state and egress rules. Persist only
when deliberate tenant persistence is required. Delete with
`DeleteAppContainerProfile` after exit and broker resource cleanup.

Build `SECURITY_CAPABILITIES` with `AppContainerSid`, enabled
`SID_AND_ATTRIBUTES` entries (`SE_GROUP_ENABLED`), and `Reserved=0`.
Derive named capabilities with `DeriveCapabilitySidsFromName`, or use
`CreateWellKnownSid` for well-known ones. Keep all referenced buffers alive
through `CreateProcessW`. Add `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`
using `InitializeProcThreadAttributeList` / `UpdateProcThreadAttribute`.
For LPAC additionally set
`PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY` to
`PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT`; this opts out of ambient
`ALL APPLICATION PACKAGES` access. Chromium implements the same attributes
in [C9]. Free the attribute list and SID allocations with their documented
matching APIs. See [Microsoft's launch recipe](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer).

| Resource or capability | Initial policy and qualification |
| --- | --- |
| `internetClient` | Needed by the process actually opening public-network sockets for normal browsing. Grant only where required, and still enforce destination policy. Anonymous pipes need no network capability. |
| `privateNetworkClientServer`, `internetClientServer`, `enterpriseAuthentication` | Do not grant to the outer browser by default. LAN access, inbound listeners and integrated enterprise authentication are separate opt-ins. Chromium's internal network profile grants some of these; verify effective child capabilities and WFP coverage. |
| `registryRead` | LPAC candidate for registry discovery, including driver/runtime settings. Does not authorize arbitrary registry writes. Ordinary AppContainer has broader ambient system-read access. |
| `lpacCom` | LPAC candidate only if observed COM activation requires it; verify the actual COM server and its policy. Not a blanket bypass of all COM ACLs. |
| `lpacPnPNotifications` / `lpacPnpNotifications` | GPU/driver-discovery candidate, not a documented universal D3D11 requirement. [C10] contains both spellings; record exactly which name/SID is tested. Do not add both speculatively. |
| `lpacMedia`, `lpacServicesManagement`, `lpacSessionManagement`, `lpacInstrumentation`, `lpacAppExperience` | Present in Chromium's media CDM LPAC policy. They are not a proven GPU capability bundle. Add one only against an observed denied operation and rerun isolation tests. prtsd's QSV encoder does not require granting these to Chromium. |
| `lpacIdentityServices`, `lpacCryptoServices`, `lpacEnterprisePolicyChangeNotifications` | Candidates for specific network/crypto/policy services, as in [C1]. Do not inherit the entire media/network capability list into the browser. |
| `chromeInstallFiles`, `lpacChromeInstallFiles` | Chromium-defined capability names, not automatic read permission. [C1, C10] use them with matching installation ACLs. Prefer a per-instance package-SID ACE for PRTS's outer browser; shared install capabilities must never authorize tenant state. |

File authorization must satisfy both normal user/group access and the
AppContainer/capability side of the check. Provision ACLs using
`GetNamedSecurityInfoW`, `SetEntriesInAclW`, and `SetNamedSecurityInfoW`:

* `--user-data-dir=<absolute per-instance path>`, cache, downloads and private
  `TEMP`/`TMP`: grant launch user and that instance's package SID required
  modify rights, with container/object inheritance. Retain SYSTEM/admin and
  trusted broker access; avoid granting tenants `WRITE_DAC`/`WRITE_OWNER`.
  Use a low mandatory label for data that the low-IL process must write
  (`LABEL_SECURITY_INFORMATION`, e.g. `S:(ML;OICI;NW;;;LW)`). A DACL grant
  alone need not overcome mandatory integrity checks. [C11] also labels its
  container data directory low.
* Chromium install: grant read/execute and directory traversal to the user
  and outer package SID, never write. Cover all component DLLs, resources,
  locales and runtime dependencies in this component build. Chromium's
  internal child profiles may require the install capability ACEs above as
  well; an outer package SID does not substitute for every child identity.
* Keep the install's mandatory label compatible with Chromium's image-load
  mitigations: do not mark executable DLL directories low just to make data
  writes work. Keep writable caches outside the installation.
* Registry: use `GetAppContainerRegistryLocation` for container-owned state
  where applicable. Explicitly grant only required keys/rights; never make
  HKCU/HKLM generally writable. Registry redirection is not a substitute for
  inspecting actual accesses by an unpackaged browser. LPAC may need
  `registryRead` even for normal discovery; use ProcMon/ETW to identify the
  actual failures and avoid modifying system/driver ACLs globally.

See [AppContainer token and child inheritance](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-for-legacy-applications-).
Do not grant all tenants' state to `ALL APPLICATION PACKAGES`,
`ALL RESTRICTED APPLICATION PACKAGES`, `Everyone`, or a shared capability.

### GPU access and failure diagnosis

D3D11 hardware rendering is possible in AppContainer; this does not prove
that an arbitrary Win32 browser broker and vendor driver work inside LPAC.
There is no universal `gpu`/`directX` capability to add to this recipe. No
inspected Chromium policy provides a validated GPU LPAC capability list.
Use ordinary AppContainer first, then discover LPAC requirements on the
actual Windows 10 image and driver. `registryRead`, possibly `lpacCom` and
the PnP capability above are investigation candidates, not guarantees.

Build a later probe running with the **same token, attributes, desktop and
job** as the target process. Enumerate through `CreateDXGIFactory1` and
`IDXGIFactory1::EnumAdapters1`; inspect `DXGI_ADAPTER_DESC1` description,
vendor/device IDs, LUID, and `DXGI_ADAPTER_FLAG_SOFTWARE`. Try
`D3D11CreateDevice` on the chosen hardware adapter with
`D3D_DRIVER_TYPE_UNKNOWN` (required for an explicit adapter), or use a null
adapter with `D3D_DRIVER_TYPE_HARDWARE`. Never retry with WARP as a successful
probe. See [device creation](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-d3d11createdevice).

Record the separate HRESULTs from DXGI factory creation, enumeration, D3D11
device creation, texture creation, handle export and import. Known failure
classes include inaccessible registry/driver files or device interfaces in
LPAC, COM/PnP initialization, low-IL desktop access, wrong adapter selection,
device removal and unsupported sharing. An empty output/monitor list is not
the same as failure to enumerate render adapters. Session 0, RDP reconnects,
and vendor driver changes also need qualification. Do not assume an
interactive-session Windows 11 result applies to a Windows 10 service.

[C2] documents desktop/token sensitivity; [C12] shows ANGLE's actual DXGI
enumeration, non-Microsoft adapter preference, and explicit WARP selection.
If regular AppContainer works and LPAC fails, retain regular AppContainer
while investigating access-denied events. If hardware rendering fails, mark
that profile unavailable; never silently widen permissions or switch graphics
to software. A successful standalone probe is necessary but does not prove
Chromium selected that device.

### Anonymous pipes

`CreatePipe` with `SECURITY_ATTRIBUTES.bInheritHandle=TRUE` creates the two
directions of PRTS traffic. Make prtsd's ends non-inheritable with
`SetHandleInformation(HANDLE_FLAG_INHERIT, 0)`. Put **only** the child ends
and explicitly selected logging handles in `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`;
use real inheritable handles, `bInheritHandles=TRUE`, and
`STARTF_USESTDHANDLES` only when actually mapping stdio. PRTS browser handles
can instead be passed as explicit startup handle values. Set unused stdio
deliberately; do not inherit Node/prtsd's control-plane stdout by accident.

Combine this allowlist with the AppContainer attributes. A preopened,
inherited handle conveys authority across the boundary; AppContainer is not
a reason to permit broad inheritance. Test `ReadFile`/`WriteFile`, EOF and
cancellation under both AC and LPAC. [C9] builds both attributes together.
See [startup attribute requirements](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute).

Close prtsd's copies of child ends immediately after successful launch to
ensure EOF works. Prevent concurrent process launches from accidentally
inheriting another instance's endpoints. Chromium must explicitly manage any
endpoint passed onward to its GPU process; ordinary browser inheritance does
not automatically add it to Chromium's own child handle allowlist. Do not
propagate all instance control pipes to every renderer. Frame lengths,
message types, quotas and handle ownership are checked in prtsd; received
input and tenant identifiers are never trusted solely because they arrived
over a local pipe.

### Shared texture NT handles

Create export textures with `D3D11_RESOURCE_MISC_SHARED_NTHANDLE` and, for
the initial synchronization design, `D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX`.
Use `IDXGIResource1::CreateSharedHandle` with no global name and explicit
non-inheritable security attributes. Grant only the required
`DXGI_SHARED_RESOURCE_READ` / `DXGI_SHARED_RESOURCE_WRITE` rights. The old
`IDXGIResource::GetSharedHandle` result is not an NT handle and must not be
passed to `DuplicateHandle`/`CloseHandle` as one. See
[NT texture sharing](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgiresource1-createsharedhandle).

The design must satisfy "NT handles duplicated into prtsd" without giving
the browser `PROCESS_DUP_HANDLE` access to prtsd:

1. Keep texture handles in a known producing process (normally Chromium GPU).
   Through Chromium's existing broker/IPC, identify its actual process handle
   and lifetime. prtsd must authenticate that it belongs to the instance/job;
   a pipe-supplied PID alone is insufficient and can be reused.
2. prtsd retains/obtains `PROCESS_DUP_HANDLE` on that source process and calls
   `DuplicateHandle(source_process, source_texture_handle,
   GetCurrentProcess(), ..., FALSE, ...)` to pull a duplicate into itself.
   Alternatively Chromium can broker the texture handle into the browser
   first, and prtsd pulls from that known browser process. Either form needs
   an explicit PRTS protocol ownership/acknowledgement handshake.
3. The source keeps the handle alive until acknowledgement; do not use
   `DUPLICATE_CLOSE_SOURCE` on an untrusted supplied integer. Never interpret
   a received integer as an already-owned handle in prtsd's handle table.
   Reject pseudo-handles and prevent generic handle-broker requests.
4. Import with `ID3D11Device1::OpenSharedResource1` on prtsd's selected adapter;
   validate resource interface, format, size, bind/misc flags and outstanding
   texture quota before encoding. Bound keyed-mutex waits and release all
   duplicates on timeout, browser crash and instance destruction. A malicious
   producer can mutate shared contents: validate metadata and avoid using
   shared memory as trusted control structures.

AppContainer does not categorically prohibit an outside broker pulling an
existing handle; it also does not authorize opening the source process or
named resources. Provision process access deliberately under the launch
identity, and do not grant tenants access to the broker. DACLs matter when
opening objects; possession of an already-open handle is itself authority.
See [DuplicateHandle access requirements](https://learn.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-duplicatehandle).

Match producer and prtsd D3D11 adapter LUIDs. QSV may need textures on Intel;
if Chromium runs on a different adapter, choose a compatible adapter or
document a measured cross-adapter/copy path. Do not call that zero-copy.
Successful sharing alone is not hardware proof: ANGLE notes WARP-to-WARP
sharing support [C12]. [C13] demonstrates Chromium importing an NT handle
with `OpenSharedResource1`; its CPU staging-copy helper is not the PRTS fast
path. Test keyed mutex/fence synchronization independently of handle access.

## 4. Restricted tokens, integrity, window stations and desktops

Use a dedicated non-admin launch identity. An optional restricted-token
experiment can use `OpenProcessToken`, `DuplicateTokenEx`,
`CreateRestrictedToken(DISABLE_MAX_PRIVILEGE, ...)`, and
`SetTokenInformation(TokenIntegrityLevel, TOKEN_MANDATORY_LABEL)` before
`CreateProcessAsUserW`. Record the remaining privileges, deny-only SIDs,
restricted SIDs and integrity label. Required launch privileges depend on the
token/account and API; do not solve failures by enabling `SeDebugPrivilege`
for tenant processes or running the browser as SYSTEM.

Low integrity mainly restricts writes upward and UIPI interactions; it does
not stop same-user reads or create a tenant identity. Restricting SIDs can
provide an additional ACL check, but every required resource then needs a
matching grant and browser sandbox brokerage may fail. Chromium already
constructs carefully differentiated renderer/GPU tokens [C7, C8]. Do not
apply `USER_LOCKDOWN`, untrusted IL, or all renderer mitigations to the outer
browser. Start `job` at the ordinary non-admin token; test any browser token
restrictions as a separate recorded experiment. AppContainer normally runs
at low IL and is a separate property from restricted-token status.

`CreateDesktopW` creates a desktop on the calling process's window station;
`CreateWindowStationW` creates a separate station. Launch using
`STARTUPINFO.lpDesktop=L"<station>\\<desktop>"`. A dedicated launcher helper
can select the station with `SetProcessWindowStation`; avoid switching the
station of a multithreaded prtsd process. Set DACLs for the launch user,
AppContainer identity where needed, and Chromium child tokens; account for
low-IL `DESKTOP_READOBJECTS`/`DESKTOP_WRITEOBJECTS`. Do not call
`SwitchDesktop` or `SetThreadDesktop` on existing UI threads as a production
shortcut. Close desktops/stations after all associated processes exit.
See [CreateDesktopW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-createdesktopw).

Separate desktops reduce window-message/hook interaction; a separate window
station also separates clipboard/global-atom scope. Neither isolates files,
network, kernel or GPU memory, and both consume desktop heap. Chromium
already uses alternate stations for some targets. Its GPU policy explicitly
depends on desktop access [C2]; an added station can break USER initialization,
vendor driver helpers or device creation. Offscreen D3D11 has no inherent
requirement to display a window, but that does not prove every Chromium or
driver initialization path works on a private desktop. Keep the default
desktop arrangement initially; qualify a private desktop, then a private
station independently with the GPU tests. No visible/focused window or OS
input is allowed in any profile.

## 5. Tenant state and network policy

Allocate user-data-dir, disk cache, downloads, logs and temp paths under a
broker-controlled root using opaque IDs, never tenant-provided raw paths.
Do not share profile locks, cookies, service workers, extension state or
credential stores across instances. Avoid exposing host extensions or host
authentication. Sanitize the environment and working directory. Treat
profile contents, reparse points and cleanup paths as untrusted: resolve and
verify containment before deletion, and never recursively follow a tenant
junction into another directory. Retention/restore must preserve tenant ACLs.

The non-AppContainer `job` profile needs separate Windows users with private
ACLs if browser compromise must not read other instances. Identical user
tokens with different directories are not enough. For AppContainer enforce
per-instance package-SID grants as above, and test both normal user and
package access checks. Shared internal Chromium service SIDs/capabilities
must not gain all tenants' writable data.

Network choices, in order of enforcement strength:

* **WFP with AppContainer identity:** a privileged provisioning service uses
  `FwpmEngineOpen0`, `FwpmTransactionBegin0`, `FwpmSubLayerAdd0`,
  `FwpmFilterAdd0` and `FwpmTransactionCommit0`. Match
  `FWPM_CONDITION_ALE_PACKAGE_ID` as `FWP_SID` at
  `FWPM_LAYER_ALE_AUTH_CONNECT_V4/V6`; cover inbound/listen policy as needed.
  Install rules before resuming the browser. Deny unwanted LAN, loopback,
  link-local/metadata and tenant subnets and permit only policy destinations.
  Account for IPv6, UDP/QUIC and DNS, not only TCP/443. Bind rule lifetime to
  instance cleanup; policy-service failure must not open egress. WFP package
  SID matching is documented in [condition types](https://learn.microsoft.com/en-us/windows-hardware/drivers/network/filtering-condition-data-types)
  and [layer availability](https://learn.microsoft.com/en-us/windows/win32/fwp/filtering-conditions-available-at-each-filtering-layer).
* **Identity caveat:** Chromium may launch its network service under its own
  AppContainer SID. Capture the actual socket-owning process/token and verify
  which package SID WFP classifies. A filter only on the outer browser SID
  may miss all traffic. If different instances share the internal service
  SID, package-only filters cannot distinguish them. Promotion requires
  unique constrained service identities, or another enforced identity such
  as a separate launch user. Do not assume child identities form a WFP
  wildcard hierarchy. Browser/GPU/utility egress must also be covered.
* **Proxy:** configure a per-instance authenticated forward proxy using
  `--proxy-server=...`. Enforce at WFP that sockets can reach only the proxy
  and required resolvers. Address loopback bypass rules, PAC, DNS resolution,
  QUIC, WebRTC UDP/STUN and direct sockets. Chromium proxy configuration
  alone is not a boundary after browser compromise. Prefer a reachable
  isolated proxy over machine-wide AppContainer loopback exemptions.
* **No AppContainer:** same-executable `ALE_APP_ID` rules cannot distinguish
  tenants. A separate user SID with `ALE_USER_ID` policy or a VM gives an
  enforceable identity. A job ID is not a general substitute for an ALE
  package/user condition.

prtsd's WebRTC and control-plane sockets are outside these rules. They must
authenticate tenant/session routing and cannot offer a generic URL-fetch or
socket service that lets Chromium bypass egress controls. Use a separate
privileged network-policy provisioner rather than grant firewall-management
rights to Chromium or the Node control plane.

## 6. Staged profiles and acceptance gates

| Profile | Enabled layers | Promotion rule |
| --- | --- | --- |
| `none` | Normal non-admin browser, Chromium child sandbox, unique state and strict pipe inheritance | Developer diagnostics only; still forbid software graphics in the hardware qualification run. `none` means no **outer** isolation, not `--no-sandbox`. |
| `job` | `none` plus per-instance outer job, kill-on-close, tested resource limits, zero outer UI mask | Establish hardware baseline and reliable nested sandbox launches/cleanup. Not sufficient for hostile same-user tenants. |
| `appcontainer` | `job` plus unique outer AppContainer SID, ACLs, constrained capabilities and enforceable network identity | Experimental until every child policy and all GPU/egress/negative tests pass on Windows 10 22H2. Fail closed when requested and unsupported. |
| `appcontainer` with LPAC sub-option | Above plus all-packages opt-out and audited LPAC capabilities | Separate qualification after ordinary AppContainer; record LPAC status explicitly. Never silently downgrade to ordinary AC. |

Implement the future launcher in that order. Add GPU/process evidence first,
then job limits, then ordinary AppContainer, then LPAC, then private desktop
experiments. A failed `appcontainer` request returns an isolation error; an
operator may explicitly choose another profile in a development environment.

### Verification checklist (required, not yet executed)

1. **Record the environment.** Capture OS build, driver versions, physical
   adapter IDs/LUIDs, session ID/service account, Chromium revision/build
   configuration, command line and selected isolation/capabilities. Repeat
   on Windows 10 22H2, not only Windows 11; include service/RDP conditions
   used in deployment. Test two concurrent instances.
2. **Prove effective sandbox state.** Inspect `OpenProcessToken` /
   `GetTokenInformation` for `TokenIsAppContainer`, `TokenAppContainerSid`,
   `TokenCapabilities`, `TokenIntegrityLevel`, `TokenRestrictedSids` and
   privileges on browser/GPU/renderer/network/utility processes. Check LPAC
   with `TokenIsLessPrivilegedAppContainer` where supported. Check job
   membership and process mitigations with `GetProcessMitigationPolicy`.
   Exercise renderer, network and GPU restart; no child sandbox error,
   bypass switch, breakaway or shared tenant authority is acceptable.
3. **Expose a PRTS GPU-info message.** Proposed `gpu.info` request/event
   (not an implemented protocol claim) should report instance ID, GPU PID
   and generation, selected backend, active `gpu::GPUInfo` fields including
   vendor/device, driver version, `gl_renderer`, `sandboxed`, and Windows
   LUID, plus `GpuFeatureInfo.status_values`. [C14] supplies accessors.
   Read the **active** GPU info, not just `GetGPUInfoForHardwareGpu`, which
   can describe available hardware while active rendering uses software.
   Also report actual producer D3D11 adapter description/LUID/software flag
   from its device (`IDXGIDevice::GetAdapter` -> `IDXGIAdapter1::GetDesc1`).
4. **Collect logs while instrumentation is added.** Launch a qualification
   run with `--headless=new --use-angle=d3d11 --enable-logging=stderr
   --v=1 --vmodule=gpu*=2,gl*=2` plus a private user-data-dir. Capture stderr
   on a separate pipe or a private log file; never mix logs into framed
   control stdout. If this build's logs omit adapter/backend selection,
   add the GPU-info instrumentation before declaring a pass. Flags request
   D3D11; flags and a live GPU PID are not evidence that it succeeded.
5. **Reject software rendering explicitly.** Require ANGLE D3D11 on an
   enumerated physical adapter with expected vendor/device/LUID and
   `DXGI_ADAPTER_FLAG_SOFTWARE` clear. Reject renderer/adapter strings
   containing SwiftShader, WARP, Microsoft Basic Render Driver or software
   rasterizer, and reject software/disabled/blocklisted required feature
   statuses. WARP is Direct3D, so a `Direct3D11` string alone is insufficient.
   Do not use `--disable-gpu`, `--use-angle=swiftshader`,
   `--use-angle=d3d11-warp`, `--enable-unsafe-swiftshader`, or sandbox bypass
   switches. Do not use `--ignore-gpu-blocklist` to make an acceptance test
   pass. A diagnostic status lacking sufficient evidence is `unknown`, not
   `hardware`. Confirm failure detection with a separate deliberate software
   run. See [WARP device creation](https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-devices-create-warp).
6. **Exercise the real frame path.** Render animated Canvas/WebGL and
   compositor content, multiple tabs and input. Export/import actual NT
   textures across the sandbox boundary, compare producer/importer LUIDs,
   observe synchronization and continuously encode/stream. Record fast-path
   versus copy/readback counts. Verify QSV hardware-encoder selection
   separately; an encoded stream alone proves neither hardware rendering
   nor zero-copy. Repeat after GPU reset/restart and resize.
7. **Verify enforcement and failure behavior.** From a controlled test
   process with the profile's token, attempt another tenant's files, temp,
   registry state, pipes, process handles and textures; require denial.
   Test LAN/metadata/loopback/direct-UDP/IPv6/proxy bypass and capture WFP
   identity evidence. Test memory/process/CPU limits, daemon crash,
   kill-on-close, pipe EOF, handle leaks, malformed frame messages and
   texture exhaustion. Node/prtsd must survive and other instances must
   remain usable. Do not equate a navigation failure with proved egress
   enforcement.
8. **Gate readiness continuously.** Report a profile ready only after the
   active GPU checks and frame handshake pass. Revalidate on GPU generation
   changes and driver/OS/Chromium upgrades. Store evidence with the instance
   result. Refuse or stop instances that fall back to software; do not
   advertise hardware readiness based on stale startup information.

## windows-rs implementation map

Use the official `windows` crate. Module paths below correspond to Cargo
features with `::` replaced by `_` (for example `Win32_System_JobObjects`).
Enable cross-module dependencies needed by the selected functions; pin and
compile-check the chosen crate version when implementing the launcher.

| `windows::` module / feature suffix | Main use |
| --- | --- |
| `Win32::Foundation` | `HANDLE`, errors, `CloseHandle`, `DuplicateHandle`, `SetHandleInformation`. |
| `Win32::System::JobObjects` | Job creation, limits, accounting, membership and termination. |
| `Win32::System::Threading` | `CreateProcessW`, `CreateProcessAsUserW`, `STARTUPINFOEXW`, startup attributes, process/token opening, mitigation queries, waits/resume. |
| `Win32::System::Pipes`, `Win32::Storage::FileSystem`, `Win32::System::IO` | Anonymous pipes, `ReadFile`/`WriteFile`, IO completion and cancellation support. |
| `Win32::Security`, `Win32::Security::Authorization` | Tokens/SIDs, `SECURITY_CAPABILITIES`, restricted tokens, ACLs and security descriptors. |
| `Win32::Security::Isolation` | `CreateAppContainerProfile`, derive/delete profile, profile/registry location APIs. |
| `Win32::System::Registry` | Explicit key access and provisioning. |
| `Win32::System::StationsAndDesktops` | Desktop/station creation, selection and closing. |
| `Win32::Graphics::Direct3D`, `Win32::Graphics::Direct3D11` | Device creation, textures, `ID3D11Device1::OpenSharedResource1`. |
| `Win32::Graphics::Dxgi`, `Win32::Graphics::Dxgi::Common` | Adapter enumeration, LUID/software classification, shared NT handles/keyed mutexes. |
| `Win32::NetworkManagement::WindowsFilteringPlatform` | WFP engine, filters, layers and conditions. |
| `Win32::System::Memory`, `Win32::System::Environment` | Matching allocation cleanup and launch-environment helpers where used. |

Binding references: [AppContainer APIs](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/Security/Isolation/index.html),
[desktop APIs](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/System/StationsAndDesktops/index.html),
[D3D11 device creation](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/Graphics/Direct3D11/fn.D3D11CreateDevice.html).

## Inspected Chromium sources

All paths refer to the pinned checkout above. Symbols are included because
line numbers move between Chromium revisions.

* **[C1]** `sandbox/policy/win/sandbox_win.cc`:
  `AddDefaultConfigForSandboxedProcess`, `GenerateConfigForSandboxedProcess`,
  `SetupAppContainerProfile`, `AddAppContainerPolicy`,
  `IsAppContainerEnabledForSandbox`, `StartSandboxedProcess`.
* **[C2]** `content/browser/gpu/gpu_process_host.cc`:
  `GpuSandboxedProcessLauncherDelegate::InitializeConfig`,
  `CanLowIntegrityAccessDesktop`, `ShouldSetDelayedIntegrity`, `GetSandboxType`.
* **[C3]** `sandbox/win/src/job.cc`: `Job::Init`, `SetActiveProcessLimit`.
* **[C4]** `content/browser/renderer_host/renderer_sandboxed_process_launcher_delegate.cc`:
  `RendererSandboxedProcessLauncherDelegateWin::InitializeConfig`.
* **[C5]** `sandbox/policy/features.cc`: renderer AppContainer and network
  LPAC feature declarations/defaults; also inspected `features.h`.
* **[C6]** `sandbox/features.cc`: `IsAppContainerSandboxSupported`.
* **[C7]** `sandbox/win/src/sandbox_policy_base.cc`: `PolicyBase::MakeTokens`,
  `InitProcess`; `sandbox/win/src/restricted_token.cc`:
  `RestrictedToken::GetRestrictedToken`.
* **[C8]** `sandbox/win/src/restricted_token_utils.cc`: `CreateRestrictedToken`;
  `sandbox/win/src/app_container_base.cc`: `BuildPrimaryToken`,
  `BuildImpersonationToken`.
* **[C9]** `sandbox/win/src/startup_information_helper.cc`:
  `SetAppContainer`, `AddInheritedHandle`, `BuildStartupInformation`;
  `sandbox/win/src/broker_services.cc`: `PreSpawnTarget`, `CreateSandboxProcess`.
* **[C10]** `sandbox/policy/win/lpac_capability.h`: exact system and
  Chromium-defined capability strings.
* **[C11]** `sandbox/win/src/app_container_base.cc`:
  `CreateAppContainerDirectory`, `CreateProfile`, `GetSecurityCapabilities`.
* **[C12]** `third_party/angle/src/libANGLE/renderer/d3d/d3d11/Renderer11.cpp`:
  `callD3D11CreateDevice`, adapter initialization and WARP sharing comments.
* **[C13]** `gpu/ipc/common/dxgi_helpers.cc`: `CopyDXGIBufferToShMem`,
  `OpenSharedResource1` import and keyed-mutex helpers.
* **[C14]** `gpu/config/gpu_info.h`, `gpu/config/gpu_feature_info.h`,
  `content/browser/gpu/gpu_data_manager_impl.h`: active GPU identity,
  feature statuses and GPU-info accessors.
