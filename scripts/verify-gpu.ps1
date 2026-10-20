# Invoked by test.ps1 with a deadline and per-gate logs. Exercise the production
# launcher in its default auto frame mode, then check Chromium's active GPU info.
$ErrorActionPreference = 'Stop'
if (!$env:PRTS_BROWSER_LOG) { throw 'PRTS_BROWSER_LOG must name the gate log' }
Remove-Item -LiteralPath $env:PRTS_BROWSER_LOG -ErrorAction SilentlyContinue
$env:PRTS_FRAME_MODE = 'auto'
$env:PRTS_TEST_ENCODER = 'qsv'
& node --test server/test/e2e-window.js
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$contents = Get-Content -LiteralPath $env:PRTS_BROWSER_LOG -Raw
$captures = [regex]::Matches($contents, 'PRTS native capture GPU vendor=(\d+) device=(\d+) luid=(\d+) swiftshader=(\d+)')
if (!$captures.Count) { throw 'No active Chromium capture GPU evidence' }
foreach ($capture in $captures) {
    if ($capture.Groups[1].Value -ne '32902' -or
        $capture.Groups[3].Value -eq '0' -or
        $capture.Groups[4].Value -ne '0') {
        throw "Chromium capture did not use verified Intel hardware: $($capture.Value)"
    }
    Write-Output $capture.Value
}
Write-Output 'PASS production auto capture uses Intel hardware; decoded WebGL/video and QSV zero-copy verified'
exit 0
