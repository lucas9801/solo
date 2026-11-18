# Builds Solo for Windows and stages it for the Microsoft Store package.
#
#   script\package-solo.ps1 [-TargetDir <cargo target dir>] [-OutDir dist] [-Step Stage]
#
# Builds the solo-dist profile and puts in <OutDir>\Solo: solo.exe with the
# ConPTY runtime, solo-agent-hook.exe (which Claude Code runs to report its
# status), solo-update-helper.exe, solo-terminal-host.exe, solo-cli.exe and
# the licenses. The licenses of Solo's dependencies are generated first
# (script\generate-licenses.ps1, which installs cargo-about if needed),
# embedded in solo.exe (Help > Open Licenses) and put next to it as
# licenses.md. script\package-solo-msix.ps1 makes the Store package from
# that folder; releases have no other Windows package (no installer, no zip).
#
# -Step Stage is what it always does (kept for the callers that pass it).

param(
    [string]$TargetDir = "",
    [string]$OutDir = "dist",
    [ValidateSet("Stage")]
    [string]$Step = "Stage"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$stage = Join-Path $OutDir "Solo"

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
# The ConPTY runtime (MIT, signed by Microsoft), which crates\zed\build.rs
# downloads into the target folder. Without it terminals fall back to
# Windows' older ConPTY, so the stage can't go without it. The build script
# reruns when the commit changes; to rerun it after a failed download, touch
# crates\zed\build.rs.
foreach ($runtime in "conpty.dll", "OpenConsole.exe") {
    $path = Join-Path $built $runtime
    if (-not (Test-Path $path)) { throw "$runtime isn't in ${built}: the ConPTY package wasn't downloaded (see the zed build script's warning in cargo's output)" }
    Copy-Item $path $stage
}
Copy-Item "LICENSE-GPL" $stage -ErrorAction SilentlyContinue
Copy-Item "assets\licenses.md" $stage
Copy-Item "vendor\README.md" (Join-Path $stage "THIRD-PARTY-VENDORED.md")
Copy-Item "vendor\warp_command_signatures\LICENSE.md" (Join-Path $stage "LICENSE-command-signatures.md")

Write-Host "Staged $stage"
