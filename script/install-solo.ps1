# Installs Solo from an unpacked release zip for the current user: copies it to
# %LOCALAPPDATA%\Programs\Solo and adds Start menu and desktop shortcuts.
#
#   powershell -ExecutionPolicy Bypass -File install.ps1 [-Uninstall]

param([switch]$Uninstall)
$ErrorActionPreference = "Stop"

$installDir = Join-Path $env:LOCALAPPDATA "Programs\Solo"
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Solo.lnk"
$desktop = Join-Path ([Environment]::GetFolderPath("Desktop")) "Solo.lnk"

Get-Process solo -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -like "$installDir\*" } |
    ForEach-Object { Write-Host "Closing the running Solo..."; $_.CloseMainWindow() | Out-Null; $_.WaitForExit(5000) | Out-Null }

if ($Uninstall) {
    Remove-Item -Force $startMenu, $desktop -ErrorAction SilentlyContinue
    if (Test-Path $installDir) { Remove-Item -Recurse -Force $installDir }
    Write-Host "Solo uninstalled. Settings stay in $env:APPDATA\Solo and $env:LOCALAPPDATA\Solo."
    return
}

New-Item -ItemType Directory -Force $installDir | Out-Null
Get-ChildItem $PSScriptRoot | Where-Object { $_.Name -ne "install.ps1" } |
    Copy-Item -Destination $installDir -Recurse -Force
Copy-Item (Join-Path $PSScriptRoot "install.ps1") $installDir -Force

$shell = New-Object -ComObject WScript.Shell
foreach ($link in $startMenu, $desktop) {
    $shortcut = $shell.CreateShortcut($link)
    $shortcut.TargetPath = Join-Path $installDir "solo.exe"
    $shortcut.WorkingDirectory = $env:USERPROFILE
    $shortcut.Description = "Solo: a terminal-first editor"
    $shortcut.Save()
}
Write-Host "Solo installed to $installDir, with Start menu and desktop shortcuts."
