# Packages Solo as an MSIX: for the Microsoft Store ("Solo Code"), or a test
# package signed with a self-signed certificate for sideloading. It doesn't
# build Solo: it takes the release build staged by
# script\package-solo.ps1 -Step Stage (dist\Solo), or any folder with the same
# exes, and doesn't touch the zip.
#
# For the Store:
#
#   script\package-solo-msix.ps1 -Store [-SourceDir dist\Solo] [-OutDir dist]
#       [-ExplorerCommandDll <path>] [-SymbolsDir <folder with .pdb files>]
#
# writes dist\SoloCode-<version>-x64.msix and .msixupload, unsigned (the Store
# signs it), with the identity Partner Center gives Solo Code (Product
# identity) and the Explorer context menu. It refuses
# a debug build (it reads assets and shaders from the source tree, so it
# crashes once installed) and a build that isn't of the stable channel.
#
# A test package, to sideload:
#
#   script\package-solo-msix.ps1 [-SourceDir dist\Solo] [-OutDir dist] [-Version 2.3.0.0]
#       [-IdentityName Solo] [-Publisher "CN=..."] [-PublisherDisplayName Solo]
#       [-DisplayName Solo] [-SoloArguments "..."] [-AliasSuffix ""]
#       [-Protocol solo] [-CertificateThumbprint <sha1>] [-ExplorerCommandDll <path>]
#       [-NoContextMenu] [-KeepLayout]
#
# e.g. with -IdentityName SoloMsixTest -DisplayName "Solo MSIX Test"
# -AliasSuffix -msixtest -Protocol solo-msixtest
# -SoloArguments "--user-data-dir C:\scratch\solo" -CertificateThumbprint ...,
# so it doesn't get in the way of an installed Solo. It's signed with
# -CertificateThumbprint, a code-signing certificate in Cert:\CurrentUser\My
# whose subject is -Publisher, which Windows has to trust to install it
# (Cert:\LocalMachine\TrustedPeople; adding it there takes an administrator).
#
# The package holds solo.exe, solo-cli.exe, solo-agent-hook.exe,
# solo-terminal-host.exe and solo-update-helper.exe (with conpty.dll and
# OpenConsole.exe) and runs Solo as a full-trust desktop app (runFullTrust),
# with an execution alias for solo, solo://
# links, "Open with" for text and code files, and "Open with Solo Code" on
# folders and files in Explorer's context menu.
#
# The context menu is a COM class (IExplorerCommand) in
# solo-explorer-command.dll, built from crates\explorer_command_injector:
#
#   cargo build --profile solo-dist -p explorer_command_injector --no-default-features --features solo
#
# -ExplorerCommandDll is the built DLL; by default it's looked for in
# -SourceDir (as solo-explorer-command.dll) and in the cargo target folder
# (CARGO_TARGET_DIR or target, solo-dist\explorer_command_injector.dll). A
# test package goes without the menu if there's none (or with -NoContextMenu).
#
# No package declares the restricted capability unvirtualizedResources: the
# Store refused it to Solo Code. So Windows redirects Solo's new files in
# %APPDATA% and %LOCALAPPDATA% (its data folder among them) to the package's
# own folder; what Solo hands to the shells and agents in its terminals,
# which run outside the package, goes to a folder they see too (see
# util::packaged::package_shared_dir). The script refuses a manifest with
# the capability.
#
# -SoloArguments are passed to solo.exe whenever it's started from the
# package (Start menu, solo://, files), e.g. "--user-data-dir C:\scratch" for
# tests. -AliasSuffix renames the execution aliases (solo<suffix>.exe, ...), so
# a test package doesn't shadow an installed Solo on PATH.
#
# -SymbolsDir: the .pdb files of Solo's exes (the cargo target folder's
# solo-dist), put in the .msixupload as a .appxsym, so Partner Center can
# symbolicate crash reports.

