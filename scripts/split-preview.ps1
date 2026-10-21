param(
    [Parameter(Mandatory=$true)][string]$Assets,
    [ValidateRange(1,128)][int]$PartSizeMB = 32,
    [string]$VerificationDir
)
$ErrorActionPreference = 'Stop'
$Assets = (Resolve-Path -LiteralPath $Assets).Path
$browser = @(Get-ChildItem -LiteralPath $Assets -Filter 'prts-chromium-*.zip' -File)
if ($browser.Count -ne 1) { throw 'Expected exactly one qualified browser ZIP.' }
$source = $browser[0]
if (Get-ChildItem -LiteralPath $Assets -File | Where-Object { $_.Name -match ('^'+[regex]::Escape($source.Name)+'\.[0-9]{3}$') }) { throw 'Archive parts already exist.' }
$manifest = @()
$inputFile = [IO.File]::OpenRead($source.FullName)
try {
    $buffer = New-Object byte[] (1MB)
    for ($index=1; $inputFile.Position -lt $inputFile.Length; $index++) {
        $path = '{0}.{1:d3}' -f $source.FullName,$index
        $output = [IO.File]::Open($path,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write)
        try {
            $remaining = $PartSizeMB * 1MB
            while ($remaining -gt 0) {
                $read = $inputFile.Read($buffer,0,[Math]::Min($buffer.Length,$remaining))
                if (!$read) { break }
                $output.Write($buffer,0,$read)
                $remaining -= $read
            }
        } finally { $output.Dispose() }
        $manifest += ('{0}  {1}' -f (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant(),(Split-Path $path -Leaf))
    }
} finally { $inputFile.Dispose() }
$manifest | Set-Content -Encoding ascii (Join-Path $Assets 'SHA256PARTS.txt')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'join-preview.ps1') -Destination (Join-Path $Assets 'JOIN.ps1')
if ($VerificationDir) {
    $VerificationDir = (Resolve-Path -LiteralPath $VerificationDir).Path
    if ((Get-Content (Join-Path $VerificationDir 'verify-full.exit') -Raw).Trim() -ne '0' -or
        (Get-Content (Join-Path $VerificationDir 'verify-gate.done') -Raw).Trim() -ne 'PASS' -or
        (Get-FileHash (Join-Path $Assets 'SHA256SUMS.txt')).Hash -ne
        (Get-FileHash (Join-Path $VerificationDir 'verify-SHA256SUMS.txt')).Hash) { throw 'Require the original archive qualification receipt.' }
    $proof = Join-Path $Assets ('rejoin-proof-' + [Guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path $proof
    & (Join-Path $PSScriptRoot 'join-preview.ps1') -Assets $Assets -OutputDirectory $proof
    Copy-Item -LiteralPath (Join-Path $Assets 'SHA256PARTS.txt') -Destination (Join-Path $VerificationDir 'verify-SHA256PARTS.txt')
}
Write-Output "Created $($manifest.Count) parts. Reconstruct into a fresh directory and compare the qualified ZIP checksum before publishing."
