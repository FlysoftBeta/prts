param(
    [string]$Assets = $PSScriptRoot,
    [string]$OutputDirectory = $Assets
)
$ErrorActionPreference = 'Stop'
$Assets = (Resolve-Path -LiteralPath $Assets).Path
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$browser = @(Get-Content (Join-Path $Assets 'SHA256SUMS.txt') | Where-Object { $_ -match '^[0-9a-fA-F]{64}  prts-chromium-[^\\/]+\.zip$' })
if ($browser.Count -ne 1) { throw 'Expected one browser ZIP checksum.' }
$hash,$name = $browser[0] -split '  ',2
$parts = @()
foreach ($line in Get-Content (Join-Path $Assets 'SHA256PARTS.txt')) {
    if ($line -notmatch '^([0-9a-fA-F]{64})  ([^\\/]+\.zip\.[0-9]{3})$') { throw "Invalid part entry: $line" }
    $partHash,$partName = $Matches[1],$Matches[2]
    if ($partName -ne ('{0}.{1:d3}' -f $name,($parts.Count+1))) { throw 'Archive parts are not consecutive.' }
    $path = Join-Path $Assets $partName
    if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $partHash) { throw "Part checksum mismatch: $partName" }
    $parts += $path
}
if (!$parts.Count) { throw 'No archive parts.' }
$destination = Join-Path $OutputDirectory $name
if (Test-Path -LiteralPath $destination) {
    if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -ne $hash) { throw "Existing archive checksum mismatch: $destination" }
    Write-Output "Verified existing archive: $destination"
    return
}
$temporary = "$destination.joining"
$output = [IO.File]::Open($temporary,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write)
try {
    foreach ($path in $parts) {
        $inputFile = [IO.File]::OpenRead($path)
        try { $inputFile.CopyTo($output) } finally { $inputFile.Dispose() }
    }
} finally { $output.Dispose() }
if ((Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash -ne $hash) { throw "Reconstructed archive checksum mismatch: $temporary" }
Move-Item -LiteralPath $temporary -Destination $destination
Write-Output "Reconstructed and SHA256 verified: $destination"
