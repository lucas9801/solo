# Helpers for the scenario scripts (prepare-user-like-data.ps1,
# upgrade-scenario.ps1): start Solo with its own data folder, drive it through
# its own window only (PostMessage, PrintWindow: no global input, no focus
# stealing), list its terminals with solo-cli, read its log, and the fake
# agents. Dot-source it:  . "$PSScriptRoot\solo-scenario-lib.ps1"
#
# Only Windows. ASCII only (Windows PowerShell reads scripts in the ANSI code
# page).

$ErrorActionPreference = 'Stop'

if (-not ('SoloScenario.W' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
namespace SoloScenario {
public static class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static IntPtr Find(uint pid, string cls) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p == pid) {
        var sb = new StringBuilder(256); GetClassName(h, sb, 256);
        if (sb.ToString() == cls) { found = h; return false; }
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
}
'@
    Add-Type -AssemblyName System.Drawing
}

# The Solo processes the scenario started: only these are ever ended.
if (-not $script:SoloStarted) { $script:SoloStarted = @() }

function Write-Step([string]$message) {
    Write-Host ("[{0:HH:mm:ss}] {1}" -f (Get-Date), $message)
}

# A stable Solo shares its single-instance pipe with any other stable Solo
# (an installed one): with one running, ours would hand its folders to it,
# and solo-cli would ask it. So none may run but ours.
function Assert-NoOtherSolo {
    $ours = @($script:SoloStarted | ForEach-Object { $_.Id })
    $others = @(Get-Process -Name solo -ErrorAction SilentlyContinue | Where-Object { $ours -notcontains $_.Id })
    if ($others.Count -gt 0) {
        throw "another Solo runs (pid $($others.Id -join ', ')): quit it first; the scenario's Solo would hand its folders to it"
    }
}

# The environment Solo and its terminals get: the fake agents first on PATH,
# the UI language, the window off screen.
function Set-ScenarioEnvironment([string]$Root, [string]$Language) {
    $env:SOLO_SCENARIO_ROOT = $Root
    $env:SOLO_LANGUAGE = $Language
    $env:SOLO_WINDOW_POSITION = '-3000,40'
    $fakebin = Join-Path $Root 'fakebin'
    if (-not ($env:PATH -split ';' | Where-Object { $_ -eq $fakebin })) {
        $env:PATH = "$fakebin;$env:PATH"
    }
}

# Starts Solo from $Exe with its data in $DataDir (opening $Path if given),
# and waits for its window. Returns the process.
function Start-Solo([string]$Exe, [string]$DataDir, [string]$Path = '') {
    Assert-NoOtherSolo
    $arguments = @('--user-data-dir', "`"$DataDir`"")
    if ($Path) { $arguments += "`"$Path`"" }
    $started = Get-Date
    $process = Start-Process -FilePath $Exe -WorkingDirectory (Split-Path $Exe) -ArgumentList $arguments -WindowStyle Minimized -PassThru
    $script:SoloStarted += $process
    for ($i = 0; $i -lt 240; $i++) {
        Start-Sleep -Milliseconds 500
        if ($process.HasExited) { throw "Solo exited at once (code $($process.ExitCode))" }
        $hwnd = [SoloScenario.W]::Find([uint32]$process.Id, 'Zed::Window')
        if ($hwnd -ne [IntPtr]::Zero) {
            [SoloScenario.W]::SetWindowPos($hwnd, [IntPtr]::Zero, -3000, 40, 1400, 900, 0x0014) | Out-Null
            [SoloScenario.W]::ShowWindowAsync($hwnd, 4) | Out-Null
            Start-Sleep -Milliseconds 300
            [SoloScenario.W]::SetWindowPos($hwnd, [IntPtr]::Zero, -3000, 40, 1400, 900, 0x0014) | Out-Null
            Write-Step "Solo $($process.Id) started from $Exe"
            $process | Add-Member -NotePropertyName ScenarioStart -NotePropertyValue $started -Force
            return $process
        }
    }
    throw 'Solo showed no window in two minutes'
}

# Opens $Path in the running Solo, as opening a folder from outside does: a
# second solo.exe hands it over and exits. Opens (or shows) its tab.
function Open-InSolo([string]$Exe, [string]$DataDir, [string]$Path) {
    $p = Start-Process -FilePath $Exe -WorkingDirectory (Split-Path $Exe) -ArgumentList @('--user-data-dir', "`"$DataDir`"", "`"$Path`"") -WindowStyle Minimized -PassThru
    if (-not $p.WaitForExit(30000)) { throw "the second solo.exe for $Path didn't hand over" }
}

function Get-SoloWindow($process) {
    $hwnd = [SoloScenario.W]::Find([uint32]$process.Id, 'Zed::Window')
    if ($hwnd -eq [IntPtr]::Zero) { throw "no window for Solo $($process.Id)" }
    $hwnd
}

function Send-SoloText($process, [string]$text) {
    $hwnd = Get-SoloWindow $process
    foreach ($char in $text.ToCharArray()) {
        [SoloScenario.W]::PostMessage($hwnd, 0x0102, [IntPtr][int]$char, [IntPtr]1) | Out-Null
        Start-Sleep -Milliseconds 15
    }
}

function Send-SoloKey($process, [int]$virtualKey, [int]$scanCode) {
    $hwnd = Get-SoloWindow $process
    [SoloScenario.W]::PostMessage($hwnd, 0x0100, [IntPtr]$virtualKey, [IntPtr](1 -bor ($scanCode -shl 16))) | Out-Null
    [SoloScenario.W]::PostMessage($hwnd, 0x0101, [IntPtr]$virtualKey, [IntPtr]([int64]0xC0000001 -bor ($scanCode -shl 16))) | Out-Null
}

function Send-SoloEnter($process) { Send-SoloKey $process 0x0D 0x1C }

function Send-SoloClick($process, [int]$x, [int]$y, [switch]$Right) {
    $hwnd = Get-SoloWindow $process
    $lParam = [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF))
    [SoloScenario.W]::PostMessage($hwnd, 0x0200, [IntPtr]0, $lParam) | Out-Null
    Start-Sleep -Milliseconds 60
    if ($Right) { $down, $up, $flag = 0x0204, 0x0205, 2 } else { $down, $up, $flag = 0x0201, 0x0202, 1 }
    [SoloScenario.W]::PostMessage($hwnd, $down, [IntPtr]$flag, $lParam) | Out-Null
    Start-Sleep -Milliseconds 60
    [SoloScenario.W]::PostMessage($hwnd, $up, [IntPtr]0, $lParam) | Out-Null
}

# Closes Solo's window as its title bar's x does (WM_CLOSE).
function Close-SoloWindow($process) {
    [SoloScenario.W]::PostMessage((Get-SoloWindow $process), 0x0010, [IntPtr]0, [IntPtr]0) | Out-Null
}

# Ends Solo as a crash would: only a Solo this scenario started.
function Stop-SoloHard($process) {
    $ours = $script:SoloStarted | Where-Object { $_.Id -eq $process.Id }
    $live = Get-Process -Id $process.Id -ErrorAction SilentlyContinue
    if (-not $ours -or -not $live -or $live.ProcessName -ne 'solo' -or $live.StartTime -lt $process.ScenarioStart.AddSeconds(-2)) {
        throw "refusing to end process $($process.Id): not the Solo this scenario started"
    }
    & taskkill.exe /F /PID $process.Id | Out-Null
    $process.WaitForExit(30000) | Out-Null
}

function Save-SoloShot($process, [string]$file) {
    $hwnd = Get-SoloWindow $process
    $r = New-Object SoloScenario.W+RECT
    [SoloScenario.W]::GetWindowRect($hwnd, [ref]$r) | Out-Null
    $bitmap = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $hdc = $graphics.GetHdc()
    [SoloScenario.W]::PrintWindow($hwnd, $hdc, 2) | Out-Null
    $graphics.ReleaseHdc($hdc)
    $bitmap.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose(); $bitmap.Dispose()
}

# `solo-cli agent ls --json` of the running Solo, as objects.
function Get-SoloTerminals([string]$Cli) {
    $output = & $Cli agent ls --json 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { throw "solo-cli agent ls failed: $output" }
    $parsed = $output | ConvertFrom-Json
    @($parsed)
}

# The lines Solo logged since $offset (a byte offset into Solo.log).
function Get-SoloLogSince([string]$DataDir, [long]$offset) {
    $log = Join-Path $DataDir 'logs\Solo.log'
    if (-not (Test-Path $log)) { return @() }
    $stream = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite')
    try {
        if ($offset -gt $stream.Length) { $offset = 0 }
        $stream.Seek($offset, 'Begin') | Out-Null
        $reader = New-Object IO.StreamReader($stream, [Text.Encoding]::UTF8)
        @($reader.ReadToEnd() -split "`r?`n" | Where-Object { $_ })
    } finally { $stream.Dispose() }
}

function Get-SoloLogLength([string]$DataDir) {
    $log = Join-Path $DataDir 'logs\Solo.log'
    if (Test-Path $log) { (Get-Item $log).Length } else { 0 }
}

# The fake agents running, from the files they write when they start:
# agent, project, pid and its creation time.
function Get-FakeAgents([string]$Root) {
    Get-ChildItem (Join-Path $Root 'pids') -Filter '*.json' -ErrorAction SilentlyContinue | ForEach-Object {
        $entry = Get-Content $_.FullName -Raw | ConvertFrom-Json
        $process = Get-Process -Id $entry.pid -ErrorAction SilentlyContinue
        $alive = $process -and ($process.StartTime.ToUniversalTime().ToString('o') -eq $entry.started)
        $entry | Add-Member -NotePropertyName alive -NotePropertyValue ([bool]$alive) -PassThru
    }
}

# Ends what the scenario left running: its fake agents (checked by pid and
# start time) and the terminal hosts started from its data folder.
function Stop-ScenarioLeftovers([string]$Root) {
    foreach ($agent in @(Get-FakeAgents $Root | Where-Object { $_.alive })) {
        $process = Get-Process -Id $agent.pid -ErrorAction SilentlyContinue
        if ($process -and $process.ProcessName -eq 'powershell') {
            Stop-Process -Id $agent.pid -Force -ErrorAction SilentlyContinue
        }
    }
    $hostDir = Join-Path $Root 'data'
    Get-Process -Name solo-terminal-host -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($hostDir, [StringComparison]::OrdinalIgnoreCase) } | ForEach-Object {
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
}
