# PRTS status

## Current state
- Phase: 0 (design in progress; Chromium hook survey and Windows isolation design complete)
- Next action: Complete remaining Phase 0 designs, then implement Chromium milestones from chromium-hooks.md under orchestrator control.

## Log
- 2026-10-19: Added isolation.md after inspecting Chromium 158.0.8111.0 Windows sandbox/GPU policy and Microsoft API references. Covers job/AC/LPAC profiles, ACLs, handles, network enforcement and hardware-GPU acceptance gates. Outer AppContainer, nested jobs and Windows 10 hardware rendering remain untested runtime gates. Documentation checks and all 18 source-path checks passed. No Chromium build, source edits or commit; session usage metrics unavailable.
- 2026-10-19: plan written. Codex CLI 0.163.0 verified with `-m gpt-6.1-sol`.
- 2026-10-19: Added chromium-hooks.md after validating local Chromium 158.0.8111.0 at caef9db47e2cf3fe8e9f07ddceeff6ddf6957385 (matches PIN). Recommended ClientFrameSinkVideoCapturer with kPreferMappableSharedImage, DXGI NT handle duplication from browser into prtsd, keyed mutex and retained frame leases; documented Windows native-handle preference restriction and hidden HWND/DComp behavior. Read scripts/cr.ps1. Documentation only; no Chromium build, source edits, or commits.
