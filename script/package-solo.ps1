# Builds Solo for Windows and packages it as a portable zip with an install
# script.
#
#   script\package-solo.ps1 [-TargetDir <cargo target dir>] [-OutDir dist]
#
# The zip holds Solo\solo.exe with the ConPTY runtime, solo-agent-hook.exe
# (which Claude Code runs to report its status) and solo-update-helper.exe
# (which installs updates once Solo has exited) next to it, the licenses, and
# install.ps1, which copies Solo to %LOCALAPPDATA%\Programs\Solo and adds
# Start menu and desktop shortcuts. The licenses of Solo's dependencies are
# generated first (script\generate-licenses.ps1, which installs cargo-about if
# needed), embedded in solo.exe (Help > Open Licenses) and put next to it as
# licenses.md. With Inno Setup 6 installed it also builds
# Solo-Setup-<version>.exe from script\solo.iss (-Iscc to give ISCC.exe's path).
#
# Signing (optional): with -SignCommand, or the SOLO_SIGN_COMMAND environment
# variable, set to a command line that signs the file given as $f, e.g.
#
#   signtool.exe sign /fd sha256 /tr http://timestamp.example /td sha256 /a $f
#   powershell.exe -ExecutionPolicy Bypass -File C:\path\sign.ps1 $f
#
# solo.exe, solo-agent-hook.exe and solo-update-helper.exe are signed before
# they're zipped, and Inno Setup signs the installer and its uninstaller with
# the same command. It uses Inno Setup's sign tool syntax: $f is the quoted
# path of the file, $q a double quote (use it instead of quotes, which Windows
# PowerShell doesn't pass on reliably) and $$ a dollar sign. Without it nothing
# is signed and packaging works as before.

param(
    [string]$TargetDir = "",
    [string]$OutDir = "dist",
    [string]$Iscc = "",
    [string]$SignCommand = $env:SOLO_SIGN_COMMAND
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ($TargetDir) { $env:CARGO_TARGET_DIR = $TargetDir } else { $TargetDir = Join-Path $root "target" }
$env:RELEASE_CHANNEL = "stable"
$env:CARGO_INCREMENTAL = "0"

# Before building: solo.exe embeds assets\licenses.md when it's compiled.
& (Join-Path $PSScriptRoot "generate-licenses.ps1") (Join-Path $root "assets\licenses.md")
if (-not (Test-Path (Join-Path $root "assets\licenses.md"))) { throw "Generating the licenses failed" }

cargo build --profile solo-dist -p zed -p solo_agent_hook -p solo_update_helper -p cli --bin solo --bin solo-agent-hook --bin solo-update-helper --bin cli
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$built = Join-Path $TargetDir "solo-dist"
$version = (Select-String -Path "crates\zed\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$stage = Join-Path $OutDir "Solo"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item (Join-Path $built "solo.exe") $stage
Copy-Item (Join-Path $built "solo-agent-hook.exe") $stage
Copy-Item (Join-Path $built "solo-update-helper.exe") $stage
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
Copy-Item "script\install-solo.ps1" (Join-Path $stage "install.ps1")

# Runs $SignCommand on a file the way Inno Setup runs a sign tool.
function Invoke-Sign([string]$file) {
    $line = $SignCommand.Replace('$$', "`0").Replace('$q', '"').Replace('$f', "`"$file`"").Replace("`0", '$')
    Write-Host "Signing $file"
    # Through ProcessStartInfo, so the command line reaches cmd.exe unchanged.
    $info = New-Object System.Diagnostics.ProcessStartInfo "cmd.exe", "/d /s /c `"$line`""
    $info.UseShellExecute = $false
    $process = [System.Diagnostics.Process]::Start($info)
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "Signing $file failed (exit code $($process.ExitCode))" }
}
if ($SignCommand) {
    foreach ($exe in "solo.exe", "solo-agent-hook.exe", "solo-update-helper.exe", "solo-cli.exe") {
        Invoke-Sign (Resolve-Path (Join-Path $stage $exe)).Path
    }
}

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
$isccArgs = @("/Q", "/DAppVersion=$version", "/DSourceDir=$((Resolve-Path $stage).Path)", "/DOutputDir=$((Resolve-Path $OutDir).Path)")
# solo.iss signs the installer and uninstaller with the sign tool SoloSign
# when SignInstaller is defined.
if ($SignCommand) { $isccArgs += @("/DSignInstaller", "/sSoloSign=$SignCommand") }
& $Iscc @isccArgs "script\solo.iss"
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
Write-Host "Packaged $(Join-Path $OutDir "Solo-Setup-$version.exe")"
