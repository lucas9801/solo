# Prepares data close to a user's for testing Solo by hand or from a script
# (see upgrade-scenario.ps1): git repositories to open as project tabs, fake
# `claude` and `codex` (fake-agent.ps1: no account, no model) first on PATH,
# and a data folder whose settings have no AI model configured (Solo's
# default). With -Exe it also starts that Solo with the data folder, opens
# every repository as a tab and starts an agent in each tab's terminal:
# claude, codex, claude, ... It never touches your own Solo data: everything
# is under -Root.
#
#   script\prepare-user-like-data.ps1 -Root E:\solo-test\user-like
#   script\prepare-user-like-data.ps1 -Root E:\solo-test\user-like -Exe E:\solo-build\solo.exe
#
# Then `$env:PATH` must have <Root>\fakebin first when you start Solo
# yourself (this script sets it for the Solo it starts). Splitting a
# terminal pane is left to you: Solo takes no key combinations from outside
# its keyboard. ASCII only.

param(
    [Parameter(Mandatory = $true)][string]$Root,
    [string[]]$Projects = @('alpha', 'beta', 'gamma', 'delta'),
    [string]$Exe = '',
    [ValidateSet('zh', 'en')][string]$Language = 'zh',
    [switch]$Fresh
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\solo-scenario-lib.ps1"

if ($Fresh -and (Test-Path $Root)) {
    if (Get-Process -Name solo -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Root*" }) {
        throw "a Solo from $Root still runs"
    }
    Remove-Item -Recurse -Force $Root
}
New-Item -ItemType Directory -Force $Root | Out-Null
$Root = (Resolve-Path $Root).Path
$dataDir = Join-Path $Root 'data'
$reposDir = Join-Path $Root 'repos'
$fakebin = Join-Path $Root 'fakebin'
New-Item -ItemType Directory -Force $dataDir, $reposDir, $fakebin, (Join-Path $Root 'pids') | Out-Null

# The repositories, each with a commit.
foreach ($project in $Projects) {
    $repo = Join-Path $reposDir $project
    if (Test-Path (Join-Path $repo '.git')) { continue }
    New-Item -ItemType Directory -Force (Join-Path $repo 'src') | Out-Null
    Set-Content -Encoding ASCII (Join-Path $repo 'README.md') "# $project`n`nA test project.`n"
    Set-Content -Encoding ASCII (Join-Path $repo 'src\main.rs') "fn main() {`n    println!(`"$project`");`n}`n"
    & git -C $repo init -q -b main 2>&1 | Out-Null
    & git -C $repo -c user.name=test -c user.email=test@example.com add -A 2>&1 | Out-Null
    & git -C $repo -c user.name=test -c user.email=test@example.com commit -q -m "Start $project" 2>&1 | Out-Null
}

# The fake agents.
Copy-Item -Force (Join-Path $PSScriptRoot 'fake-agent.ps1') $fakebin
foreach ($agent in 'claude', 'codex') {
    Set-Content -Encoding ASCII (Join-Path $fakebin "$agent.cmd") "@powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"%~dp0fake-agent.ps1`" $agent %*`r`n"
}

# Settings: Solo's defaults (no AI model), nothing else.
$config = Join-Path $dataDir 'config'
New-Item -ItemType Directory -Force $config | Out-Null
$settings = Join-Path $config 'settings.json'
if (-not (Test-Path $settings)) { Set-Content -Encoding ASCII $settings "{`n}`n" }

Write-Step "prepared $Root (projects: $($Projects -join ', '); data: $dataDir)"

if ($Exe) {
    Set-ScenarioEnvironment $Root $Language
    $first = Join-Path $reposDir $Projects[0]
    $solo = Start-Solo $Exe $dataDir $first
    Start-Sleep -Seconds 8
    for ($ix = 0; $ix -lt $Projects.Count; $ix++) {
        $repo = Join-Path $reposDir $Projects[$ix]
        if ($ix -gt 0) {
            Open-InSolo $Exe $dataDir $repo
            Start-Sleep -Seconds 8
        }
        $agent = if ($ix % 2 -eq 0) { 'claude' } else { 'codex' }
        Send-SoloText $solo $agent
        Send-SoloEnter $solo
        Write-Step "started $agent in $($Projects[$ix])"
        Start-Sleep -Seconds 3
    }
    # Back to the first tab.
    Open-InSolo $Exe $dataDir $first
    Write-Step "Solo $($solo.Id) runs with $($Projects.Count) project tabs"
    $solo
}
