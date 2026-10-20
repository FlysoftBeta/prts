param(
    [ValidateSet('quick', 'mf', 'gpu', 'flyouts', 'full')][string]$Suite = 'quick',
    [switch]$AllowDisconnectedDesktop,
    [string]$LogPrefix = (Join-Path $env:TEMP ('prts-tests-' + (Get-Date -Format 'yyyyMMdd-HHmmss')))
)

# Every gate runs in a separate process with a deadline. Keep GPU/lifetime gates
# sequential; concurrent MFT activation invalidates process-handle measurements.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\Program Files\nodejs;" + $env:PATH
$journal = Join-Path (Get-Location) 'docs/prts/journal-mvp.md'
$shell = (Get-Process -Id $PID).Path
Remove-Item Env:\PRTS_BROWSER_PROTOCOL, Env:\PRTS_M5_CAPTURE, Env:\PRTS_M5_INPUT,
    Env:\PRTS_M5_HOSTS, Env:\PRTS_M5_FLYOUTS, Env:\PRTS_M5_CONTEXT,
    Env:\PRTS_M5_ONE_CREDIT, Env:\PRTS_M5_TRACE,
    Env:\PRTS_FRAME_MODE, Env:\PRTS_TEST_ENCODER, Env:\PRTS_M1_ALLOW_DISCONNECTED_DESKTOP -ErrorAction SilentlyContinue
if ($AllowDisconnectedDesktop) { $env:PRTS_M1_ALLOW_DISCONNECTED_DESKTOP = '1' }
$failed = @()
function Gate([string]$Name, [string]$Command, [int]$Seconds = 180) {
    $log = "$LogPrefix-$Name"
    Add-Content -Encoding UTF8 $journal "- $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') [codex] Starting $Name (timeout ${Seconds}s): $Command | logs $log.log / $log.err.log | inspect exit and continue qualification."
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("try { & { $Command }; exit `$LASTEXITCODE } catch { [Console]::Error.WriteLine(`$_.ToString()); exit 1 }"))
    $process = Start-Process -FilePath $shell -ArgumentList @('-NoProfile', '-EncodedCommand', $encoded) -WindowStyle Hidden -PassThru -RedirectStandardOutput "$log.log" -RedirectStandardError "$log.err.log"
    # Cache the native process handle before it exits. Windows PowerShell's
    # Start-Process wrapper otherwise loses ExitCode for very short-lived gates.
    $null = $process.Handle
    if (!$process.WaitForExit($Seconds * 1000)) {
        # Kill only the gate's process tree, including its browser/daemon children.
        & taskkill.exe /PID $process.Id /T /F | Out-Null
        $result = 124
    } else {
        $process.WaitForExit()
        $result = $process.ExitCode
        if ($null -eq $result) { throw "No exit status for gate $Name" }
    }
    $result | Set-Content "$log.exit"
    Add-Content -Encoding UTF8 $journal "- $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') [codex] $Name exit=$result | logs $log.log / $log.err.log | inspect failures before merging."
    Write-Output "$Name exit=$result ($log.log)"
    if ($result -ne 0) { $script:failed += $Name }
}

function FlyoutGates {
    $env:PRTS_M5_HOSTS = '1'
    Gate 'flyout-hosts' '.\prtsd\target\release\browser_m5.exe'
    Remove-Item Env:\PRTS_M5_HOSTS
    Gate 'composition-hardware' 'cargo test --manifest-path prtsd/Cargo.toml --lib d3d_composition -- --ignored --nocapture --test-threads=1' 300
    $env:PRTS_M5_FLYOUTS = '1'
    foreach ($credits in @(2, 1)) {
        if ($credits -eq 1) { $env:PRTS_M5_ONE_CREDIT = '1' }
        foreach ($mode in @('cpu', 'gpu')) {
            $env:PRTS_FRAME_MODE = $mode
            Gate "flyouts-$mode-$credits" '.\prtsd\target\release\browser_m5.exe'
        }
    }
    Remove-Item Env:\PRTS_M5_FLYOUTS, Env:\PRTS_M5_ONE_CREDIT
    $env:PRTS_M5_CONTEXT = '1'
    foreach ($credits in @(2, 1)) {
        if ($credits -eq 1) { $env:PRTS_M5_ONE_CREDIT = '1' }
        foreach ($mode in @('cpu', 'gpu')) {
            $env:PRTS_FRAME_MODE = $mode
            Gate "context-$mode-$credits" '.\prtsd\target\release\browser_m5.exe'
        }
    }
    Remove-Item Env:\PRTS_M5_CONTEXT, Env:\PRTS_M5_ONE_CREDIT
}

