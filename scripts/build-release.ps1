# Full non-component release build of PRTS Chromium into out\Release (first run ~6-8 h; later runs incremental).
# Start detached: Start-Process powershell -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\Users\Flysoft\Desktop\chromium\scripts\build-release.ps1'
param([int]$TimeoutSeconds = 28800)
$ErrorActionPreference = 'Stop'
$env:DEPOT_TOOLS_WIN_TOOLCHAIN = '0'
$env:PATH = 'C:\src\depot_tools;' + $env:PATH
$src = 'C:\src\chromium\src'
$out = Join-Path $src 'out\Release'
$log = 'C:\src\chromium\prts-release-build.log'
$running = Get-CimInstance Win32_Process | Where-Object {
    $_.Name -in @('siso.exe','ninja.exe') -and $_.CommandLine -match 'out[\\/]Release'
}
if ($running) { throw "Release build already running (PID $($running.ProcessId -join ',')); poll it instead." }
New-Item -ItemType Directory -Force $out | Out-Null
if (-not (Test-Path (Join-Path $out 'args.gn'))) {
@'
is_debug = false
is_component_build = false
is_official_build = false
chrome_pgo_phase = 0
dcheck_always_on = false
symbol_level = 0
blink_symbol_level = 0
v8_symbol_level = 0
'@ | Out-File -Encoding ascii (Join-Path $out 'args.gn')
}
$journal = Join-Path (Split-Path $PSScriptRoot -Parent) 'docs/prts/journal-mvp.md'
function BuildStep([string]$Command, [int]$Seconds, [string]$Output) {
    Add-Content -Encoding UTF8 $journal "- $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') [codex] Release step timeout=${Seconds}s: $Command | logs $Output / $Output.err.log | inspect exit before packaging."
    $process = Start-Process cmd.exe -ArgumentList @('/d','/s','/c',$Command) -WorkingDirectory $src -WindowStyle Hidden -PassThru -RedirectStandardOutput $Output -RedirectStandardError "$Output.err.log"
    $null = $process.Handle
    if (!$process.WaitForExit($Seconds * 1000)) {
        & taskkill.exe /PID $process.Id /T /F | Out-Null
        $code = 124
    } else { $process.WaitForExit(); $code = $process.ExitCode }
    $code | Set-Content "$Output.exit"
    Add-Content -Encoding UTF8 $journal "- $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') [codex] Release step exit=$code | $Output | require success and clean-extract verification."
    if ($code -ne 0) { throw "Release command failed ($code): $Command" }
}
# Do not launch this while another ninja is using out/Release. Existing outputs
# are retained; after the initial build this is always an incremental build.
BuildStep 'gn gen out\Release' 600 "$log.gn.log"
BuildStep 'autoninja -C out\Release chrome' $TimeoutSeconds $log
