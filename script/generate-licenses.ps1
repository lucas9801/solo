# Writes the licenses of Solo's themes, icons and Rust dependencies to
# assets/licenses.md (or the path given as the first argument), which is
# embedded in the app (Help > Open Licenses) when it's built afterwards.
#
# SOLO: runs in Windows PowerShell 5.1 as well as PowerShell 7, writes UTF-8
# without a byte order mark, and keeps the theme and icon licenses (cargo
# about's -o used to overwrite them).

$ErrorActionPreference = 'Stop'

$CARGO_ABOUT_VERSION = "0.8.2"
$outputFile = if ($args[0]) { $args[0] } else { "$(Get-Location)/assets/licenses.md" }
$templateFile = "script/licenses/template.md.hbs"
$utf8 = New-Object System.Text.UTF8Encoding $false

$needsInstall = $true
try {
    $versionOutput = cargo about --version 2>$null
    if ($versionOutput -match "cargo-about $CARGO_ABOUT_VERSION") {
        $needsInstall = $false
        Write-Host "cargo-about@$CARGO_ABOUT_VERSION is already installed"
    }
} catch {
}

if ($needsInstall) {
    Write-Host "Installing cargo-about@$CARGO_ABOUT_VERSION..."
    cargo install --locked "cargo-about@$CARGO_ABOUT_VERSION"
    if ($LASTEXITCODE -ne 0) { throw "Installing cargo-about failed" }
}

Write-Host "Generating cargo licenses"

$codeLicenses = [IO.Path]::GetTempFileName()
try {
    $aboutArgs = @('about', 'generate')
    if ($env:ALLOW_MISSING_LICENSES) { $aboutArgs += '--fail' }
    $aboutArgs += @('-c', 'script/licenses/zed-licenses.toml', $templateFile, '-o', $codeLicenses)
    cargo @aboutArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo about failed" }

    Write-Host "Applying replacements"
    $code = [IO.File]::ReadAllText($codeLicenses, $utf8)
    $replacements = @{
        '&quot;' = '"'
        '&#x27;' = "'"
        '&#x3D;' = '='
        '&#x60;' = '`'
        '&lt;'   = '<'
        '&gt;'   = '>'
    }
    foreach ($find in $replacements.Keys) {
        $code = $code.Replace($find, $replacements[$find])
    }

    $root = Split-Path -Parent $PSScriptRoot
    $themes = [IO.File]::ReadAllText((Join-Path $root "assets/themes/LICENSES"), $utf8)
    $icons = [IO.File]::ReadAllText((Join-Path $root "assets/icons/LICENSES"), $utf8)
    # cargo about skips crates that aren't published, like the vendored Warp
    # command signatures (MIT), so their license is added here.
    $signatures = [IO.File]::ReadAllText((Join-Path $root "vendor/warp_command_signatures/LICENSE.md"), $utf8)
    $content = "# ###### THEME LICENSES ######`n`n$themes`n# ###### ICON LICENSES ######`n`n$icons`n# ###### CODE LICENSES ######`n`n$code`n# ###### VENDORED CODE LICENSES ######`n`n#### warp-command-signatures, warp-completion-metadata (MIT License)`n`n$signatures"

    $directory = Split-Path -Parent $outputFile
    if ($directory) { New-Item -ItemType Directory -Force $directory | Out-Null }
    [IO.File]::WriteAllText($outputFile, $content, $utf8)
} finally {
    Remove-Item -Force $codeLicenses -ErrorAction SilentlyContinue
}

Write-Host "generate-licenses completed. See $outputFile"
