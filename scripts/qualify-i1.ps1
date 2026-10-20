param(
    [string]$LogPrefix = 'C:\src\chromium\prts-i1',
    [string[]]$Profiles = @('none', 'job'),
    [switch]$UiMatrix,
    [switch]$SkipBuild,
    [switch]$Experimental,
    [switch]$CaptureOnly
)
$ErrorActionPreference = 'Stop'
$env:PATH = 'C:\Users\Flysoft\.cargo\bin;C:\Program Files\nodejs;' + $env:PATH
$env:PRTS_M1_ALLOW_DISCONNECTED_DESKTOP = '1'
$env:PRTS_FRAME_MODE = 'gpu'
Set-Location (Split-Path $PSScriptRoot -Parent)
$journal = Join-Path (Get-Location) 'docs\prts\journal-i1.md'
$failed = @()
function Checkpoint($message) {
    Add-Content $journal "- $(Get-Date -Format 'yyyy-MM-dd HH:mm') [codex i1] $message | qualification in progress | inspect $LogPrefix logs and continue next gate."
}
function Gate($name, $program, [string[]]$arguments = @()) {
    $log = "$LogPrefix-$name.log"
    Checkpoint "Starting $name; log $log."
    # Process redirection retains native stderr lines without PowerShell formatting.
    $options = @{FilePath=$program; Wait=$true; PassThru=$true; WindowStyle='Hidden'; RedirectStandardOutput="$log.stdout"; RedirectStandardError=$log}
    if ($arguments.Count) { $options.ArgumentList=$arguments }
    $process = Start-Process @options
    $process.ExitCode | Set-Content "$LogPrefix-$name.exit"
    Checkpoint "Finished $name exit=$($process.ExitCode); stdout $log.stdout."
    if ($process.ExitCode -ne 0) { $script:failed += $name }
    return $process.ExitCode -eq 0
}
if (!$SkipBuild) {
    if (!(Gate 'release' 'cargo' @('build','--manifest-path','prtsd\Cargo.toml','--release','--bins'))) { exit 1 }
}
if ($Experimental) { $env:PRTS_I1_EXPERIMENTAL = '1'; $env:PRTS_INSTALL_CAPABILITY='1' }
foreach ($profile in $Profiles) {
    if ($profile -notin @('none','job','appcontainer','lpac')) { throw 'Invalid profile' }
    $env:PRTS_ISOLATION_PROFILE = $profile
    $env:PRTS_BROWSER_LOG = "$LogPrefix-$profile-chrome.log"
    $smoke = Gate "$profile-smoke" '.\prtsd\target\release\browser_i1.exe'
    if (!$smoke) { continue }
    $null = Gate "$profile-software-reject" '.\prtsd\target\release\browser_i1.exe' @('--software-check')
    $null = Gate "$profile-m4" '.\prtsd\target\release\browser_m4.exe'
    $env:PRTS_M5_CAPTURE = '1'
    if (!$CaptureOnly) { $env:PRTS_M5_INPUT = '1' }
    $null = Gate "$profile-m5" '.\prtsd\target\release\browser_m5.exe'
    Remove-Item Env:\PRTS_M5_CAPTURE,Env:\PRTS_M5_INPUT -ErrorAction SilentlyContinue
    $env:PRTS_TEST_ENCODER = 'qsv'
    if (!$CaptureOnly) { $null = Gate "$profile-e2e" 'C:\Program Files\nodejs\node.exe' @('--test','server/test/e2e-browser.js','server/test/e2e-window.js') }
    if ($profile -eq 'appcontainer') {
        $null = Gate "$profile-public-e2e" 'C:\Program Files\nodejs\node.exe' @('--test','server/test/isolation-public.js')
        $null = Gate "$profile-daemon-crash" 'C:\Program Files\nodejs\node.exe' @('server/test/isolation-live.js')
    }
}
if ($UiMatrix) {
    $env:PRTS_ISOLATION_PROFILE = 'job'
    foreach ($flag in @(@('EXITWINDOWS',128),@('DESKTOP',64),@('DISPLAYSETTINGS',16),@('SYSTEMPARAMETERS',8),@('READCLIPBOARD',2),@('WRITECLIPBOARD',4),@('GLOBALATOMS',32),@('HANDLES',1))) {
        $name=$flag[0]; $mask=$flag[1]
        $env:PRTS_JOB_LIMITS = '{"ui_restrictions":' + $mask + '}'
        $env:PRTS_BROWSER_LOG = "$LogPrefix-ui-$name-chrome.log"
        if (Gate "ui-$name-smoke" '.\prtsd\target\release\browser_i1.exe') {
            $null = Gate "ui-$name-m4" '.\prtsd\target\release\browser_m4.exe'
            $env:PRTS_M5_CAPTURE='1'; $env:PRTS_M5_INPUT='1'
            $null = Gate "ui-$name-m5" '.\prtsd\target\release\browser_m5.exe'
            Remove-Item Env:\PRTS_M5_CAPTURE,Env:\PRTS_M5_INPUT -ErrorAction SilentlyContinue
        }
    }
    Remove-Item Env:\PRTS_JOB_LIMITS -ErrorAction SilentlyContinue
}
if ($failed.Count) { "FAILED: $($failed -join ', ')" | Set-Content "$LogPrefix.done"; exit 1 }
if ($CaptureOnly) { 'PASS capture-only (not full qualification)' | Set-Content "$LogPrefix.done" }
else { 'PASS' | Set-Content "$LogPrefix.done" }
