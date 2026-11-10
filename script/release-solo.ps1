# Releases a new version of Solo on GitHub: sets the version and commits it,
# builds the installer and zip with package-solo.ps1 from that commit, checks
# that solo.exe has the commit in it, tags it, pushes, and creates a GitHub
# release with them, which running copies of Solo update from.
#
#   script\release-solo.ps1 (-Notes <text> | -NotesFile <file>) [-Version <x.y.z>] [-Draft] [-PublishOnly]
#
# The release notes must start with a one-sentence summary in Chinese on the
# first line and in English on the second, plain text: the update
# notification shows the one in Solo's language
# (script\check-solo-release-notes.ps1 checks them before anything else).
#
# Without -Version the patch version goes up by one. If building or the check
# of solo.exe fails (or is interrupted), the version commit and the tag are
# undone. -PublishOnly only creates the GitHub release of the current version,
# already built, tagged and pushed (when creating it failed). Needs a clean
# working tree on the branch that tracks the repository's main branch, Inno
# Setup 6, and the GitHub CLI (gh), signed in with `gh auth login`.

param(
    [string]$Version = "",
    [string]$Notes = "",
    [string]$NotesFile = "",
    [switch]$Draft,
    [switch]$PublishOnly,
    [string]$Iscc = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# The notes first, so a release doesn't stop on them after building.
if ($NotesFile) { $Notes = [IO.File]::ReadAllText((Resolve-Path $NotesFile)) }
if (-not $Notes) { throw "Give the release notes with -Notes or -NotesFile." }
& (Join-Path $PSScriptRoot "check-solo-release-notes.ps1") -Notes $Notes

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

# Throws unless the packaged solo.exe was built from $commit: its product
# version is "<version>+<channel>.<commit>" (crates/windows_resources).
function Assert-BuiltFrom([string]$commit) {
    $exe = "dist\Solo\solo.exe"
    if (-not (Test-Path $exe)) { throw "$exe isn't built" }
    $productVersion = (Get-Item $exe).VersionInfo.ProductVersion
    if (-not $productVersion -or -not $productVersion.EndsWith(".$commit")) {
        throw "$exe was built from another commit (product version $productVersion), not $commit"
    }
    Write-Host "$exe was built from $commit"
}

if ($PublishOnly) {
    $Version = $current
    $tag = "v$Version"
    if (-not (git tag --list $tag)) { throw "Tag $tag doesn't exist; release without -PublishOnly" }
    $setup = "dist\Solo-Setup-$Version.exe"
    $zip = "dist\Solo-$Version-windows-x64.zip"
    foreach ($file in $setup, $zip) { if (-not (Test-Path $file)) { throw "$file isn't built" } }
    Assert-BuiltFrom (git rev-parse "$tag^{commit}")
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

Write-Host "Releasing Solo $Version (was $current)"

# The version of the zed crate is the app's version, here and in Cargo.lock.
function Set-Version([string]$path, [string]$pattern, [string]$replacement) {
    $text = [IO.File]::ReadAllText((Resolve-Path $path))
    $updated = [regex]::Replace($text, $pattern, $replacement, [Text.RegularExpressions.RegexOptions]::Multiline)
    if ($updated -eq $text) { throw "Couldn't set the version in $path" }
    [IO.File]::WriteAllText((Resolve-Path $path), $updated)
}

# Commit the version before building, so solo.exe carries the commit the
# tag is on (crates/zed/build.rs takes it from ZED_COMMIT_SHA); undo it all
# unless the build is checked and tagged.
$before = git rev-parse HEAD
$tagged = $false
try {
    Set-Version $cargoToml '^version = ".+"' "version = `"$Version`""
    Set-Version "Cargo.lock" '(?<=^name = "zed"\r?\nversion = ")[^"]+' $Version
    git commit -q -m "Solo $Version" -- $cargoToml Cargo.lock
    if ($LASTEXITCODE -ne 0) { throw "git commit failed" }
    $commit = git rev-parse HEAD
    $env:ZED_COMMIT_SHA = $commit

    & (Join-Path $PSScriptRoot "package-solo.ps1") -Iscc $Iscc
    if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { throw "Packaging failed" }
    $setup = "dist\Solo-Setup-$Version.exe"
    $zip = "dist\Solo-$Version-windows-x64.zip"
    foreach ($file in $setup, $zip) { if (-not (Test-Path $file)) { throw "$file wasn't built" } }
    Assert-BuiltFrom $commit

    git tag -a $tag -m "Solo $Version" $commit
    if ($LASTEXITCODE -ne 0) { throw "git tag failed" }
    if ((git rev-parse "$tag^{commit}") -ne $commit) { throw "Tag $tag isn't on $commit" }
    $tagged = $true
} finally {
    Remove-Item Env:\ZED_COMMIT_SHA -ErrorAction SilentlyContinue
    if (-not $tagged) {
        Write-Warning "Releasing failed: undoing the version commit and tag $tag"
        if (git tag --list $tag) { git tag -d $tag | Out-Null }
        if ((git rev-parse HEAD) -ne $before) {
            # --keep: back to the commit before, keeping any other changes.
            git reset -q --keep $before
        } else {
            git checkout -q -- $cargoToml Cargo.lock
        }
    }
}

git push
if ($LASTEXITCODE -ne 0) { throw "git push failed" }
git push origin $tag
if ($LASTEXITCODE -ne 0) { throw "Pushing the tag failed" }
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
