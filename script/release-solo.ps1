# Releases a new version of Solo on GitHub: sets the version, commits and tags
# it, builds the installer and zip with package-solo.ps1, pushes, and creates a
# GitHub release with them, which running copies of Solo update from.
#
#   script\release-solo.ps1 [-Version <x.y.z>] [-Notes <text>] [-Draft] [-PublishOnly]
#
# Without -Version the patch version goes up by one. -PublishOnly only creates
# the GitHub release of the current version, already built, tagged and pushed
# (when creating it failed). Needs a clean working tree
# on the branch that tracks the repository's main branch, Inno Setup 6, and the
# GitHub CLI (gh), signed in with `gh auth login`.

param(
    [string]$Version = "",
    [string]$Notes = "",
    [switch]$Draft,
    [switch]$PublishOnly,
    [string]$Iscc = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$gh = (Get-Command gh.exe -ErrorAction SilentlyContinue).Source
if (-not $gh) {
    $gh = @(
        "$env:LOCALAPPDATA\Programs\GitHub CLI\gh.exe",
        "$env:ProgramFiles\GitHub CLI\gh.exe",
        "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\GitHub.cli_Microsoft.Winget.Source_8wekyb3d8bbwe\bin\gh.exe"
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $gh) { throw "GitHub CLI not found. Install it (winget install GitHub.cli) and run gh auth login." }
& $gh auth status *> $null
if ($LASTEXITCODE -ne 0) { throw "GitHub CLI isn't signed in. Run: gh auth login" }

# The release goes to the repository Solo is pushed to, not to Zed's (upstream).
$repo = (git remote get-url origin) -replace '^.*github\.com[:/]', '' -replace '\.git$', ''

$cargoToml = "crates\zed\Cargo.toml"
$current = (Select-String -Path $cargoToml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if ($PublishOnly) {
    $Version = $current
    $tag = "v$Version"
    if (-not (git tag --list $tag)) { throw "Tag $tag doesn't exist; release without -PublishOnly" }
    $previousTag = git tag --merged "$tag^" --list "v*" --sort=-v:refname | Select-Object -First 1
    $setup = "dist\Solo-Setup-$Version.exe"
    $zip = "dist\Solo-$Version-windows-x64.zip"
    foreach ($file in $setup, $zip) { if (-not (Test-Path $file)) { throw "$file isn't built" } }
} else {
if (git status --porcelain --untracked-files=no) { throw "Commit or stash your changes first." }
if (-not $Version) {
    $parts = $current.Split(".")
    $Version = "$($parts[0]).$($parts[1]).$([int]$parts[2] + 1)"
}
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3, not $Version" }
if ([version]$Version -le [version]$current) { throw "Version $Version isn't newer than $current" }
$tag = "v$Version"
if (git tag --list $tag) { throw "Tag $tag already exists" }
# The latest release before this one in Solo's history (Zed's tags aren't in it).
$previousTag = git tag --merged HEAD --list "v*" --sort=-v:refname | Select-Object -First 1

Write-Host "Releasing Solo $Version (was $current)"

# The version of the zed crate is the app's version, here and in Cargo.lock.
function Set-Version([string]$path, [string]$pattern, [string]$replacement) {
    $text = [IO.File]::ReadAllText((Resolve-Path $path))
    $updated = [regex]::Replace($text, $pattern, $replacement, [Text.RegularExpressions.RegexOptions]::Multiline)
    if ($updated -eq $text) { throw "Couldn't set the version in $path" }
    [IO.File]::WriteAllText((Resolve-Path $path), $updated)
}
Set-Version $cargoToml '^version = ".+"' "version = `"$Version`""
Set-Version "Cargo.lock" '(?<=^name = "zed"\r?\nversion = ")[^"]+' $Version

& (Join-Path $PSScriptRoot "package-solo.ps1") -Iscc $Iscc
if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { throw "Packaging failed" }
$setup = "dist\Solo-Setup-$Version.exe"
$zip = "dist\Solo-$Version-windows-x64.zip"
foreach ($file in $setup, $zip) { if (-not (Test-Path $file)) { throw "$file wasn't built" } }

git commit -q -m "Solo $Version" -- $cargoToml Cargo.lock
if ($LASTEXITCODE -ne 0) { throw "git commit failed" }
git tag -a $tag -m "Solo $Version"
git push
if ($LASTEXITCODE -ne 0) { throw "git push failed" }
git push origin $tag
if ($LASTEXITCODE -ne 0) { throw "Pushing the tag failed" }
}

if (-not $Notes) {
    $range = if ($previousTag) { "$previousTag..$tag" } else { $tag }
    $Notes = (git log --no-merges --format="- %s" $range | Where-Object { $_ -notmatch "^- Solo \d" }) -join "`n"
}
$notesFile = New-TemporaryFile
[IO.File]::WriteAllText($notesFile, $Notes)
$releaseArgs = @("release", "create", $tag, $setup, $zip, "--repo", $repo, "--verify-tag", "--title", "Solo $Version", "--notes-file", $notesFile)
if ($Draft) { $releaseArgs += "--draft" }
& $gh @releaseArgs
$created = $LASTEXITCODE
Remove-Item $notesFile
if ($created -ne 0) { throw "Creating the GitHub release failed; the tag $tag is pushed, so rerun with -PublishOnly" }
Write-Host "Released Solo $Version"
