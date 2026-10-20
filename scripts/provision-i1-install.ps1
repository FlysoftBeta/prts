param([string]$Install = 'C:\src\chromium\src\out\Default', [string]$Log = 'C:\src\chromium\prts-i1-install-acl.log')
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
$installPath = (Resolve-Path -LiteralPath $Install).Path
if ($installPath -ne 'C:\src\chromium\src\out\Default') { throw 'Only the explicitly qualified Chromium install is permitted' }
$sid = (& .\prtsd\target\release\browser_i1.exe --install-capability | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $sid -notmatch '^S-1-15-3-1024(-[0-9]+){8}$') { throw 'Invalid PRTS install capability SID' }
# One-time grant. /T includes existing component DLLs copied with source ACLs.
# No state paths or system/driver ACLs are modified; no write rights are granted.
& icacls.exe $installPath /grant "*${sid}:(OI)(CI)(RX)" /T /C /Q *> $Log
$result = $LASTEXITCODE
if ($result -ne 0) { throw "Install RX grant failed; inspect $Log" }
"PRTS install capability: $sid; set PRTS_INSTALL_CAPABILITY=1 for experimental launches"
