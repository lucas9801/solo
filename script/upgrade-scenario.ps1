# The "upgrade and restart" scenario, run before every release: the path
# where 2.5.2 broke (terminals of tabs not opened yet moved into the current
# tab, reconnected agents not recognized, a terminal host failing to start
# after an update), which no single feature's test covers.
#
# What it does, with its own data folder (-Root; your own Solo data is never
# touched) and fake agents (fake-agent.ps1: no account, no model):
#  1. Starts the previous release (-OldDir: its program files, solo.exe and
#     the rest) with 3 project tabs (git repositories), an agent in each
#     tab's terminal (claude, codex, claude), and stops in the first tab.
#  2. "Updates": quits it, as "Update and Restart" does, and starts the new
#     build (-NewDir) with the same data folder. Releases up to 2.5.3 keep
#     terminals running an agent when they quit, which is what an update's
#     restart does; this script doesn't run the installer (it would register
#     Solo for the user: solo:// links, the Explorer menu, an uninstall
#     entry), so the "update" is quitting the old files and starting the new
#     ones. On a throwaway machine (a CI runner) the installer could do it
#     instead: Solo-Setup-<old>.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
#     /DIR=<folder>, then pass that folder as -OldDir.
#  3. Waits -WaitSeconds (90) without opening the other tabs, then opens
#     them one by one.
#  4. Checks, from `solo-cli agent ls --json`, the fake agents' pid files and
#     Solo.log: each agent's terminal is in its own project's tab, the agent
#     and its status are recognized and the title is the agent; the agent is
#     the same process; there's no extra empty shell; no terminal "ends with
#     Solo" (its host couldn't start); Solo.log has no ERROR or WARN outside
#     the allowed list.
#  5. Then a crash (taskkill /F of solo.exe): the agents keep running and
#     come back, checked the same way.
#  6. Then a normal quit (closing the window): since Solo asks before
#     quitting ends running agents, it confirms the prompt and checks that
#     the agents ended; a Solo that doesn't ask and keeps them (2.5.3 and
#     before) is reported as such.
#
#   script\upgrade-scenario.ps1 -OldDir E:\solo-2.5.3 -NewDir E:\solo-build -Root E:\solo-test\upgrade
#
# Exit code 0 when everything passed, 1 otherwise; screenshots and the
# report (report.md) are in <Root>\out. A stable Solo shares its
# single-instance pipe with any installed Solo: quit yours first (the script
# refuses to start while another solo.exe runs).
#
# Later fixes add their scenario here (agents kept for an update and
# reconnected; quitting asks; turn snapshots).
#
# ASCII only (Windows PowerShell reads scripts in the ANSI code page).

