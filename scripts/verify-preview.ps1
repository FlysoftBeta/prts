param(
    [Parameter(Mandatory=$true)][string]$Assets,
    [string]$ExtractDir = ('C:\src\chromium\prts-preview-check-' + (Get-Date -Format 'yyyyMMdd-HHmmss')),
    [switch]$AllowDisconnectedDesktop,
    [switch]$PreviewKnownIssues
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$Assets = (Resolve-Path -LiteralPath $Assets).Path
if (Test-Path -LiteralPath $ExtractDir) { throw "Choose a fresh extraction directory: $ExtractDir" }
$archives = @()
foreach ($line in Get-Content -LiteralPath (Join-Path $Assets 'SHA256SUMS.txt')) {
    if ($line -notmatch '^([0-9a-fA-F]{64})  ([^\\/]+\.zip)$') { throw "Invalid SHA256 manifest line: $line" }
    $expected = $Matches[1]
    $archive = Join-Path $Assets $Matches[2]
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expected) { throw "SHA256 mismatch: $archive" }
    $archives += $archive
}
if ($archives.Count -ne 2) { throw 'Expected Chromium and server archives' }
$null = New-Item -ItemType Directory -Path $ExtractDir
foreach ($archive in $archives) { Expand-Archive -LiteralPath $archive -DestinationPath $ExtractDir }
$ExtractDir = (Resolve-Path -LiteralPath $ExtractDir).Path
$env:PRTS_CHROME_PATH = Join-Path $ExtractDir 'prts-chromium-win-x64\chrome.exe'
$env:PRTSD_PATH = Join-Path $ExtractDir 'prts-server-win-x64\prtsd\prtsd.exe'
$env:PRTS_TEST_SERVER_DIR = Join-Path $ExtractDir 'prts-server-win-x64\server'
$env:PATH = 'C:\Program Files\nodejs;' + $env:PATH
function Bounded([string]$Command, [int]$Seconds, [string]$Log) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("try { & { $Command }; exit `$LASTEXITCODE } catch { [Console]::Error.WriteLine(`$_.ToString()); exit 1 }"))
    $p = Start-Process powershell.exe -ArgumentList @('-NoProfile','-EncodedCommand',$encoded) -WorkingDirectory $repo -WindowStyle Hidden -PassThru -RedirectStandardOutput "$Log.log" -RedirectStandardError "$Log.err.log"
    $null = $p.Handle
    if (!$p.WaitForExit($Seconds * 1000)) { & taskkill.exe /PID $p.Id /T /F | Out-Null; $code = 124 }
    else { $p.WaitForExit(); $code = $p.ExitCode }
    $code | Set-Content "$Log.exit"
    if ($code -ne 0) { throw "Verification failed ($code): $Log" }
}
$journal = Join-Path $repo 'docs/prts/journal-mvp.md'
Add-Content -Encoding UTF8 $journal "- $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') [codex] Preview SHA256 verified; clean extract $ExtractDir | packaged Node install and full acceptance with extracted Chrome/daemon/server overrides next | inspect $ExtractDir\verify-* logs before publication."
Bounded 'npm.cmd --prefix $env:PRTS_TEST_SERVER_DIR ci --omit=dev' 300 (Join-Path $ExtractDir 'verify-install')
$env:PRTS_PREVIEW_LOG_PREFIX = Join-Path $ExtractDir 'verify-gate'
$command = '& ./scripts/test.ps1 -Suite full -LogPrefix $env:PRTS_PREVIEW_LOG_PREFIX'
if ($AllowDisconnectedDesktop) { $command += ' -AllowDisconnectedDesktop' }
if ($PreviewKnownIssues) { $command += ' -PreviewKnownIssues' }
Bounded $command 3600 (Join-Path $ExtractDir 'verify-full')
Add-Content -Encoding UTF8 $journal "- $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') [codex] Clean-extract preview full acceptance PASS | assets $Assets extract $ExtractDir | commit recorded qualification and publish prerelease with SHA256 sums."
Write-Output "PASS clean-extract preview: $ExtractDir"
