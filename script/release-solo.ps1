# Starts a release of Solo: sets the version, saves the release notes as
# docs\release-notes\<version>.md, commits both, tags the commit v<version>
# and pushes the branch and the tag. Nothing is built here: the tag starts the
# "Solo package" workflow (.github/workflows/solo-package.yml), which builds
# Solo on GitHub's runners, checks solo.exe was built from the tagged commit,
# has it signed (once SignPath signing is turned on, see
# docs\code-signing-policy.md), packages the installer and the zip, and creates
# a draft GitHub release with them and the notes. Check the draft, then publish
# it (on GitHub, or `gh release edit v<version> --draft=false`); running
# copies of Solo update only from published releases.
#
#   script\release-solo.ps1 (-Notes <text> | -NotesFile <file>) [-Version <x.y.z>] [-DryRun]
#
# The release notes must start with a one-sentence summary in Chinese on the
# first line and in English on the second, plain text: the update
# notification shows the one in Solo's language
# (script\check-solo-release-notes.ps1 checks them before anything else).
#
# Without -Version the patch version goes up by one. -DryRun checks
# everything and shows what would happen without changing, committing,
# tagging or pushing anything. If committing, tagging or pushing the branch
# fails, the version commit and the tag are undone. Needs a clean working
# tree on a branch that tracks the repository's main branch.
#
# To build packages locally (for testing only; releases come from CI), run
# script\package-solo.ps1.

param(
    [string]$Version = "",
    [string]$Notes = "",
    [string]$NotesFile = "",
    [switch]$DryRun
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# The notes first, so a release doesn't stop on them later.
if ($NotesFile) { $Notes = [IO.File]::ReadAllText((Resolve-Path $NotesFile)) }
if (-not $Notes) { throw "Give the release notes with -Notes or -NotesFile." }
& (Join-Path $PSScriptRoot "check-solo-release-notes.ps1") -Notes $Notes

# The release goes to the repository Solo is pushed to, not to Zed's (upstream).
$repo = (git remote get-url origin) -replace '^.*github\.com[:/]', '' -replace '\.git$', ''
# (Not rev-parse @{u}: Windows PowerShell stops on a native command's stderr.)
$upstreamBranch = git for-each-ref --format="%(upstream:short)" (git symbolic-ref -q HEAD)
if (-not $upstreamBranch) { throw "The current branch doesn't track a branch of origin; check out the one that tracks main." }

$cargoToml = "crates\zed\Cargo.toml"
$current = (Select-String -Path $cargoToml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value

if (git status --porcelain --untracked-files=no) { throw "Commit or stash your changes first." }
if (-not $Version) {
    $parts = $current.Split(".")
    $Version = "$($parts[0]).$($parts[1]).$([int]$parts[2] + 1)"
}
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3, not $Version" }
if ([version]$Version -le [version]$current) { throw "Version $Version isn't newer than $current" }
$tag = "v$Version"
if (git tag --list $tag) { throw "Tag $tag already exists" }
$remoteTag = git ls-remote --tags origin "refs/tags/$tag"
if ($LASTEXITCODE -ne 0) { throw "Couldn't reach origin to check for tag $tag" }
if ($remoteTag) { throw "Tag $tag already exists on origin" }
# The workflow reads the notes from here (and checks them again).
$notesPath = "docs\release-notes\$Version.md"
if (Test-Path $notesPath) { throw "$notesPath already exists" }

# The version of the zed crate is the app's version, here and in Cargo.lock.
function Get-WithVersion([string]$path, [string]$pattern, [string]$replacement) {
    $text = [IO.File]::ReadAllText((Resolve-Path $path))
    $updated = [regex]::Replace($text, $pattern, $replacement, [Text.RegularExpressions.RegexOptions]::Multiline)
    if ($updated -eq $text) { throw "Couldn't set the version in $path" }
    return $updated
}
$newCargoToml = Get-WithVersion $cargoToml '^version = ".+"' "version = `"$Version`""
$newCargoLock = Get-WithVersion "Cargo.lock" '(?<=^name = "zed"\r?\nversion = ")[^"]+' $Version
# Stored with LF line endings, like the rest of the repository.
$notesText = ($Notes -replace "\r\n", "`n").TrimEnd() + "`n"

if ($DryRun) {
    Write-Host "Dry run: nothing is changed, committed, tagged or pushed."
    Write-Host "Would release Solo $Version (was $current) from $(git rev-parse --short HEAD), pushing to $upstreamBranch"
    Write-Host "Would set the version in $cargoToml and Cargo.lock and write $notesPath`:"
    Write-Host $notesText
    Write-Host "Would commit `"Solo $Version`", tag it $tag and push the branch and $tag to $repo,"
    Write-Host "which starts https://github.com/$repo/actions/workflows/solo-package.yml"
    return
}

Write-Host "Releasing Solo $Version (was $current)"

# Undo the commit and the tag unless the branch gets pushed.
$before = git rev-parse HEAD
$pushed = $false
$utf8 = New-Object System.Text.UTF8Encoding $false
try {
    [IO.File]::WriteAllText((Join-Path $root $cargoToml), $newCargoToml, $utf8)
    [IO.File]::WriteAllText((Join-Path $root "Cargo.lock"), $newCargoLock, $utf8)
    New-Item -ItemType Directory -Force (Split-Path $notesPath) | Out-Null
    [IO.File]::WriteAllText((Join-Path $root $notesPath), $notesText, $utf8)
    git add -- $notesPath
    if ($LASTEXITCODE -ne 0) { throw "git add failed" }
    git commit -q -m "Solo $Version" -- $cargoToml Cargo.lock $notesPath
    if ($LASTEXITCODE -ne 0) { throw "git commit failed" }
    $commit = git rev-parse HEAD

    git tag -a $tag -m "Solo $Version" $commit
    if ($LASTEXITCODE -ne 0) { throw "git tag failed" }
    if ((git rev-parse "$tag^{commit}") -ne $commit) { throw "Tag $tag isn't on $commit" }

    git push
    if ($LASTEXITCODE -ne 0) { throw "git push failed" }
    $pushed = $true
} finally {
    if (-not $pushed) {
        Write-Warning "Releasing failed: undoing the version commit and tag $tag"
        if (git tag --list $tag) { git tag -d $tag | Out-Null }
        if ((git rev-parse HEAD) -ne $before) {
            # --keep: back to the commit before, keeping any other changes.
            git reset -q --keep $before
        } else {
            git checkout -q -- $cargoToml Cargo.lock
        }
        if (Test-Path $notesPath) {
            git rm -q --cached --ignore-unmatch -- $notesPath | Out-Null
            Remove-Item $notesPath
        }
    }
}

# The branch is public now, so a failure here keeps the commit and the tag.
git push origin $tag
if ($LASTEXITCODE -ne 0) { throw "Pushing the tag failed; the version commit is pushed, so push the tag again: git push origin $tag" }
Write-Host "Pushed $tag. The Solo package workflow builds it and creates a draft release:"
Write-Host "  https://github.com/$repo/actions/workflows/solo-package.yml"
Write-Host "Check the draft at https://github.com/$repo/releases, then publish it."
