<#
Chromium workflow: pinned, history-less checkout + own commits kept as patches.

  Infra repo: this repo (PIN, patches\, scripts\)    Checkout: $env:CR_ROOT (default C:\src\chromium)
  In src: branch "main" = upstream at PIN, branch "work" = main + patches\*.patch

Commands:
  init               fresh checkout of src@PIN without history (creates .gclient), then apply patches
  pin [rev|latest]   write PIN (default: latest upstream main); does not touch the checkout
  sync               export work, sync src+deps to PIN (shallow), re-apply patches
  update             pin latest + sync
  export [branch]    work (or selected WIP branch) commits -> patches\
  apply              recreate "work" from main + patches\
  status             PIN vs checkout, patch count
  gen   [out]        gn gen with recommended args (default out\Default)
  build [out] [tgt]  autoninja (default target: chrome)
  run   [out]        launch chrome.exe
Commit on "work" with plain git, then "export" and commit patches\ in this repo.
#>
param([Parameter(Position=0)][string]$Cmd = 'help', [Parameter(Position=1)][string]$A1, [Parameter(Position=2)][string]$A2)
$ErrorActionPreference = 'Stop'
$env:Path = "C:\src\depot_tools;" + [Environment]::GetEnvironmentVariable('Path','Machine') + ";" + [Environment]::GetEnvironmentVariable('Path','User')
$env:DEPOT_TOOLS_WIN_TOOLCHAIN = '0'
$Repo    = Split-Path $PSScriptRoot
$Root    = if ($env:CR_ROOT) { $env:CR_ROOT } else { 'C:\src\chromium' }
$Src     = "$Root\src"
$Patches = "$Repo\patches"
$PinFile = "$Repo\PIN"
$Url     = 'https://chromium.googlesource.com/chromium/src.git'

function G { & git -C $Src @args; if ($LASTEXITCODE) { throw "git $args failed ($LASTEXITCODE)" } }
function Has-Branch($b) { & git -C $Src show-ref --verify --quiet "refs/heads/$b"; -not $LASTEXITCODE }
function Get-Pin { if (-not (Test-Path $PinFile)) { throw "No PIN; run 'cr pin'" }; (Get-Content $PinFile -TotalCount 1).Trim() }
function Native($line) { cmd /c "$line 2>&1"; if ($LASTEXITCODE) { throw "'$line' failed ($LASTEXITCODE)" } }

function Pin($rev) {
  if (-not $rev -or $rev -eq 'latest') {
    $rev = ((& git ls-remote $Url refs/heads/main) -split '\s+')[0]
  } elseif ($rev -notmatch '^[0-9a-f]{40}$') {
    $rev = ((& git ls-remote $Url $rev) -split '\s+')[0]   # branch/tag name, e.g. refs/branch-heads/7000
  }
  if ($rev -notmatch '^[0-9a-f]{40}$') { throw "Cannot resolve revision" }
  $rev | Set-Content -Encoding ascii $PinFile
  Write-Host "PIN = $rev"
}

function Export([string]$branch = 'work') {
  if (-not (Has-Branch $branch) -or -not (Has-Branch main)) { throw "Missing main/$branch export branch" }
  New-Item -ItemType Directory -Force $Patches | Out-Null
  Get-ChildItem $Patches -Filter *.patch | Remove-Item
  G format-patch --zero-commit --no-signature --no-stat -o $Patches "main..$branch" | Out-Null
  Write-Host "Exported $((Get-ChildItem $Patches -Filter *.patch).Count) patch(es)"
}

function Apply {
  G checkout -q -f -B work main
  $list = @(Get-ChildItem $Patches -Filter *.patch -ErrorAction SilentlyContinue | Sort-Object Name)
  if ($list.Count) {
    & git -C $Src am -q --3way --keep-cr $list.FullName
    if ($LASTEXITCODE) { throw "git am failed: fix in $Src, 'git am --continue', then 'cr export'" }
  }
  Write-Host "work = main + $($list.Count) patch(es)"
}

function Sync-Checkout {
  $pin = Get-Pin
  if (Test-Path "$Src\.git") {
    if (Has-Branch work) {
      if (G status --porcelain --untracked-files=no) { throw 'Uncommitted changes in src; commit or stash first.' }
      Export
    }
    G checkout -q -f --detach
  }
  Push-Location $Root
  try { Native "gclient sync --revision src@$pin --no-history --shallow -D --force --reset" } finally { Pop-Location }
  G branch -f main $pin
  Apply
}

function Init {
  New-Item -ItemType Directory -Force $Root | Out-Null
  if (-not (Test-Path "$Root\.gclient")) {
    @"
solutions = [
  {
    "name": "src",
    "url": "$Url",
    "managed": False,
    "custom_deps": {},
    "custom_vars": {},
  },
]
"@ | Set-Content -Encoding ascii "$Root\.gclient"
  }
  Sync-Checkout
}

function Gen($out) {
  $dir = Join-Path $Src $out
  New-Item -ItemType Directory -Force $dir | Out-Null
  if (-not (Test-Path "$dir\args.gn")) { Copy-Item "$Repo\args.gn" "$dir\args.gn" }
  Push-Location $Src; try { Native "gn gen $out" } finally { Pop-Location }
}

$Out = if ($A1) { $A1 } else { 'out\Default' }
switch ($Cmd) {
  'init'   { Init }
  'pin'    { Pin $A1 }
  'sync'   { Sync-Checkout }
  'update' { Pin 'latest'; Sync-Checkout }
  'export' { if ($A1) { Export $A1 } else { Export } }
  'apply'  { Apply }
  'status' {
    $head = if (Has-Branch main) { G rev-parse main } else { G rev-parse HEAD }
    Write-Host "PIN   $(Get-Pin)`nmain  $head  $(if ($head -eq (Get-Pin)) {'(in sync)'} else {'(OUT OF SYNC: run cr sync)'})"
    Write-Host "patches: $(@(Get-ChildItem $Patches -Filter *.patch -ErrorAction SilentlyContinue).Count)"
  }
  'gen'    { Gen $Out }
  'build'  {
    if (-not (Test-Path (Join-Path $Src "$Out\build.ninja"))) { Gen $Out }
    $tgt = if ($A2) { $A2 } else { 'chrome' }
    Push-Location $Src; try { Native "autoninja -C $Out $tgt" } finally { Pop-Location }
  }
  'run'    { & (Join-Path $Src "$Out\chrome.exe") }
  default  { (Get-Content $PSCommandPath -TotalCount 20) -join "`n" }
}
