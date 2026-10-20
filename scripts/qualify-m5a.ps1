param([string]$LogPrefix = 'C:\src\chromium\prts-m5a2-final')

# Run sequentially: browser acceptance owns its Chromium processes and shared GPU.
$ErrorActionPreference = 'Continue'
$env:PATH = 'C:\Users\Flysoft\.cargo\bin;C:\Program Files\nodejs;' + $env:PATH
$env:PRTS_M1_ALLOW_DISCONNECTED_DESKTOP = '1'
Set-Location (Split-Path $PSScriptRoot -Parent)
$journal = Join-Path (Get-Location) 'docs\prts\journal-m5.md'
$failed = @()
function Gate($name, [scriptblock]$action) {
    $log = "$LogPrefix-$name.log"
    Add-Content $journal "- $(Get-Date -Format 'yyyy-MM-dd HH:mm') [codex m5a2] Starting final gate $name. | log $log | inspect result and continue qualification."
    & $action *> $log
    $result = $LASTEXITCODE
    $result | Set-Content "$LogPrefix-$name.exit"
    Add-Content $journal "- $(Get-Date -Format 'yyyy-MM-dd HH:mm') [codex m5a2] Final gate $name exit=$result. | log $log | inspect any failure; complete docs and stage infra."
    if ($result -ne 0) { $script:failed += $name }
}
Remove-Item Env:\PRTS_BROWSER_PROTOCOL, Env:\PRTS_M5_CAPTURE, Env:\PRTS_M5_INPUT -ErrorAction SilentlyContinue
Gate 'release' { cargo build --manifest-path prtsd\Cargo.toml --release --bins }
if ($failed.Count) { exit 1 }
Gate 'cargo-test' { cargo test --manifest-path prtsd\Cargo.toml }
Gate 'clippy' { cargo clippy --manifest-path prtsd\Cargo.toml --all-targets -- -D warnings }
Gate 'fmt' { cargo fmt --manifest-path prtsd\Cargo.toml --check }
Gate 'node-unit' { & npm.cmd --prefix server test }
$env:PRTS_M5_CAPTURE = '1'
$env:PRTS_M5_INPUT = '1'
$env:PRTS_FRAME_MODE = 'gpu'
Gate 'm5-gpu' { & .\prtsd\target\release\browser_m5.exe }
$env:PRTS_FRAME_MODE = 'cpu'
Gate 'm5-cpu' { & .\prtsd\target\release\browser_m5.exe }
Remove-Item Env:\PRTS_M5_CAPTURE, Env:\PRTS_M5_INPUT -ErrorAction SilentlyContinue
$env:PRTS_FRAME_MODE = 'gpu'
Gate 'm1' { & .\prtsd\target\release\browser_m1.exe }
Gate 'm2' { & .\prtsd\target\release\browser_m2.exe }
$env:PRTS_FRAME_MODE = 'cpu'
Gate 'm3' { & .\prtsd\target\release\browser_m3.exe }
$env:PRTS_FRAME_MODE = 'gpu'
Gate 'm4' { & .\prtsd\target\release\browser_m4.exe }
Gate 'm4-recovery' { & .\prtsd\target\release\browser_m4_recovery.exe }
Gate 'mf-hardware' { cargo test --manifest-path prtsd\Cargo.toml --release --test mf_encoder -- --ignored --nocapture --test-threads=1 }
$env:PRTS_TEST_ENCODER = 'qsv'
Gate 'v1-e2e-gpu' { & node --test server/test/e2e-browser.js }
Gate 'v2-e2e-gpu' { & node --test server/test/e2e-window.js }
$env:PRTS_FRAME_MODE = 'cpu'
$env:PRTS_TEST_ENCODER = 'software'
Gate 'v1-e2e-cpu' { & node --test server/test/e2e-browser.js }
Gate 'v2-e2e-cpu' { & node --test server/test/e2e-window.js }
if ($failed.Count) {
    "FAILED: $($failed -join ', ')" | Set-Content "$LogPrefix.done"
    exit 1
}
'PASS' | Set-Content "$LogPrefix.done"
