param(
    [string]$Version = 'v0.1.0-preview',
    [string]$ChromeDir = 'C:\src\chromium\src\out\Release',
    [string]$Destination = ('C:\src\chromium\prts-preview-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$ChromeDir = (Resolve-Path -LiteralPath $ChromeDir).Path
# This script only packages a completed, non-component build; never build here.
$gnArgs = Get-Content (Join-Path $ChromeDir 'args.gn') -Raw
foreach ($required in @('is_component_build = false','dcheck_always_on = false','symbol_level = 0')) {
    if (!$gnArgs.Contains($required)) { throw "Release args must contain: $required" }
}
if (Test-Path -LiteralPath $Destination) { throw "Choose a fresh packaging directory: $Destination" }
$null = New-Item -ItemType Directory -Path $Destination
$browser = Join-Path $Destination 'prts-chromium-win-x64'
$bundle = Join-Path $Destination 'prts-server-win-x64'
$null = New-Item -ItemType Directory -Path $browser,$bundle,(Join-Path $bundle 'prtsd'),(Join-Path $bundle 'server')
foreach ($name in @('chrome.exe','chrome.dll','chrome_elf.dll','chrome_100_percent.pak','chrome_200_percent.pak','resources.pak','icudtl.dat','v8_context_snapshot.bin','d3dcompiler_47.dll','libEGL.dll','libGLESv2.dll')) {
    Copy-Item -LiteralPath (Join-Path $ChromeDir $name) -Destination $browser
}
foreach ($pattern in @('chrome_wer.dll','chrome_renderer.dll','dxcompiler.dll','dxil.dll','vulkan-1.dll','vk_swiftshader.dll','vk_swiftshader_icd.json','*.manifest','vcruntime140*.dll','msvcp140*.dll')) {
    Get-ChildItem -Path (Join-Path $ChromeDir $pattern) -File -ErrorAction SilentlyContinue | Copy-Item -Destination $browser
}
Copy-Item -LiteralPath (Join-Path $ChromeDir 'locales') -Destination $browser -Recurse
foreach ($dir in @('MEIPreload','IwaKeyDistribution')) {
    if (Test-Path -LiteralPath (Join-Path $ChromeDir $dir)) { Copy-Item -LiteralPath (Join-Path $ChromeDir $dir) -Destination $browser -Recurse }
}
Copy-Item -LiteralPath (Join-Path $repo 'prtsd/target/release/prtsd.exe') -Destination (Join-Path $bundle 'prtsd')
Get-ChildItem -Path (Join-Path $ChromeDir 'vcruntime140*.dll'),(Join-Path $ChromeDir 'msvcp140*.dll') -File | Copy-Item -Destination (Join-Path $bundle 'prtsd')
# Explicit source allowlist: no node_modules, tests, screenshots or build tools.
foreach ($name in @('index.js','daemon.js','framing.js','package.json','package-lock.json','public')) {
    Copy-Item -LiteralPath (Join-Path $repo "server/$name") -Destination (Join-Path $bundle 'server') -Recurse
}
@'
param([string]$ChromePath = (Join-Path $PSScriptRoot '..\prts-chromium-win-x64\chrome.exe'))
$ErrorActionPreference='Stop'
$env:PRTS_CHROME_PATH=(Resolve-Path -LiteralPath $ChromePath).Path
$env:PRTSD_PATH=Join-Path $PSScriptRoot 'prtsd\prtsd.exe'
$env:PRTS_BROWSER_PROTOCOL='2'
& node (Join-Path $PSScriptRoot 'server')
exit $LASTEXITCODE
'@ | Set-Content -Encoding UTF8 (Join-Path $bundle 'start.ps1')
@'
PRTS preview (Windows x64)

Extract both archives into the same directory. Install Node.js 22 or newer.
In PowerShell:
  cd prts-server-win-x64
  npm.cmd --prefix server ci --omit=dev
  powershell -ExecutionPolicy Bypass -File start.ps1
Open http://127.0.0.1:8080 and create a Chromium instance. Use auto for hardware
encoding with software fallback. The stream includes the browser toolbar and
app/context/select/omnibox popups. A second viewer can create another instance.

Known gaps: touch gestures, additional flyout families, independent streams for
extra windows within one instance, AppContainer/LPAC, Windows 10 qualification,
connected-desktop foreground/cursor qualification. Intel MF is the current
hardware encoder; direct oneVPL and NVENC follow later. Software latency is
report-only. Browser application branding is PRTS; web UA/CH uses Chrome identity.
'@ | Set-Content -Encoding UTF8 (Join-Path $bundle 'QUICKSTART.txt')
Copy-Item -LiteralPath (Join-Path $bundle 'QUICKSTART.txt') -Destination $browser
Copy-Item -LiteralPath 'C:\src\chromium\src\LICENSE' -Destination (Join-Path $browser 'LICENSE.chromium')
$revision = & git -C $repo rev-parse HEAD
$pin = (Get-Content (Join-Path $repo 'PIN') -Raw).Trim()
"version=$Version`ninfra=$revision`nchromium_pin=$pin" | Set-Content -Encoding UTF8 (Join-Path $bundle 'BUILD.txt')
Copy-Item -LiteralPath (Join-Path $bundle 'BUILD.txt') -Destination $browser
Add-Type -AssemblyName System.IO.Compression.FileSystem
foreach ($dir in @($browser,$bundle)) {
    $zip = "$dir-$Version.zip"
    [IO.Compression.ZipFile]::CreateFromDirectory($dir,$zip,[IO.Compression.CompressionLevel]::Optimal,$true)
    if ((Get-Item -LiteralPath $zip).Length -ge 2GB) { throw "Asset exceeds GitHub 2GB limit: $zip" }
}
Get-ChildItem -LiteralPath $Destination -Filter '*.zip' | ForEach-Object {
    $hash=Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256
    "$($hash.Hash.ToLowerInvariant())  $($_.Name)"
} | Set-Content -Encoding ascii (Join-Path $Destination 'SHA256SUMS.txt')
Write-Output "Packaged $Destination; clean-extract verification is required before gh release create."
