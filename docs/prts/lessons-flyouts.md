# Flyout lessons (MVP, 2026-10-21)

The revised 07:30 plan accepts the working app menu, page context menu, native
select popup and omnibox suggestions. Their gates exercise decoded video and
real input at DSF 1, 2 and 1.25, over CPU/software and GPU/QSV, with one and two
negotiated capture credits. Other flyouts, independent tracks for additional
windows, and touch gestures remain unqualified; they are not release claims.

## What worked

Views popups can live in separate Aura roots. Track their owner and geometry,
capture each source with premultiplied alpha, and compose privately in prtsd
before encoding. The CPU reference and Intel D3D11 implementation agree within
one byte. GPU composition feeds QSV without reading pixels back. Per-source
caches have a 512 MiB aggregate bound; credits return after a private copy,
so slow consumers cannot hold Chromium's two export resources hostage.

Fair scheduling across sources matters with a one-credit handshake. Repeated
base frames must not starve a popup; alpha, shadows, clipping and the exact base
restoration on close need explicit pixel assertions. Merely observing a popup
widget or a changed frame does not prove it appeared in the encoded stream.

Input gates must select actual commands: New tab creates a selected tab; Reload
reruns page script; select commits a blue option and page; an omnibox row click
navigates to a blue fixture. Aura mouse release must carry the changed button
even though wire `buttons` describes the held state after release. The wrong
flags delivered press/release events but Views correctly rejected the click.

## Root causes and failed approaches

- Cold browser UI GPU raster stalled capture behind shader compilation.
  Trace4 measured a copy waiting 106 ms behind Graphite raster flush/Dawn
  compiler tasks, while readback itself took about 4 ms. Switching to Ganesh
  alone was intermittent. Eager startup refresh retries did not solve it.
  Disabling only `UiGpuRasterization` in the launcher resolved that Views stall;
  page GPU raster, Intel compositing, WebGL and shared textures remain enabled.
- Views client insets, popup monitor lookup, 64-bit distance calculations and
  DIP-to-pixel rounding all affect stream containment. A 48,192-pixel monitor
  offset exposed an integer-overflow bug. Check the entire selectable row,
  including fractional-scale edges, rather than only its centre.
- During resize, Viz can fit an old root surface into the new capture size,
  creating black margins. Accept root frames only when source size and full
  content bounds match the requested dimensions.
- A WebUI probe calling `GetWebContents()` created/attached empty contents and
  changed the popup being tested. Read-only `web_contents()` avoids that.
  Native bounds rounded to 767 DIP while DOM geometry was 767.200012 DIP;
  readiness permits less than one DIP of rounding while the full DOM rect is
  retained for the exact containment assertion.
- Retyping the same URL could leave suggestions closed after history inline
  completion. Unique per-scale query fragments make the second query real.
- A script-ready OOPIF fixture was not necessarily painted. Static loopback
  fixtures plus a real base-pixel witness avoid false restoration failures.

## Measurements and latency policy

Historical build40/Rust37 omnibox matrix passed all four transport/credit
variants at all three scales; the slowest measured CPU/software decoded
appearance was 244.32 ms (`prts-flyouts-omnibox-all2-*`). Earlier CPU runs were
intermittently 253–294 ms; a 294 ms run split into 203 ms capture and 91 ms
software encode/decode. Ganesh comparisons still measured about 260 ms.

Packed-BGRA SIMD conversion, up to four OpenH264 workers, a bounded visibility
witness and eliminating a redundant decoded-image copy reduced measurement
overhead. These changes retain odd-size edge replication and MF padding;
conversion tests compare BT.601/chroma/tails against the scalar reference.
Posting the first input-barrier poll avoids a timer turn without removing
renderer ACK, pending-routing or visual-state checks.

CPU readback/software timing is report-only under the revised plan. Correctness
still requires decoded pixels, input, exact restoration, bounded resources and
eventual completion. GPU texture capture plus hardware encoding retains the
250 ms decoded-appearance budget and 100 ms missing-layer diagnostic check.
Gate logs print the path's latency policy and measured milliseconds. Do not
spend another milestone chasing software timing near the production budget.

Historical evidence: `C:/src/chromium/prts-flyouts-qualified1-*`,
`prts-flyouts-context-all1-*`, `prts-flyouts-select-all1-*`,
`prts-flyouts-omnibox-all2-*`; the detailed sequence is in journal-mvp.md.
Final wrap-up qualification is recorded separately there and in STATUS.md.

Desktop gates observe hidden/cloaked HWNDs, no taskbar entry and native cursor
write counters. Physical foreground/cursor behaviour still needs a connected
desktop; this session is disconnected. Windows 10 22H2 remains unqualified.
