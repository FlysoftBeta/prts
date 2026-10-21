# Full non-component release build of PRTS Chromium into out\Release (first run ~6-8 h; later runs incremental).
# Start detached: Start-Process powershell -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\Users\Flysoft\Desktop\chromium\scripts\build-release.ps1'
$ErrorActionPreference = 'Continue'
$env:DEPOT_TOOLS_WIN_TOOLCHAIN = '0'
$env:PATH = 'C:\src\depot_tools;' + $env:PATH
$src = 'C:\src\chromium\src'
$out = Join-Path $src 'out\Release'
$log = 'C:\src\chromium\prts-release-build.log'
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
Push-Location $src
"$(Get-Date -Format s) gn gen" | Out-File -Encoding utf8 $log
cmd /c "gn gen out\Release >> `"$log`" 2>&1"
"$(Get-Date -Format s) autoninja chrome" | Out-File -Append -Encoding utf8 $log
cmd /c "autoninja -C out\Release chrome >> `"$log`" 2>&1"
"$(Get-Date -Format s) exit=$LASTEXITCODE" | Out-File -Append -Encoding utf8 $log
Pop-Location
