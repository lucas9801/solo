# Packages Solo as an MSIX (R12 trial: distributing Solo through the
# Microsoft Store). It doesn't build anything: it takes the files staged by
# script\package-solo.ps1 -Step Stage (dist\Solo), or any folder with the same
# exes, and doesn't touch the Inno Setup installer or the zip.
#
#   script\package-solo-msix.ps1 [-SourceDir dist\Solo] [-OutDir dist] [-Version 2.3.0.0]
#       [-IdentityName Solo] [-Publisher "CN=..."] [-PublisherDisplayName Solo]
#       [-Unvirtualized] [-SoloArguments "..."] [-AliasSuffix ""] [-Protocol solo]
#       [-CertificateThumbprint <sha1>] [-KeepLayout]
#
# The package holds solo.exe, solo-cli.exe, solo-agent-hook.exe,
# solo-terminal-host.exe and solo-update-helper.exe (with conpty.dll and
# OpenConsole.exe) and runs Solo as a full-trust desktop app (runFullTrust).
# For the Store, -IdentityName, -Publisher and -PublisherDisplayName are the
# values Partner Center shows under Product identity, and the package goes up
# unsigned (the Store signs it). For sideloading it's signed with
# -CertificateThumbprint, a code-signing certificate in Cert:\CurrentUser\My
# whose subject is -Publisher.
#
# -Unvirtualized declares the restricted capability unvirtualizedResources and
# turns file system and registry write virtualization off, so Solo's writes to
# %APPDATA% and %LOCALAPPDATA% land in the real folders rather than in the
# package's private ones.
#
# -SoloArguments are passed to solo.exe whenever it's started from the
# package (Start menu, solo://), e.g. "--user-data-dir C:\scratch" for tests.
# -AliasSuffix renames the execution aliases (solo<suffix>.exe, ...), so a
# test package doesn't shadow an installed Solo on PATH.

param(
    [string]$SourceDir = "dist\Solo",
    [string]$OutDir = "dist",
    [string]$Version = "",
    [string]$IdentityName = "Solo",
    [string]$Publisher = "CN=Solo MSIX Test",
    [string]$PublisherDisplayName = "Solo",
    [string]$DisplayName = "Solo",
    [switch]$Unvirtualized,
    [string]$SoloArguments = "",
    [string]$AliasSuffix = "",
    [string]$Protocol = "solo",
    [string]$CertificateThumbprint = "",
    [string]$PackageName = "",
    [switch]$KeepLayout
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$files = @("solo.exe", "solo-cli.exe", "solo-agent-hook.exe", "solo-terminal-host.exe", "solo-update-helper.exe")
$optional = @("conpty.dll", "OpenConsole.exe", "LICENSE-GPL", "licenses.md", "THIRD-PARTY-VENDORED.md", "LICENSE-command-signatures.md")

function Find-SdkTool([string]$name) {
    $found = (Get-Command $name -ErrorAction SilentlyContinue).Source
    if ($found) { return $found }
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $found = Get-ChildItem $kits -Directory -Filter "10.*" -ErrorAction SilentlyContinue |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "x64\$name" } |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $found) { throw "$name not found: install the Windows SDK" }
    return $found
}

