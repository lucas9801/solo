# Builds Solo for Windows and stages it for its packages: Solo Code for the
# Microsoft Store and the installer released on GitHub.
#
#   script\package-solo.ps1 [-TargetDir <cargo target dir>] [-OutDir dist] [-Step All|Stage|Installer] [-Iscc <ISCC.exe>]
#
# Builds the solo-dist profile and puts in <OutDir>\Solo: solo.exe with the
# ConPTY runtime, solo-agent-hook.exe (which Claude Code runs to report its
# status), solo-update-helper.exe, solo-terminal-host.exe, solo-cli.exe and
# the licenses. The licenses of Solo's dependencies are generated first
# (script\generate-licenses.ps1, which installs cargo-about if needed),
# embedded in solo.exe (Help > Open Licenses) and put next to it as
# licenses.md. script\package-solo-msix.ps1 makes the Store package from
# that folder.
#
# Steps: Stage (the default) builds and fills <OutDir>\Solo. Installer makes
# <OutDir>\Solo-Setup-<version>.exe from <OutDir>\Solo as it is, with Inno
# Setup 6 (script\solo.iss; -Iscc gives ISCC.exe's path, otherwise it's looked
# for on PATH and in Inno Setup's usual folders), named after the staged
# solo.exe's version. All does both. The installer isn't signed. Releases
# have no zip.

param(
    [string]$TargetDir = "",
    [string]$OutDir = "dist",
    [ValidateSet("All", "Stage", "Installer")]
    [string]$Step = "Stage",
    [string]$Iscc = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$stage = Join-Path $OutDir "Solo"

function Find-Iscc {
    if ($Iscc) { return $Iscc }
    $found = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
    if (-not $found) {
        $found = @(
            "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
            "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
            "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
        ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    }
    return $found
}

if ($Step -eq "All" -or $Step -eq "Stage") {
    if ($TargetDir) { $env:CARGO_TARGET_DIR = $TargetDir } else { $TargetDir = Join-Path $root "target" }
    $env:RELEASE_CHANNEL = "stable"
    $env:CARGO_INCREMENTAL = "0"

    # Before building: solo.exe embeds assets\licenses.md when it's compiled.
    & (Join-Path $PSScriptRoot "generate-licenses.ps1") (Join-Path $root "assets\licenses.md")
    if (-not (Test-Path (Join-Path $root "assets\licenses.md"))) { throw "Generating the licenses failed" }

    cargo build --profile solo-dist -p zed -p solo_agent_hook -p solo_update_helper -p solo_terminal_host -p cli --bin solo --bin solo-agent-hook --bin solo-update-helper --bin solo-terminal-host --bin cli
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    $built = Join-Path $TargetDir "solo-dist"
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force $stage | Out-Null

    Copy-Item (Join-Path $built "solo.exe") $stage
    Copy-Item (Join-Path $built "solo-agent-hook.exe") $stage
    Copy-Item (Join-Path $built "solo-update-helper.exe") $stage
    # Holds terminals' consoles, so they can outlive Solo (terminal.keep_running_on_quit).
    Copy-Item (Join-Path $built "solo-terminal-host.exe") $stage
    # The command line, which agents in Solo's terminals use (solo-cli agent ...).
    Copy-Item (Join-Path $built "cli.exe") (Join-Path $stage "solo-cli.exe")
    foreach ($runtime in "conpty.dll", "OpenConsole.exe") {
        $path = Join-Path $built $runtime
        if (Test-Path $path) { Copy-Item $path $stage } else { Write-Warning "$runtime not found; terminals fall back to the system ConPTY" }
    }
    Copy-Item "LICENSE-GPL" $stage -ErrorAction SilentlyContinue
    Copy-Item "assets\licenses.md" $stage
    Copy-Item "vendor\README.md" (Join-Path $stage "THIRD-PARTY-VENDORED.md")
    Copy-Item "vendor\warp_command_signatures\LICENSE.md" (Join-Path $stage "LICENSE-command-signatures.md")

    Write-Host "Staged $stage"
}

if ($Step -eq "All" -or $Step -eq "Installer") {
    foreach ($exe in "solo.exe", "solo-agent-hook.exe", "solo-update-helper.exe", "solo-terminal-host.exe", "solo-cli.exe") {
        if (-not (Test-Path (Join-Path $stage $exe))) { throw "$exe isn't staged in $stage; run with -Step Stage first" }
    }
    # The version of the staged solo.exe, which may be another version's
    # build than this checkout's (a release's built files, say).
    $version = (Get-Item (Join-Path $stage "solo.exe")).VersionInfo.ProductVersion
    if (-not $version) { throw "The staged solo.exe has no product version" }
    $Iscc = Find-Iscc
    if (-not $Iscc -or -not (Test-Path $Iscc)) { throw "Inno Setup 6 not found; install it (winget install JRSoftware.InnoSetup) or pass -Iscc <path to ISCC.exe>" }
    New-Item -ItemType Directory -Force $OutDir | Out-Null
    & $Iscc /Q "/DAppVersion=$version" "/DSourceDir=$((Resolve-Path $stage).Path)" "/DOutputDir=$((Resolve-Path $OutDir).Path)" "script\solo.iss"
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed (exit code $LASTEXITCODE)" }
    Write-Host "Packaged $(Join-Path $OutDir "Solo-Setup-$version.exe")"
}
