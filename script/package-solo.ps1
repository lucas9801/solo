# Builds Solo for Windows and packages it as a portable zip with an install
# script.
#
#   script\package-solo.ps1 [-TargetDir <cargo target dir>] [-OutDir dist]
#
# The zip holds Solo\solo.exe with the ConPTY runtime and solo-agent-hook.exe
# (which Claude Code runs to report its status) next to it, the licenses, and
# install.ps1, which copies Solo to %LOCALAPPDATA%\Programs\Solo and adds
# Start menu and desktop shortcuts. With Inno Setup 6 installed it also builds
# Solo-Setup-<version>.exe from script\solo.iss (-Iscc to give ISCC.exe's path).

param(
    [string]$TargetDir = "",
    [string]$OutDir = "dist",
    [string]$Iscc = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ($TargetDir) { $env:CARGO_TARGET_DIR = $TargetDir } else { $TargetDir = Join-Path $root "target" }
$env:RELEASE_CHANNEL = "stable"
$env:CARGO_INCREMENTAL = "0"

cargo build --profile solo-dist -p zed -p solo_agent_hook --bin solo --bin solo-agent-hook
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$built = Join-Path $TargetDir "solo-dist"
$version = (Select-String -Path "crates\zed\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$stage = Join-Path $OutDir "Solo"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item (Join-Path $built "solo.exe") $stage
Copy-Item (Join-Path $built "solo-agent-hook.exe") $stage
foreach ($runtime in "conpty.dll", "OpenConsole.exe") {
    $path = Join-Path $built $runtime
    if (Test-Path $path) { Copy-Item $path $stage } else { Write-Warning "$runtime not found; terminals fall back to the system ConPTY" }
}
Copy-Item "LICENSE-GPL" $stage -ErrorAction SilentlyContinue
Copy-Item "vendor\README.md" (Join-Path $stage "THIRD-PARTY-VENDORED.md")
Copy-Item "vendor\warp_command_signatures\LICENSE.md" (Join-Path $stage "LICENSE-command-signatures.md")
Copy-Item "script\install-solo.ps1" (Join-Path $stage "install.ps1")

$zip = Join-Path $OutDir "Solo-$version-windows-x64.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip
Write-Host "Packaged $zip"

if (-not $Iscc) {
    $Iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
    if (-not $Iscc) {
        $Iscc = @(
            "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
            "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
            "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
        ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    }
}
if (-not $Iscc -or -not (Test-Path $Iscc)) {
    Write-Warning "Inno Setup 6 not found, so no installer was built. Install it (winget install JRSoftware.InnoSetup) or pass -Iscc <path to ISCC.exe>."
    return
}
& $Iscc /Q "/DAppVersion=$version" "/DSourceDir=$((Resolve-Path $stage).Path)" "/DOutputDir=$((Resolve-Path $OutDir).Path)" "script\solo.iss"
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
Write-Host "Packaged $(Join-Path $OutDir "Solo-Setup-$version.exe")"
