# Installs Solo from an unpacked release zip for the current user: copies it to
# %LOCALAPPDATA%\Programs\Solo, adds Start menu and desktop shortcuts, and adds
# "Open with Solo" to Explorer's context menu (files, folders, a folder's
# background, drives; on Windows 11 under "Show more options").
#
#   powershell -ExecutionPolicy Bypass -File install.ps1 [-NoContextMenu] [-Uninstall]
#
# -NoContextMenu leaves the context menu out (and removes it if an earlier
# install added it); -Uninstall removes it along with everything else.

param([switch]$Uninstall, [switch]$NoContextMenu)
$ErrorActionPreference = "Stop"

# The keys the context menu lives in, under HKCU\<classes>, the same ones
# solo.iss writes. "%1" is the file; "%V" the folder, the folder whose
# background was clicked, or the drive.
$contextMenuKeys = [ordered]@{
    "*\shell\Solo" = "%1"
    "Directory\shell\Solo" = "%V"
    "Directory\Background\shell\Solo" = "%V"
    "Drive\shell\Solo" = "%V"
}

function Get-ContextMenuText {
    if ((Get-UICulture).Name -like "zh*") {
        # The Chinese for "Open with Solo", spelled out so this script stays
        # ASCII (Windows PowerShell reads a .ps1 without a BOM in the ANSI
        # code page).
        return "$([char]0x901A)$([char]0x8FC7) Solo $([char]0x6253)$([char]0x5F00)"
    }
    return "Open with Solo"
}

# Writes the context menu keys under HKCU\$Classes for $Exe.
function Add-SoloContextMenu([string]$Exe, [string]$Classes = "Software\Classes", [string]$Text = (Get-ContextMenuText)) {
    foreach ($entry in $contextMenuKeys.GetEnumerator()) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey("$Classes\$($entry.Key)")
        try {
            $key.SetValue("", $Text)
            $key.SetValue("Icon", $Exe)
            $command = $key.CreateSubKey("command")
            try { $command.SetValue("", "`"$Exe`" `"$($entry.Value)`"") } finally { $command.Close() }
        } finally { $key.Close() }
    }
}

# Removes the context menu keys under HKCU\$Classes, if they're there.
function Remove-SoloContextMenu([string]$Classes = "Software\Classes") {
    foreach ($name in $contextMenuKeys.Keys) {
        [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree("$Classes\$name", $false)
    }
}

# Dot-sourced (by a test): only the definitions above.
if ($MyInvocation.InvocationName -eq ".") { return }

$installDir = Join-Path $env:LOCALAPPDATA "Programs\Solo"
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Solo.lnk"
$desktop = Join-Path ([Environment]::GetFolderPath("Desktop")) "Solo.lnk"

Get-Process solo -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -like "$installDir\*" } |
    ForEach-Object { Write-Host "Closing the running Solo..."; $_.CloseMainWindow() | Out-Null; $_.WaitForExit(5000) | Out-Null }

if ($Uninstall) {
    Remove-Item -Force $startMenu, $desktop -ErrorAction SilentlyContinue
    Remove-SoloContextMenu
    if (Test-Path $installDir) { Remove-Item -Recurse -Force $installDir }
    Write-Host "Solo uninstalled. Settings stay in $env:APPDATA\Solo and $env:LOCALAPPDATA\Solo."
    return
}

New-Item -ItemType Directory -Force $installDir | Out-Null
Get-ChildItem $PSScriptRoot | Where-Object { $_.Name -ne "install.ps1" } |
    Copy-Item -Destination $installDir -Recurse -Force
Copy-Item (Join-Path $PSScriptRoot "install.ps1") $installDir -Force

$exe = Join-Path $installDir "solo.exe"
$shell = New-Object -ComObject WScript.Shell
foreach ($link in $startMenu, $desktop) {
    $shortcut = $shell.CreateShortcut($link)
    $shortcut.TargetPath = $exe
    $shortcut.WorkingDirectory = $env:USERPROFILE
    $shortcut.Description = "Solo: a terminal-first editor"
    $shortcut.Save()
}

if ($NoContextMenu) {
    Remove-SoloContextMenu
    Write-Host "Solo installed to $installDir, with Start menu and desktop shortcuts."
} else {
    Add-SoloContextMenu $exe
    Write-Host "Solo installed to $installDir, with Start menu and desktop shortcuts and `"Open with Solo`" in Explorer's context menu."
}
