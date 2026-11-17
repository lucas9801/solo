# Builds Solo for Windows and packages it as a portable zip with an install
# script.
#
#   script\package-solo.ps1 [-TargetDir <cargo target dir>] [-OutDir dist] [-Step All|Stage|Package]
#
# The zip holds Solo\solo.exe with the ConPTY runtime, solo-agent-hook.exe
# (which Claude Code runs to report its status) and solo-update-helper.exe
# (which installs updates once Solo has exited) next to it, the licenses, and
# install.ps1, which copies Solo to %LOCALAPPDATA%\Programs\Solo and adds
# Start menu and desktop shortcuts. The licenses of Solo's dependencies are
# generated first (script\generate-licenses.ps1, which installs cargo-about if
# needed), embedded in solo.exe (Help > Open Licenses) and put next to it as
# licenses.md. (Releases no longer have an installer: Solo is installed from
# the Microsoft Store, script\package-solo-msix.ps1.)
#
# Steps: All (the default) does everything. Stage only builds and puts the
# files in <OutDir>\Solo; Package only makes the zip from <OutDir>\Solo as it
# is, so the files can be signed in between (the release workflow,
# .github/workflows/solo-package.yml, has them signed by SignPath).
#
# Signing (optional, local): with -SignCommand, or the SOLO_SIGN_COMMAND
# environment variable, set to a command line that signs the file given as $f,
# e.g.
#
#   signtool.exe sign /fd sha256 /tr http://timestamp.example /td sha256 /a $f
#   powershell.exe -ExecutionPolicy Bypass -File C:\path\sign.ps1 $f
#
# Solo's own exes are signed before they're zipped. In the command $f is the
# quoted path of the file, $q a double quote (use it instead of quotes, which
# Windows PowerShell doesn't pass on reliably) and $$ a dollar sign. Without
# it nothing is signed and packaging works as before.

param(
    [string]$TargetDir = "",
    [string]$OutDir = "dist",
    [string]$SignCommand = $env:SOLO_SIGN_COMMAND,
    [ValidateSet("All", "Stage", "Package")]
    [string]$Step = "All"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# The exes built from Solo's source, which are signed; conpty.dll and
# OpenConsole.exe come signed by Microsoft (crates/zed/build.rs).
$ownExes = @("solo.exe", "solo-agent-hook.exe", "solo-update-helper.exe", "solo-terminal-host.exe", "solo-cli.exe")

$version = (Select-String -Path "crates\zed\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$stage = Join-Path $OutDir "Solo"

# Runs $SignCommand on a file ($f, $q and $$ replaced as described above).
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
    Copy-Item "script\install-solo.ps1" (Join-Path $stage "install.ps1")

    if ($SignCommand) {
        foreach ($exe in $ownExes) {
            Invoke-Sign (Resolve-Path (Join-Path $stage $exe)).Path
        }
    }
    Write-Host "Staged $stage"
}

if ($Step -eq "All" -or $Step -eq "Package") {
    foreach ($exe in $ownExes) {
        if (-not (Test-Path (Join-Path $stage $exe))) { throw "$exe isn't staged in $stage; run with -Step Stage first" }
    }
    $zip = Join-Path $OutDir "Solo-$version-windows-x64.zip"
    if (Test-Path $zip) { Remove-Item -Force $zip }
    Compress-Archive -Path $stage -DestinationPath $zip
    Write-Host "Packaged $zip"
}