param(
    # The previous release's program files; without it, the new build is
    # set up too, and "updated" by restarting itself (Restart, the path
    # "Update and Restart" takes, without installing).
    [string]$OldDir = '',
    [Parameter(Mandatory = $true)][string]$NewDir,
    [Parameter(Mandatory = $true)][string]$Root,
    [int]$WaitSeconds = 90,
    # The new build gives a Solo with --user-data-dir its own single
    # instance, so it may run beside another Solo (an installed one).
    [switch]$IsolatedNewBuild,
    [ValidateSet('zh', 'en')][string]$Language = 'en',
    [switch]$SkipCrash,
    [switch]$SkipQuit
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\solo-scenario-lib.ps1"

$projects = @('alpha', 'beta', 'gamma')
$agents = @{ alpha = 'claude'; beta = 'codex'; gamma = 'claude' }
$sameBuild = -not $OldDir
if ($sameBuild) { $OldDir = $NewDir }
$oldExe = Join-Path $OldDir 'solo.exe'
$newExe = Join-Path $NewDir 'solo.exe'
$newCli = Join-Path $NewDir 'solo-cli.exe'
foreach ($file in $oldExe, $newExe, $newCli) {
    if (-not (Test-Path $file)) { throw "$file is missing" }
}

# Log lines that are allowed: known, harmless messages.
$allowed = @(
    # Solo looks for local model servers (LM Studio, llama.cpp, Ollama) at
    # startup; with none running, as by default, the connection is refused.
    'provider/lmstudio.rs',
    'provider/llama_cpp.rs',
    'provider/ollama.rs',
    # A GPU vendor's optional helper library Solo asks for its driver
    # version; machines without it log this at every start.
    'amd_ags_x64.dll'
)

$results = New-Object System.Collections.ArrayList
function Add-Result([string]$phase, [string]$check, [bool]$ok, [string]$detail = '') {
    [void]$results.Add([pscustomobject]@{ Phase = $phase; Check = $check; Ok = $ok; Detail = $detail })
    $mark = if ($ok) { 'ok  ' } else { 'FAIL' }
    Write-Host "  $mark $phase / $check $(if ($detail) { "- $detail" })"
}

& "$PSScriptRoot\prepare-user-like-data.ps1" -Root $Root -Projects $projects -Language $Language -Fresh | Out-Null
$Root = (Resolve-Path $Root).Path
$dataDir = Join-Path $Root 'data'
$reposDir = Join-Path $Root 'repos'
$out = Join-Path $Root 'out'
New-Item -ItemType Directory -Force $out | Out-Null
Set-ScenarioEnvironment $Root $Language

function Wait-Until([scriptblock]$condition, [int]$seconds) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        if (& $condition) { return $true }
        Start-Sleep -Seconds 1
    }
    $false
}

# Opens each tab not opened yet, then checks every terminal.
function Test-Restored([string]$phase, $solo, $expectedAgents, [long]$logOffset) {
    $before = @(Get-SoloTerminals $newCli $dataDir)
    $shown = @($before | Where-Object { $_.tab } | ForEach-Object { $_.tab } | Sort-Object -Unique)
    Add-Result $phase "before opening other tabs: terminals of tabs not opened yet weren't moved in" (@($before | Where-Object { $_.directory -and $_.tab -and -not ($_.directory -like "*\$($_.tab)*") }).Count -eq 0) (($before | ForEach-Object { "$($_.tab):$($_.directory)" }) -join '; ')
    Save-SoloShot $solo (Join-Path $out "$phase-before-tabs.png")
    foreach ($project in $projects) {
        Open-InSolo $newExe $dataDir (Join-Path $reposDir $project)
        Start-Sleep -Seconds 6
        Save-SoloShot $solo (Join-Path $out "$phase-$project.png")
    }
    Open-InSolo $newExe $dataDir (Join-Path $reposDir $projects[0])
    Start-Sleep -Seconds 4
    $terminals = @(Get-SoloTerminals $newCli $dataDir)
    ($terminals | ConvertTo-Json -Depth 4) | Set-Content -Encoding UTF8 (Join-Path $out "$phase-agent-ls.json")
    foreach ($project in $projects) {
        $repo = Join-Path $reposDir $project
        $mine = @($terminals | Where-Object { $_.directory -and ($_.directory -like "$repo*") })
        $agent = $agents[$project]
        Add-Result $phase "$project has one terminal (no extra empty shell)" ($mine.Count -eq 1) "$($mine.Count) terminal(s)"
        if ($mine.Count -lt 1) { continue }
        $terminal = $mine[0]
        if ($null -ne $terminal.PSObject.Properties['tab']) {
            Add-Result $phase "$project's terminal is in the $project tab" ($terminal.tab -eq $project) "tab: $($terminal.tab)"
        } else {
            Add-Result $phase "$project's terminal tab (this build's agent ls has no tab field)" $true 'checked through the log and the terminal counts instead'
        }
        Add-Result $phase "$project's agent is recognized" ($terminal.agent -eq $agent) "agent: $($terminal.agent), status: $($terminal.status)"
        Add-Result $phase "$project's title is the agent" ([string]$terminal.title -like "*$agent*") "title: $($terminal.title)"
        if ($null -ne $terminal.PSObject.Properties['ends_with_solo']) {
            Add-Result $phase "$project's terminal doesn't end with Solo" (-not $terminal.ends_with_solo -and $terminal.hosted) "hosted: $($terminal.hosted), ends_with_solo: $($terminal.ends_with_solo)"
        }
        $expected = @($expectedAgents | Where-Object { $_.project -eq $project })
        $live = @(Get-FakeAgents $Root | Where-Object { $_.project -eq $project -and $_.alive })
        $same = $expected.Count -gt 0 -and @($expected | Where-Object { $p = $_.pid; $live | Where-Object { $_.pid -eq $p } }).Count -eq $expected.Count
        Add-Result $phase "$project's agent is the same process" $same "before: $($expected.pid -join ','), running: $($live.pid -join ',')"
    }
    $log = @(Get-SoloLogSince $dataDir $logOffset)
    $log | Set-Content -Encoding UTF8 (Join-Path $out "$phase-Solo.log")
    # The level after the time: "error 2" in a message isn't one.
    $bad = @($log | Where-Object { $_ -cmatch '^\S+\s+(ERROR|WARN)\s' } | Where-Object { $line = $_; -not ($allowed | Where-Object { $line -like "*$_*" }) })
    Add-Result $phase 'Solo.log has no ERROR or WARN outside the allowed list' ($bad.Count -eq 0) (($bad | Select-Object -First 5) -join ' | ')
    $moved = @($log | Where-Object { $_ -match 'already open in this Solo' })
    Add-Result $phase "no saved terminal was 'already open in this Solo'" ($moved.Count -eq 0) "$($moved.Count) line(s)"
    $hostFailed = @($log | Where-Object { $_ -match 'failed to start a terminal host, the terminal ends with Solo' })
    Add-Result $phase 'every terminal host started' ($hostFailed.Count -eq 0) "$($hostFailed.Count) line(s)"
}

