param(
    [Parameter(Mandatory=$true)][string]$Assets,
    [Parameter(Mandatory=$true)][string]$VerificationDir,
    [string]$Version = 'v0.1.0-preview',
    [string]$Repository = 'FlysoftBeta/prts',
    [string]$Notes = (Join-Path $PSScriptRoot '../docs/prts/release-v0.1.0-preview.md'),
    [int]$UploadTimeoutSeconds = 3600
)
$ErrorActionPreference = 'Stop'
$Assets = (Resolve-Path -LiteralPath $Assets).Path
$VerificationDir = (Resolve-Path -LiteralPath $VerificationDir).Path
if ((Get-FileHash (Join-Path $Assets 'SHA256SUMS.txt')).Hash -ne
    (Get-FileHash (Join-Path $VerificationDir 'verify-SHA256SUMS.txt')).Hash) {
    throw 'Archives differ from the clean-extract qualification receipt.'
}
if ((Get-Content (Join-Path $VerificationDir 'verify-full.exit') -Raw).Trim() -ne '0' -or
    (Get-Content (Join-Path $VerificationDir 'verify-gate.done') -Raw).Trim() -ne 'PASS') {
    throw 'Require completed clean-extract acceptance before publishing.'
}
$gh = (Get-Command gh.exe -ErrorAction SilentlyContinue).Source
if (!$gh) { $gh = 'C:\Program Files\GitHub CLI\gh.exe' }
if (!(Test-Path -LiteralPath $gh)) { throw 'GitHub CLI not found.' }
function GhJson([string[]]$Arguments) {
    $result = & $gh @Arguments
    if ($LASTEXITCODE) { throw "GitHub operation failed: $($Arguments -join ' ')" }
    return ($result | ConvertFrom-Json)
}
function Upload([string]$Path) {
    # Encode arguments as data so spaces and shell metacharacters remain literal.
    $data = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(
        (ConvertTo-Json -Compress -InputObject @($Version,$Path,'--repo',$Repository))))
    $ghLiteral = "'" + $gh.Replace("'","''") + "'"
    $code = "`$uploadArgs = ConvertFrom-Json ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$data'))); & $ghLiteral release upload @uploadArgs; exit `$LASTEXITCODE"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
    $p = Start-Process powershell.exe -ArgumentList @('-NoProfile','-EncodedCommand',$encoded) -WindowStyle Hidden -PassThru
    $null = $p.Handle
    $deadline = (Get-Date).AddSeconds($UploadTimeoutSeconds)
    while (!$p.WaitForExit(30000)) {
        Write-Output "Uploading $(Split-Path $Path -Leaf); process $($p.Id)"
        if ((Get-Date) -ge $deadline) {
            & taskkill.exe /PID $p.Id /T /F | Out-Null
            throw 'Upload timed out; rerun to resume completed assets.'
        }
    }
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { throw 'Upload failed; rerun to resume completed assets.' }
}
$expected = @{}
foreach ($line in Get-Content (Join-Path $Assets 'SHA256SUMS.txt')) {
    if ($line -notmatch '^([0-9a-fA-F]{64})  ([^\\/]+\.zip)$') { throw "Invalid checksum entry: $line" }
    $name = $Matches[2]
    $hash = $Matches[1].ToLowerInvariant()
    if ((Get-FileHash (Join-Path $Assets $name) -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) {
        throw "Local checksum mismatch: $name"
    }
    $expected[$name] = $hash
}
if ($expected.Count -ne 2) { throw 'Expected the browser and server archives.' }
$expected['SHA256SUMS.txt'] = (Get-FileHash (Join-Path $Assets 'SHA256SUMS.txt') -Algorithm SHA256).Hash.ToLowerInvariant()
# Bind qualification to these exact archives by comparing the extracted metadata.
foreach ($bundle in @('prts-chromium-win-x64','prts-server-win-x64')) {
    $qualified = Join-Path $VerificationDir "$bundle/BUILD.txt"
    $packaged = Join-Path $Assets "$bundle/BUILD.txt"
    if ((Get-FileHash $qualified).Hash -ne (Get-FileHash $packaged).Hash) { throw "Qualified BUILD metadata mismatch: $bundle" }
}
$release = & $gh release view $Version --repo $Repository --json databaseId,isDraft,url,assets 2>$null
if ($LASTEXITCODE) {
    & $gh release create $Version --repo $Repository --target main --draft --prerelease --title "PRTS $Version" --notes-file $Notes
    if ($LASTEXITCODE) { throw 'Could not create draft release.' }
    $release = & $gh release view $Version --repo $Repository --json databaseId,isDraft,url,assets
    if ($LASTEXITCODE) { throw 'Could not inspect draft release.' }
}
$release = $release | ConvertFrom-Json
foreach ($name in $expected.Keys) {
    $asset = @($release.assets | Where-Object { $_.name -eq $name })
    if ($asset.Count) {
        if ($asset.Count -ne 1 -or $asset[0].state -ne 'uploaded' -or $asset[0].digest -ne "sha256:$($expected[$name])") {
            throw "Remote asset differs or is incomplete: $name; inspect it before retrying."
        }
        Write-Output "Verified existing asset: $name"
    } else {
        if (!$release.isDraft) { throw "Published release is missing $name; inspect it before changing assets." }
        Upload (Join-Path $Assets $name)
    }
}
$remote = GhJson -Arguments @('api',"repos/$Repository/releases/$($release.databaseId)")
foreach ($name in $expected.Keys) {
    $asset = @($remote.assets | Where-Object { $_.name -eq $name })
    if ($asset.Count -ne 1 -or $asset[0].state -ne 'uploaded' -or $asset[0].digest -ne "sha256:$($expected[$name])") {
        throw "Remote checksum verification failed: $name"
    }
}
if ($remote.draft) {
    & $gh release edit $Version --repo $Repository --draft=false --prerelease --notes-file $Notes
    if ($LASTEXITCODE) { throw 'Publication failed; verified draft assets remain resumable.' }
}
$final = GhJson -Arguments @('api',"repos/$Repository/releases/$($release.databaseId)")
if ($final.draft -or !$final.prerelease) { throw 'Expected a published pre-release.' }
Write-Output "Published and SHA256 verified: $($final.html_url)"