# MSIX versions have four parts, and the Store wants the last one to be 0.
if (-not $Version) {
    $crate = (Select-String -Path "crates\zed\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
    $Version = ($crate -replace '-.*$', '') + ".0"
}
if ($Version -notmatch '^\d+\.\d+\.\d+\.\d+$') { throw "-Version must look like 2.3.0.0, not $Version" }

foreach ($file in $files) {
    if (-not (Test-Path (Join-Path $SourceDir $file))) { throw "$file isn't in $SourceDir; stage Solo first (script\package-solo.ps1 -Step Stage)" }
}

New-Item -ItemType Directory -Force $OutDir | Out-Null
if (-not $PackageName) { $PackageName = "Solo-$Version-x64" }
$layout = Join-Path $OutDir "$PackageName-layout"
$msix = Join-Path $OutDir "$PackageName.msix"
if (Test-Path $layout) { Remove-Item -Recurse -Force $layout }
New-Item -ItemType Directory -Force (Join-Path $layout "Assets") | Out-Null

foreach ($file in $files + $optional) {
    $path = Join-Path $SourceDir $file
    if (Test-Path $path) { Copy-Item $path $layout }
}

# The tiles, scaled from Solo's icon.
Add-Type -AssemblyName System.Drawing
$icon = [System.Drawing.Image]::FromFile((Resolve-Path "crates\zed\resources\app-icon@2x.png").Path)
try {
    foreach ($tile in @(@("Square44x44Logo.png", 44), @("Square150x150Logo.png", 150), @("StoreLogo.png", 50))) {
        $size = $tile[1]
        $bitmap = New-Object System.Drawing.Bitmap $size, $size
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $graphics.DrawImage($icon, 0, 0, $size, $size)
        $graphics.Dispose()
        $bitmap.Save((Join-Path $layout "Assets\$($tile[0])"), [System.Drawing.Imaging.ImageFormat]::Png)
        $bitmap.Dispose()
    }
} finally {
    $icon.Dispose()
}

function ConvertTo-XmlAttribute([string]$text) {
    [System.Security.SecurityElement]::Escape($text)
}

$soloParameters = ""
if ($SoloArguments) { $soloParameters = " uap10:Parameters=`"$(ConvertTo-XmlAttribute $SoloArguments)`"" }
$protocolParameters = (($SoloArguments, '"%1"') | Where-Object { $_ }) -join " "

$unvirtualizedProperties = ""
$unvirtualizedCapability = ""
if ($Unvirtualized) {
    $unvirtualizedProperties = @"
    <desktop6:FileSystemWriteVirtualization>disabled</desktop6:FileSystemWriteVirtualization>
    <desktop6:RegistryWriteVirtualization>disabled</desktop6:RegistryWriteVirtualization>
"@
    $unvirtualizedCapability = '    <rescap:Capability Name="unvirtualizedResources" />'
}

$manifest = [System.IO.File]::ReadAllText((Resolve-Path "script\msix\AppxManifest.xml.in").Path)
$manifest = $manifest.
    Replace("@IDENTITY_NAME@", (ConvertTo-XmlAttribute $IdentityName)).
    Replace("@PUBLISHER@", (ConvertTo-XmlAttribute $Publisher)).
    Replace("@VERSION@", $Version).
    Replace("@DISPLAY_NAME@", (ConvertTo-XmlAttribute $DisplayName)).
    Replace("@PUBLISHER_DISPLAY_NAME@", (ConvertTo-XmlAttribute $PublisherDisplayName)).
    Replace("@ALIAS_SUFFIX@", (ConvertTo-XmlAttribute $AliasSuffix)).
    Replace("@PROTOCOL@", (ConvertTo-XmlAttribute $Protocol)).
    Replace("@PROTOCOL_PARAMETERS@", (ConvertTo-XmlAttribute $protocolParameters)).
    Replace("@SOLO_PARAMETERS@", $soloParameters).
    Replace("<!--@UNVIRTUALIZED_PROPERTIES@-->", $unvirtualizedProperties).
    Replace("<!--@UNVIRTUALIZED_CAPABILITY@-->", $unvirtualizedCapability)
[System.IO.File]::WriteAllText((Join-Path (Resolve-Path $layout).Path "AppxManifest.xml"), $manifest, (New-Object System.Text.UTF8Encoding $false))

# resources.pri, so the Start menu and the Store find the tiles.
$makepri = Find-SdkTool "makepri.exe"
$priConfig = Join-Path $OutDir "$PackageName-priconfig.xml"
& $makepri createconfig /cf $priConfig /dq en-US /pv 10.0.0 /o | Out-Null
if ($LASTEXITCODE -ne 0) { throw "makepri createconfig failed" }
& $makepri new /pr $layout /cf $priConfig /mn (Join-Path $layout "AppxManifest.xml") /of (Join-Path $layout "resources.pri") /o | Out-Null
if ($LASTEXITCODE -ne 0) { throw "makepri new failed" }
Remove-Item $priConfig

$makeappx = Find-SdkTool "makeappx.exe"
if (Test-Path $msix) { Remove-Item -Force $msix }
$output = & $makeappx pack /d $layout /p $msix /o 2>&1
if ($LASTEXITCODE -ne 0) { $output | Write-Host; throw "makeappx pack failed" }

if ($CertificateThumbprint) {
    $signtool = Find-SdkTool "signtool.exe"
    & $signtool sign /fd SHA256 /sha1 $CertificateThumbprint /s My $msix
    if ($LASTEXITCODE -ne 0) { throw "signtool failed" }
}

if (-not $KeepLayout) { Remove-Item -Recurse -Force $layout }
Write-Host "Packaged $msix"