$exitCode = 0
$solo = $null
try {
    # 1. The previous release, with an agent in each of 3 tabs.
    Write-Step "1. the previous release ($oldExe)"
    # A new build has its own single instance for its data folder, and may
    # run beside another Solo; the previous release may not.
    $solo = Start-Solo $oldExe $dataDir (Join-Path $reposDir $projects[0]) -Isolated:($sameBuild -and $IsolatedNewBuild)
    Start-Sleep -Seconds 10
    for ($ix = 0; $ix -lt $projects.Count; $ix++) {
        $project = $projects[$ix]
        if ($ix -gt 0) {
            Open-InSolo $oldExe $dataDir (Join-Path $reposDir $project)
            Start-Sleep -Seconds 8
        }
        Send-SoloCommand $solo $agents[$project]
        Start-Sleep -Seconds 4
    }
    Open-InSolo $oldExe $dataDir (Join-Path $reposDir $projects[0])
    $started = Wait-Until { @(Get-FakeAgents $Root | Where-Object { $_.alive }).Count -ge $projects.Count } 60
    Add-Result 'old' 'an agent runs in each tab' $started (((Get-FakeAgents $Root) | ForEach-Object { "$($_.agent)@$($_.project)" }) -join ', ')
    Start-Sleep -Seconds 15
    Save-SoloShot $solo (Join-Path $out 'old-running.png')
    $agentsBefore = @(Get-FakeAgents $Root | Where-Object { $_.alive })

    # 2. "Update": the old release quits keeping its agents; the new one starts.
    $offset = Get-SoloLogLength $dataDir
    if ($sameBuild) {
        Write-Step '2. update: the new build restarts itself (workspace: reload)'
        $solo = Invoke-SoloRestart $solo $newExe
        Add-Result 'update' 'the agents outlived the restart' (@(Get-FakeAgents $Root | Where-Object { $_.alive }).Count -eq $agentsBefore.Count)
    } else {
        Write-Step '2. update: quitting the previous release, starting the new build'
        Close-SoloWindow $solo
        if (-not $solo.WaitForExit(30000)) {
            Save-SoloShot $solo (Join-Path $out 'old-quit-prompt.png')
            throw 'the previous release asked before quitting; this script expects it to keep its agents (up to 2.5.3)'
        }
        Start-Sleep -Seconds 3
        Add-Result 'update' 'the agents outlived the old Solo' (@(Get-FakeAgents $Root | Where-Object { $_.alive }).Count -eq $agentsBefore.Count)
        $offset = Get-SoloLogLength $dataDir
        $solo = Start-Solo $newExe $dataDir -Isolated:$IsolatedNewBuild
    }

    # 3. Not opening the other tabs for a while.
    Write-Step "3. waiting $WaitSeconds seconds in the first tab"
    Start-Sleep -Seconds $WaitSeconds
    Test-Restored 'update' $solo $agentsBefore $offset

    # 5. A crash.
    if (-not $SkipCrash) {
        Write-Step '5. crash: taskkill /F'
        Stop-SoloHard $solo
        Start-Sleep -Seconds 3
        Add-Result 'crash' 'the agents outlived the crash' (@(Get-FakeAgents $Root | Where-Object { $_.alive }).Count -eq $agentsBefore.Count)
        $offset = Get-SoloLogLength $dataDir
        $solo = Start-Solo $newExe $dataDir -Isolated:$IsolatedNewBuild
        Start-Sleep -Seconds $WaitSeconds
        Test-Restored 'crash' $solo $agentsBefore $offset
    }

    # 6. A normal quit.
    if (-not $SkipQuit) {
        Write-Step '6. normal quit: closing the window'
        Close-SoloWindow $solo
        if ($solo.WaitForExit(15000)) {
            Start-Sleep -Seconds 5
            $left = @(Get-FakeAgents $Root | Where-Object { $_.alive })
            Add-Result 'quit' 'quitting asks before it ends running agents' $false "it quit without asking; $($left.Count) agent(s) kept running (the behavior up to 2.5.3)"
        } else {
            Save-SoloShot $solo (Join-Path $out 'quit-prompt.png')
            Add-Result 'quit' 'quitting asks before it ends running agents' $true
            Send-SoloEnter $solo
            $quit = $solo.WaitForExit(30000)
            Add-Result 'quit' 'confirming quits' $quit
            $ended = Wait-Until { @(Get-FakeAgents $Root | Where-Object { $_.alive }).Count -eq 0 } 30
            Add-Result 'quit' 'confirming ends the agents' $ended "$(@(Get-FakeAgents $Root | Where-Object { $_.alive }).Count) still running"
        }
    }
} catch {
    Add-Result 'script' 'ran to the end' $false $_.Exception.Message
} finally {
    foreach ($process in $script:SoloStarted) {
        if (-not $process.HasExited) {
            try { Stop-SoloHard $process } catch { Write-Host $_ }
        }
    }
    Stop-ScenarioLeftovers $Root
}

$failed = @($results | Where-Object { -not $_.Ok })
$report = @("# Upgrade scenario", "", "- old: $OldDir", "- new: $NewDir", "- data: $dataDir", "", "| Phase | Check | Result | Detail |", "|---|---|---|---|")
$report += $results | ForEach-Object { "| $($_.Phase) | $($_.Check) | $(if ($_.Ok) { 'ok' } else { 'FAIL' }) | $($_.Detail -replace '\|', '/') |" }
$report += '', "$($results.Count - $failed.Count) passed, $($failed.Count) failed."
$report | Set-Content -Encoding UTF8 (Join-Path $out 'report.md')
Write-Host ''
Write-Host "$($results.Count - $failed.Count) passed, $($failed.Count) failed; report: $(Join-Path $out 'report.md')"
if ($failed.Count -gt 0) { $exitCode = 1 }
exit $exitCode
