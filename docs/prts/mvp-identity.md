# MVP identity and branding (2026-10-21)

Application identity uses Chromium's standard `chrome/app/theme/chromium/BRANDING`
and branded strings in `chrome/app/chromium_strings.grd` (including
`settings_chromium_strings.grdp`): PRTS product names,
About/version labels and window-title strings. Original Chromium author and
copyright attribution remains intact. No Google-branded assets are substituted.

Web identity is independent of application branding. The Windows hook in
`components/embedder_support/user_agent_utils.cc` omits the headless UA prefix
and supplies `Google Chrome` to the existing brand/GREASE/permutation generator.
Pages, workers and service workers all use the same existing metadata pipeline;
there are no JavaScript overrides or per-field network-header patches.

For PIN version158.0.8111.0 the reduced UA is:

```
Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Safari/537.36
```

The normal Chrome brand ordering for milestone158 is Chromium158,
Not.A/Brand24, Google Chrome158; the full versions are158.0.8111.0 and
GREASE24.0.0.0. Platform is Windows, mobile=false, architecture=x86,
bitness=64, wow64=false. Platform version is supplied by Chromium's existing
Windows UniversalApiContract implementation (19.0.0 on this machine).

The production pipe-launch gate `server/test/e2e-identity.js` collects HTTP
requests and JS values from a page, dedicated worker, shared worker and service
worker. It checks every UA, every JS low/high entropy field, page HTTP Client
Hints, webdriver=false, window.chrome, the standard PDF plugin/MIME lists and
nonzero consistent screen/window sizes. The existing logical-root-size property
now also supplies DOM outer dimensions, excluding invisible native resize
borders: inner960x553, outer960x640, screen960x640 at DSF1.

## Worker HTTP Client Hints

Worker fetches and worker-script requests normally omit HTTP UA Client Hints
in this Chromium version. `WorkerFetchContext::PrepareRequest` sets User-Agent;
`UpgradeResourceRequestForLoader` does not add Client Hints. That operation lives
in `FrameFetchContext::AddClientHintsIfNecessary`. Worker JS `userAgentData`
still exposes the same metadata as pages. The gate explicitly requires the
normal absent worker headers, rather than skipping their validation or injecting
extra headers that a real browser would not send.

An initial gate incorrectly required page-style hints on all worker requests.
The independent stock Edge155 reference reproduces their absence on all three
worker script/report routes. PRTS158 has the identical presence pattern, with
the expected Chrome UA and complete consistent JS brands in every context.
Evidence: `C:/src/chromium/prts-mvp-identity-reference.*` and
`prts-mvp-wrap45-identity-headers.*`; the final corrected-gate results are recorded
in journal-mvp.md. All page-header value assertions and all worker JS assertions
remain exact.

`server/test/e2e-branding.js` checks executable version resources and reads the
internal version/About pages. These checks are separate from the production
identity launch, which has no automation or remote-debugging switches. Release
archives must repeat both gates using the extracted executables.

## Worker-fixture close regression

The exact identity assertions exposed a separate instance-close deadline bug.
Video joined in14ms and Chromium acknowledged SHUTDOWN in2ms, but the process
wait object remained unsignaled. TerminateJob succeeded, GetExitCodeProcess
reported0, and another TerminateProcess returned access denied (already
terminated). All recorded browser descendants had exited; waiting another full
five seconds in Session destruction caused the control RPC to time out.

The two-second daemon grace and terminal-status checks were temporary diagnostic
workarounds. Full46 reproduced the same hang in raw/native lifecycle gates.
Chromium pipe cancellation was queued with CONTINUE_ON_SHUTDOWN, which permits
process exit before the cancellation worker finishes. Build47 uses
BLOCK_SHUTDOWN for that bounded cancellation/join task. The unchanged raw v1
startup/shutdown probe passes (`prts-mvp-raw-shutdown47-resume.*`).

Both daemon workarounds have been removed. Full acceptance must pass with the
original five-second native shutdown wait and unchanged control RPC deadline;
the identity gate additionally checks every recorded browser descendant exits.
Earlier diagnostic evidence: `prts-mvp-close-api.*` and
`prts-mvp-identity-qualified.*`. Final qualification is recorded in STATUS.md
and journal-mvp.md.