param(
    [switch]$Store,
    [string]$SourceDir = "dist\Solo",
    [string]$OutDir = "dist",
    [string]$Version = "",
    [string]$IdentityName = "Solo",
    [string]$Publisher = "CN=Solo MSIX Test",
    [string]$PublisherDisplayName = "Solo",
    [string]$DisplayName = "Solo",
    [string]$SoloArguments = "",
    [string]$AliasSuffix = "",
    [string]$Protocol = "solo",
    [string]$CertificateThumbprint = "",
    [string]$ExplorerCommandDll = "",
    [switch]$NoContextMenu,
    [string]$SymbolsDir = "",
    [string]$PackageName = "",
    [switch]$KeepLayout
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
. (Join-Path $PSScriptRoot "msix\msix-version.ps1")

# Solo Code's identity in Partner Center (Product management > Product identity).
$storeIdentity = @{
    IdentityName = "lucas9801.SoloCode"
    Publisher = "CN=43444C18-4800-4DDC-8B31-3ED856DA4453"
    PublisherDisplayName = "lucas9801"
    DisplayName = "Solo Code"
}
# The COM classes of the context menu (crates\explorer_command_injector, the
# "solo" feature): the Store's, and test packages', so both can be installed.
$storeContextMenuClsid = "7ce78673-39cf-4de5-a3dc-1fe09600cdf6"
$testContextMenuClsid = "fc3eeb9f-ca0a-4cf8-9fe9-f0c668e0fde4"

if ($Store) {
    foreach ($name in "IdentityName", "Publisher", "PublisherDisplayName", "DisplayName", "SoloArguments", "AliasSuffix", "Protocol", "CertificateThumbprint") {
        if ($PSBoundParameters.ContainsKey($name)) { throw "-$name can't be changed with -Store" }
    }
    if ($NoContextMenu) { throw "-NoContextMenu can't be used with -Store" }
    $IdentityName = $storeIdentity.IdentityName
    $Publisher = $storeIdentity.Publisher
    $PublisherDisplayName = $storeIdentity.PublisherDisplayName
    $DisplayName = $storeIdentity.DisplayName
}

$files = @("solo.exe", "solo-cli.exe", "solo-agent-hook.exe", "solo-terminal-host.exe", "solo-update-helper.exe")
$optional = @("conpty.dll", "OpenConsole.exe", "LICENSE-GPL", "licenses.md", "THIRD-PARTY-VENDORED.md", "LICENSE-command-signatures.md")
$contextMenuDll = "solo-explorer-command.dll"

# The text and code files Solo offers to open (Explorer's "Open with").
# Windows refuses some types in a package's file associations (.js, .ps1,
# .bat, .cmd, .vb, .reg, .inf, ...), so they aren't here.
$fileTypes = @(
    ".txt", ".md", ".markdown", ".mdx", ".log", ".csv", ".tsv",
    ".json", ".jsonc", ".json5", ".jsonl", ".toml", ".yaml", ".yml", ".ini", ".cfg", ".conf", ".env", ".properties",
    ".xml", ".xsd", ".svg", ".html", ".htm", ".css", ".scss", ".sass", ".less",
    ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".mts", ".cts", ".vue", ".svelte", ".astro",
    ".rs", ".py", ".pyi", ".go", ".c", ".h", ".cc", ".cpp", ".cxx", ".hpp", ".hh", ".hxx", ".mm",
    ".cs", ".fs", ".fsx", ".java", ".kt", ".kts", ".scala", ".groovy", ".gradle", ".swift", ".dart",
    ".rb", ".php", ".lua", ".pl", ".pm", ".r", ".jl", ".ex", ".exs", ".erl", ".hs", ".ml", ".mli", ".clj", ".zig", ".nim",
    ".sql", ".graphql", ".gql", ".proto", ".tf", ".tfvars", ".hcl", ".nix", ".cmake", ".mk", ".dockerfile",
    ".sh", ".bash", ".zsh", ".fish", ".psm1", ".psd1",
    ".diff", ".patch", ".gitignore", ".gitattributes", ".editorconfig"
)

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

foreach ($file in $files) {
    if (-not (Test-Path (Join-Path $SourceDir $file))) { throw "$file isn't in $SourceDir; stage Solo first (script\package-solo.ps1 -Step Stage)" }
}

# A release build only. Solo's exes carry VS_FF_DEBUG when they're built with
# debug assertions (crates\windows_resources), and their build
# ("<version>+<channel>...") in Comments.
foreach ($file in $files) {
    $info = (Get-Item (Join-Path $SourceDir $file)).VersionInfo
    if ($info.IsDebug) { throw "$file is a debug build; package a release build (script\package-solo.ps1 -Step Stage)" }
}
$soloInfo = (Get-Item (Join-Path $SourceDir "solo.exe")).VersionInfo
if ($Store -and $soloInfo.Comments -notmatch '^[^+]+\+stable(\.|$)') {
    throw "solo.exe isn't a build of the stable channel (its build is '$($soloInfo.Comments)'); build it with RELEASE_CHANNEL=stable (script\package-solo.ps1 does)"
}

# MSIX versions have four parts, and the Store wants the last one to be 0.
if (-not $Version) {
    $soloVersion = $soloInfo.ProductVersion
    if (-not $soloVersion) {
        $soloVersion = (Select-String -Path "crates\zed\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
    }
    $Version = ConvertTo-MsixVersion $soloVersion -AllowPrerelease:(-not $Store)
}
Assert-MsixVersion $Version -Store:$Store

# The context menu's DLL.
if (-not $NoContextMenu -and -not $ExplorerCommandDll) {
    $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
    $ExplorerCommandDll = @(
        (Join-Path $SourceDir $contextMenuDll),
        (Join-Path $targetDir "solo-dist\explorer_command_injector.dll")
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $ExplorerCommandDll) {
        $build = "cargo build --profile solo-dist -p explorer_command_injector --no-default-features --features solo"
        if ($Store) { throw "The context menu's DLL wasn't found; build it ($build) or pass -ExplorerCommandDll" }
        Write-Warning "The context menu's DLL wasn't found ($build), so the package has no context menu"
        $NoContextMenu = $true
    }
}
if (-not $NoContextMenu -and -not (Test-Path $ExplorerCommandDll)) { throw "$ExplorerCommandDll doesn't exist" }

New-Item -ItemType Directory -Force $OutDir | Out-Null
if (-not $PackageName) { $PackageName = if ($Store) { "SoloCode-$Version-x64" } else { "Solo-$Version-x64" } }
$layout = Join-Path $OutDir "$PackageName-layout"
$msix = Join-Path $OutDir "$PackageName.msix"
if (Test-Path $layout) { Remove-Item -Recurse -Force $layout }
New-Item -ItemType Directory -Force (Join-Path $layout "Assets") | Out-Null

foreach ($file in $files + $optional) {
    $path = Join-Path $SourceDir $file
    if (Test-Path $path) { Copy-Item $path $layout }
}
if (-not $NoContextMenu) { Copy-Item $ExplorerCommandDll (Join-Path $layout $contextMenuDll) }

# The tiles, scaled from Solo's icon. The targetsize ones (unplated) are the
# icons the taskbar, Start's list and Explorer show at those sizes.
Add-Type -AssemblyName System.Drawing
$icon = [System.Drawing.Image]::FromFile((Resolve-Path "crates\zed\resources\app-icon@2x.png").Path)
try {
    $tiles = @(@("Square44x44Logo.png", 44), @("Square150x150Logo.png", 150), @("StoreLogo.png", 50))
    foreach ($size in 16, 24, 32, 48, 256) {
        $tiles += , @("Square44x44Logo.targetsize-$($size)_altform-unplated.png", $size)
    }
    foreach ($tile in $tiles) {
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
# What solo:// links and opened files start solo.exe with.
$targetParameters = (($SoloArguments, '"%1"') | Where-Object { $_ }) -join " "

$fileTypeElements = ($fileTypes | ForEach-Object { "              <uap:FileType>$_</uap:FileType>" }) -join "`r`n"

$contextMenu = ""
if (-not $NoContextMenu) {
    $clsid = if ($Store) { $storeContextMenuClsid } else { $testContextMenuClsid }
    $verbs = ("Directory", "Directory\Background", "*" | ForEach-Object {
        @"
            <desktop5:ItemType Type="$_">
              <desktop5:Verb Id="OpenWithSolo" Clsid="$clsid" />
            </desktop5:ItemType>
"@
    }) -join "`r`n"
    $contextMenu = @"
        <!-- "Open with Solo Code" in Explorer's context menu (Windows 11's own menu, not only "Show more options"). -->
        <desktop4:Extension Category="windows.fileExplorerContextMenus">
          <desktop4:FileExplorerContextMenus>
$verbs
          </desktop4:FileExplorerContextMenus>
        </desktop4:Extension>
        <com:Extension Category="windows.comServer">
          <com:ComServer>
            <com:SurrogateServer DisplayName="$(ConvertTo-XmlAttribute $DisplayName)">
              <com:Class Id="$clsid" Path="$contextMenuDll" ThreadingModel="STA" />
            </com:SurrogateServer>
          </com:ComServer>
        </com:Extension>
"@
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
    Replace("@PROTOCOL_PARAMETERS@", (ConvertTo-XmlAttribute $targetParameters)).
    Replace("@FILE_PARAMETERS@", (ConvertTo-XmlAttribute $targetParameters)).
    Replace("@SOLO_PARAMETERS@", $soloParameters).
    Replace("<!--@FILE_TYPES@-->", $fileTypeElements).
    Replace("<!--@CONTEXT_MENU@-->", $contextMenu)
if ($manifest -match '@[A-Z_]+@') { throw "AppxManifest.xml.in has a placeholder this script doesn't fill: $($Matches[0])" }
$manifestXml = [xml]$manifest
# The Store rejects an application without a Start menu entry ("headless")
# unless Microsoft granted a waiver.
if ($manifestXml.SelectNodes("//*[@AppListEntry='none']").Count) { throw "The manifest has an application with AppListEntry=`"none`", which the Store rejects" }
# Nor did it grant Solo Code the restricted capability unvirtualizedResources.
if ($manifestXml.SelectNodes("//*[local-name()='Capability'][@Name='unvirtualizedResources']").Count) { throw "The manifest declares unvirtualizedResources, which the Store refused" }
$manifestPath = Join-Path (Resolve-Path $layout).Path "AppxManifest.xml"
[System.IO.File]::WriteAllText($manifestPath, $manifest, (New-Object System.Text.UTF8Encoding $false))

# resources.pri, so the Start menu and the Store find the tiles.
$makepri = Find-SdkTool "makepri.exe"
$priConfig = Join-Path $OutDir "$PackageName-priconfig.xml"
& $makepri createconfig /cf $priConfig /dq en-US /pv 10.0.0 /o | Out-Null
if ($LASTEXITCODE -ne 0) { throw "makepri createconfig failed" }
& $makepri new /pr $layout /cf $priConfig /mn $manifestPath /of (Join-Path $layout "resources.pri") /o | Out-Null
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

# What goes up to Partner Center: a .msixupload is a zip of the package and,
# optionally, its symbols (a .appxsym, itself a zip of .pdb files).
if ($Store) {
    $upload = Join-Path $OutDir "$PackageName.msixupload"
    $uploadDir = Join-Path $OutDir "$PackageName-upload"
    if (Test-Path $uploadDir) { Remove-Item -Recurse -Force $uploadDir }
    New-Item -ItemType Directory -Force $uploadDir | Out-Null
    Copy-Item $msix $uploadDir
    if ($SymbolsDir) {
        $pdbs = @("solo.pdb", "cli.pdb", "solo_agent_hook.pdb", "solo_terminal_host.pdb", "solo_update_helper.pdb") |
            ForEach-Object { Join-Path $SymbolsDir $_ } | Where-Object { Test-Path $_ }
        if ($pdbs) {
            $symbolsZip = Join-Path $uploadDir "$PackageName.zip"
            Compress-Archive -Path $pdbs -DestinationPath $symbolsZip
            Rename-Item $symbolsZip "$PackageName.appxsym"
        } else {
            Write-Warning "No .pdb files of Solo's exes in $SymbolsDir; the upload has no symbols"
        }
    }
    $uploadZip = Join-Path $OutDir "$PackageName.msixupload.zip"
    foreach ($old in $upload, $uploadZip) { if (Test-Path $old) { Remove-Item -Force $old } }
    Compress-Archive -Path (Join-Path $uploadDir "*") -DestinationPath $uploadZip
    Move-Item $uploadZip $upload
    Remove-Item -Recurse -Force $uploadDir
    Write-Host "Packaged $upload (to upload in Partner Center)"
}