if ($Suite -eq 'mf') {
    Gate 'mf-hardware' 'cargo test --manifest-path prtsd/Cargo.toml --release --test mf_encoder -- --ignored --nocapture --test-threads=1' 300
} elseif ($Suite -eq 'gpu') {
    $env:PRTS_BROWSER_LOG = "$LogPrefix-gpu-adapter-browser.log"
    Gate 'gpu-adapter' '& ./scripts/verify-gpu.ps1' 360
} else {
    Gate 'release' 'cargo build --manifest-path prtsd/Cargo.toml --release --bins' 900
    if ($failed.Count) { exit 1 }
    Gate 'cargo-test' 'cargo test --manifest-path prtsd/Cargo.toml' 600
    Gate 'clippy' 'cargo clippy --manifest-path prtsd/Cargo.toml --all-targets -- -D warnings' 600
    Gate 'fmt' 'cargo fmt --manifest-path prtsd/Cargo.toml --check'
    Gate 'node-unit' 'npm.cmd --prefix server test'
    if ($Suite -eq 'flyouts') { FlyoutGates }
    if ($Suite -eq 'full') {
        $env:PRTS_M5_CAPTURE = '1'
        $env:PRTS_M5_INPUT = '1'
        $env:PRTS_FRAME_MODE = 'gpu'
        Gate 'm5-gpu' '.\prtsd\target\release\browser_m5.exe'
        $env:PRTS_FRAME_MODE = 'cpu'
        Gate 'm5-cpu' '.\prtsd\target\release\browser_m5.exe'
        Remove-Item Env:\PRTS_M5_CAPTURE, Env:\PRTS_M5_INPUT
        FlyoutGates
        $env:PRTS_FRAME_MODE = 'gpu'
        Gate 'm1' '.\prtsd\target\release\browser_m1.exe' 360
        Gate 'm2' '.\prtsd\target\release\browser_m2.exe'
        $env:PRTS_FRAME_MODE = 'cpu'
        Gate 'm3' '.\prtsd\target\release\browser_m3.exe'
        $env:PRTS_FRAME_MODE = 'gpu'
        Gate 'm4' '.\prtsd\target\release\browser_m4.exe'
        Gate 'm4-recovery' '.\prtsd\target\release\browser_m4_recovery.exe'
        Gate 'mf-hardware' 'cargo test --manifest-path prtsd/Cargo.toml --release --test mf_encoder -- --ignored --nocapture --test-threads=1' 300
        Gate 'odd-sizes' 'cargo test --manifest-path prtsd/Cargo.toml --release --test odd_sizes -- --include-ignored --nocapture --test-threads=1' 300
        $env:PRTS_TEST_ENCODER = 'qsv'
        Gate 'v1-e2e-gpu' 'node --test server/test/e2e-browser.js'
        Gate 'v2-e2e-gpu' 'node --test server/test/e2e-window.js'
        $env:PRTS_BROWSER_LOG = "$LogPrefix-gpu-adapter-browser.log"
        Gate 'gpu-adapter' '& ./scripts/verify-gpu.ps1' 360
        Remove-Item Env:\PRTS_BROWSER_LOG
        $env:PRTS_FRAME_MODE = 'cpu'
        $env:PRTS_TEST_ENCODER = 'software'
        Gate 'v1-e2e-cpu' 'node --test server/test/e2e-browser.js'
        Gate 'v2-e2e-cpu' 'node --test server/test/e2e-window.js'
        Gate 'synthetic-e2e' 'node --test server/test/e2e.js'
    }
}
if ($failed.Count) {
    "FAILED: $($failed -join ', ')" | Set-Content "$LogPrefix.done"
    exit 1
}
'PASS' | Set-Content "$LogPrefix.done"
