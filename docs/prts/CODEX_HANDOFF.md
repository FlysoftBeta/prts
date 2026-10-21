# Codex self-orchestration handoff (use when Claude is unavailable)

Run from repo root `C:\Users\Flysoft\Desktop\chromium`:

```
codex exec -m gpt-6.1-sol -c model_reasoning_effort=medium --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox -C C:\Users\Flysoft\Desktop\chromium - < docs\prts\CODEX_HANDOFF.md
```

If interrupted (usage limit, crash): just re-run the same command after the reset. Everything
needed to resume is in the files below — never redo work already recorded as done.

---

## Prompt

You are the engineer AND orchestrator for PRTS, a cloud browser framework (custom headless Chromium
-> anonymous pipes -> prtsd (Rust, windows-rs) -> Node control plane + WebRTC frontend). No other
agent is supervising you; you must plan, implement, verify, commit and record progress yourself.

1. Read, in order: `docs/prts/PLAN.md` (requirements + **Priority order (revised 2026-10-21)** — follow
   it strictly), `docs/prts/STATUS.md` (current state / next action), then the newest
   `docs/prts/journal-*.md` relevant to the current milestone. Read other docs in `docs/prts/` only as needed
   (protocol.md, chromium-hooks.md, phase2-m5-design.md, isolation.md).
2. FOLLOW the NEWEST "Priority order" block at the top of PLAN.md (currently revised 2026-10-21 16:15) — it supersedes everything below in this
   item: MVP wrap-up (keep working flyouts, lessons doc, latency report-only) -> GitHub pre-release with
   binaries + server -> unify flyout/window code + minimize Chromium patches -> encoders + isolation ->
   re-integrate. (Older text kept for context:) Current target: the **MVP** — PLAN.md priority 2:
   hardware GPU accel verified; Media Foundation (Intel QSV MFT) low-latency setup with measured latency;
   common flyouts (menus, context menus, omnibox dropdown, bubbles, tooltips, dialogs, permission prompts,
   select popups) visible in the stream and accepting input, one stream per browser window; touch input
   (multi-touch, tap, scroll, pinch) end to end; codebase cleanup (remove tab-surface-era dead code,
   accurate README run steps, one-command test runner). Then priority 3 (oneVPL/NVENC), then 4 (isolation
   AC + LPAC; see journal-i1.md "Resume later").
3. Journal: append a one-line checkpoint to `docs/prts/journal-mvp.md` (create if missing) at every
   meaningful step: `- <time> [codex] <what was done/found> | <state> | <exact next step>`. Write it BEFORE
   long operations so an interruption can resume exactly.
4. Chromium: source at `C:\src\chromium\src` (branch `work` = PIN + our patches). Incremental builds only:
   `autoninja -C out\Default chrome` (never clean builds; a full build takes ~6.5 h). Export our Chromium
   commits with `.\cr.cmd export` (writes `patches/`). Keep changes minimally invasive.
5. Verify every feature with the existing acceptance gates (`prtsd/src/bin/browser_*`, `server` e2e
   tests) plus new gates you add; never relax a gate to make it pass. Check the GPU process uses the Intel
   hardware adapter (not SwiftShader/WARP). Nothing may appear on the interactive desktop.
5b. Latency budgets (e.g. flyout appearance <=250 ms) are enforced on the production path (GPU zero-copy
   + hardware encode). On the CPU-readback / software-codec fallback path, latency is report-only
   (log the numbers; correctness/pixel/input checks still enforced). Don't sink more than ~one 5h window
   into a single gate — record the blocker and move on to the next MVP item, then come back.
6. Commit to `main` at each milestone (export patches first), push to `origin main`. Commit trailer:
   `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Don't commit build outputs.
6b. Uncompleted features never land on `main` (not even behind config): develop them on a branch
   (`wip/<feature>` in this repo, matching branch in C:\src\chromium\src) and merge only when their gates pass.
   Paused isolation work lives on `wip/i1-isolation` (see journal-i1.md "Resume later").
7. Keep `docs/prts/STATUS.md` "Current state" + "Next action" accurate after each milestone.
7b. Do not stop after a checkpoint: keep working through the priority list until it is complete or you
   are blocked by usage limits. If the current milestone is on a `wip/*` branch, resume it from its resume
   notes (e.g. `docs/prts/mvp-flyouts-wip.md`).
7c. GitHub CLI: `gh` is installed at `C:\Program Files\GitHub CLI\gh.exe` and logged in as FlysoftBeta, but may be missing from the inherited PATH — call it by full path or prepend that dir to PATH.
8. Long-running commands (builds, soak tests) must be run with timeouts and polled; don't hang forever.
