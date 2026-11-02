# Builds Solo for Windows and packages it as a portable zip with an install
# script.
#
#   script\package-solo.ps1 [-TargetDir <cargo target dir>] [-OutDir dist]
#
# The zip holds Solo\solo.exe with the ConPTY runtime and solo-agent-hook.exe
# (which Claude Code runs to report its status) next to it, the licenses, and
# install.ps1, which copies Solo to %LOCALAPPDATA%\Programs\Solo and adds
# Start menu and desktop shortcuts.

param(
    [string]$TargetDir = "",
    [string]$OutDir = "dist"
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
