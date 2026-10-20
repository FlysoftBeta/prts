# PRTS autopilot: lets Codex continue the plan when the Claude orchestrator goes silent.
#
# Claude bumps $env:TEMP\prts\claude.heartbeat on every keepalive tick. If the heartbeat is older
# than -StaleMinutes (default 110 = 1h50m), Claude is assumed out of quota and this script drives
# Codex with docs/prts/CODEX_HANDOFF.md in a loop: relaunch on exit, sleep until the reset time on
# usage limits, stop when docs/prts/ALL_DONE exists. When the heartbeat becomes fresh again it stops
# launching new runs (a running Codex job is left to finish).
#
# Start detached:  Start-Process powershell -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\Users\Flysoft\Desktop\chromium\scripts\prts-autopilot.ps1'
param(
    [int]$StaleMinutes = 110,
    [int]$PollSeconds = 300,
    [string]$Repo = 'C:\Users\Flysoft\Desktop\chromium'
)

$ErrorActionPreference = 'Continue'
$dir = Join-Path $env:TEMP 'prts'
New-Item -ItemType Directory -Force $dir | Out-Null
$heartbeat = Join-Path $dir 'claude.heartbeat'
$logFile = Join-Path $dir 'autopilot.log'
$pidFile = Join-Path $dir 'autopilot.pid'
$doneFile = Join-Path $Repo 'docs\prts\ALL_DONE'

function Log($msg) { "$(Get-Date -Format s) $msg" | Out-File -Append -Encoding utf8 $logFile }

# Single instance.
if (Test-Path $pidFile) {
    $old = Get-Content $pidFile -ErrorAction SilentlyContinue
    if ($old -and (Get-Process -Id $old -ErrorAction SilentlyContinue)) { Log "already running as $old; exiting"; exit 0 }
}
$PID | Out-File -Encoding ascii $pidFile
if (-not (Test-Path $heartbeat)) { New-Item -ItemType File $heartbeat | Out-Null }
Log "autopilot started (stale=${StaleMinutes}m poll=${PollSeconds}s)"

$addendum = @'

---

## Autonomous mode (added by scripts/prts-autopilot.ps1)

The Claude orchestrator is unavailable (out of quota). You are now the sole orchestrator. Continue the
plan exactly where STATUS.md / the journals / any wip resume notes leave off. You MAY revise the plan or
priority order in PLAN.md when you have a concrete reason (blocker, better approach, finding) — record the
change and rationale in the STATUS.md log. Pace yourself: this run will be relaunched automatically after
you exit or hit a usage limit, so checkpoint to the journal often. When every item of the priority list is
complete and pushed, create the file `docs/prts/ALL_DONE` (one line summary) and stop.
'@

function Test-CodexRunning {
    # Any PRTS codex exec job (Claude-launched mvp.out or autopilot auto.out) counts.
    $procs = Get-CimInstance Win32_Process -Filter "Name='codex.exe' OR Name='node.exe'" -ErrorAction SilentlyContinue
    foreach ($p in $procs) {
        if ($p.CommandLine -and $p.CommandLine -match 'codex' -and $p.CommandLine -match 'exec' -and
            $p.CommandLine -match '(mvp|auto)\.out') { return $true }
    }
    return $false
}

function Get-ResetTime($logPath) {
    $text = Get-Content $logPath -Raw -ErrorAction SilentlyContinue
    if (-not $text -or $text -notmatch 'usage limit') { return $null }
    $m = [regex]::Matches($text, 'try again at ([^.\r\n]+?)\.?(\r|\n|$)')
    if ($m.Count -gt 0) {
        $raw = $m[$m.Count - 1].Groups[1].Value -replace '(\d)(st|nd|rd|th)', '$1'
        try {
            $t = [datetime]::Parse($raw)
            if ($t -lt (Get-Date)) { $t = $t.AddDays(1) }
            return $t.AddMinutes(3)
        } catch { }
    }
    return (Get-Date).AddMinutes(30)
}

$resumeAt = $null
$run = 0
$fastExits = 0
while ($true) {
    if (Test-Path $doneFile) { Log "ALL_DONE present; exiting"; break }

    $age = (Get-Date) - (Get-Item $heartbeat).LastWriteTime
    if ($age.TotalMinutes -lt $StaleMinutes) { Start-Sleep $PollSeconds; continue }
    if ($resumeAt -and (Get-Date) -lt $resumeAt) { Start-Sleep $PollSeconds; continue }
    if (Test-CodexRunning) { Start-Sleep $PollSeconds; continue }

    $run++
    $prompt = Join-Path $dir 'auto-prompt.md'
    (Get-Content (Join-Path $Repo 'docs\prts\CODEX_HANDOFF.md') -Raw) + $addendum | Out-File -Encoding utf8 $prompt
    $runLog = Join-Path $dir ("auto-{0:yyyyMMdd-HHmm}.log" -f (Get-Date))
    $out = Join-Path $dir 'auto.out'
    Log "heartbeat stale ($([int]$age.TotalMinutes)m); launching codex run $run -> $runLog"
    $start = Get-Date
    Push-Location $Repo
    cmd /c "codex exec -m gpt-6.1-sol -c model_reasoning_effort=medium --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox -C `"$Repo`" -o `"$out`" - < `"$prompt`" > `"$runLog`" 2>&1"
    $code = $LASTEXITCODE
    Pop-Location
    $mins = ((Get-Date) - $start).TotalMinutes
    Log "codex run $run exited $code after $([int]$mins)m"

    $resumeAt = Get-ResetTime $runLog
    if ($resumeAt) { Log "usage limit; resuming after $resumeAt"; $fastExits = 0; continue }
    if ($mins -lt 3) { $fastExits++ } else { $fastExits = 0 }
    if ($fastExits -ge 3) { $resumeAt = (Get-Date).AddMinutes(30); Log "3 fast exits; backing off 30m" }
}
Remove-Item $pidFile -ErrorAction SilentlyContinue
