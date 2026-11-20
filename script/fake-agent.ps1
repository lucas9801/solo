# A fake AI agent for Solo's scenario tests: run as `claude` or `codex` (from
# claude.cmd / codex.cmd next to it, first on PATH), it acts like one without
# an account or a model: takes the arguments Solo's shell integration adds
# (claude's --settings with its hooks, codex's hook config), reports through
# Solo's hook program (solo-agent-hook) that its session started, that a
# prompt came, then that it stopped, and keeps printing and running until
# ended. It writes where it runs and its pid to <root>\pids\, for the scenario
# to check that the same process is still there after a restart.
#
#   fake-agent.ps1 <claude|codex> [the agent's arguments...]
#
# ASCII only.

# No param() block: PowerShell would take the agent's own options
# (--settings, --mcp-config) as parameters of this script.
$Agent = [string]$args[0]
$rest = @($args | Select-Object -Skip 1)
$ErrorActionPreference = 'Continue'
$root = $env:SOLO_SCENARIO_ROOT
if (-not $root) { $root = Split-Path -Parent $PSScriptRoot }
$project = Split-Path -Leaf (Get-Location).Path
$self = Get-Process -Id $PID
$session = [guid]::NewGuid().ToString()

$pids = Join-Path $root 'pids'
New-Item -ItemType Directory -Force $pids | Out-Null
$entry = [ordered]@{
    agent = $Agent
    project = $project
    directory = (Get-Location).Path
    pid = $PID
    started = $self.StartTime.ToUniversalTime().ToString('o')
    arguments = ($rest -join ' ')
}
($entry | ConvertTo-Json) | Set-Content -Encoding ASCII (Join-Path $pids "$Agent-$project-$PID.json")

# The hook command for a Claude Code event, from the --settings file Solo
# passes; for codex, the hook program named in its arguments, else the one
# next to solo-cli.
function Get-HookCommand([string]$eventName, [string]$kind) {
    if ($Agent -eq 'claude') {
        for ($i = 0; $i -lt $rest.Count - 1; $i++) {
            if ($rest[$i] -eq '--settings' -and (Test-Path -LiteralPath $rest[$i + 1])) {
                $settings = Get-Content -LiteralPath $rest[$i + 1] -Raw | ConvertFrom-Json
                $groups = $settings.hooks.$eventName
                if ($groups) { return $groups[0].hooks[0].command }
            }
        }
    }
    $joined = $rest -join ' '
    $match = [regex]::Match($joined, '[A-Za-z]:\\[^''"]*?solo-agent-hook\.exe')
    $hook = if ($match.Success) { $match.Value } elseif ($env:SOLO_CLI) { Join-Path (Split-Path $env:SOLO_CLI) 'solo-agent-hook.exe' } else { $null }
    if ($hook -and (Test-Path -LiteralPath $hook)) { return "`"$hook`" $kind --agent $Agent" }
    $null
}

function Send-Hook([string]$eventName, [string]$kind) {
    $command = Get-HookCommand $eventName $kind
    if (-not $command) {
        Write-Host "[fake $Agent] no hook program to report $kind"
        return
    }
    $payload = @{ hook_event_name = $eventName; session_id = $session; cwd = (Get-Location).Path } | ConvertTo-Json -Compress
    $payload | & cmd.exe /d /s /c $command
}

Write-Host "[fake $Agent] pid $PID in $project, session $session"
Send-Hook 'SessionStart' 'start'
Start-Sleep -Seconds 2
Write-Host "[fake $Agent] > fix the failing test"
Send-Hook 'UserPromptSubmit' 'prompt'
for ($i = 1; $i -le 5; $i++) {
    Write-Host "[fake $Agent] working... step $i of 5"
    Start-Sleep -Seconds 2
}
Send-Hook 'Stop' 'stop'
Write-Host "[fake $Agent] done; waiting for the next prompt"
$tick = 0
while ($true) {
    Start-Sleep -Seconds 10
    $tick++
    Write-Host "[fake $Agent] still here ($tick)"
}
