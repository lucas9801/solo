# Starts a release of Solo: sets the version, saves the release notes as
# docs\release-notes\<version>.md, commits both, tags the commit v<version>
# and pushes the branch and the tag. Nothing is built here: the tag starts the
# "Solo package" workflow (.github/workflows/solo-package.yml), which builds
# Solo on GitHub's runners, checks solo.exe was built from the tagged commit,
# makes the Microsoft Store package, and creates a draft GitHub release with
# the remote servers and the notes. Check the draft, then publish
# it (on GitHub, or `gh release edit v<version> --draft=false`); running
# copies of Solo update only from published releases. Publishing it also
# submits the Microsoft Store package (Solo Code) to Partner Center
# (.github/workflows/solo-store.yml) when the repository variable
# SOLO_STORE_SUBMIT_ENABLED is "true"; while it isn't, download the
# solo-windows-x64-msix artifact of the Solo package run and upload the
# .msixupload in Partner Center by hand (docs\development.md).
#
#   script\release-solo.ps1 (-Notes <text> | -NotesFile <file>) (-Bump patch|minor|major | -Version <x.y.z>) [-DryRun] [-SkipCiCheck]
#
# Version numbers:
# - patch (2.5.1): bug fixes only. No new features, no new or changed
#   settings, no interface changes (fixing a typo aside). The notes list fixes.
# - minor (2.6.0): new features, changed behavior or defaults, new settings.
# - major (3.0.0): only when settings or data aren't compatible and users
#   have to do something by hand.
# Minor and major versions are released from main; releasing one also
# creates the branch release/<x.y> on its version commit. Patch versions are
# released from release/<x.y>: fixes go into main first and are then
# cherry-picked onto it; main's new features wait for the next minor version.
# Only the newest minor version gets patches.
#
# The release notes must start with a one-sentence summary in Chinese on the
# first line and in English on the second, plain text: the update
# notification shows the one in Solo's language
# (script\check-solo-release-notes.ps1 checks them before anything else).
#
# -Bump raises the version by those rules; -Version gives it, and must be
# the next patch, minor or major version. -DryRun checks everything and shows
# what would happen without changing, committing, tagging or pushing
# anything. If committing, tagging or pushing the branch fails, the version
# commit and the tag are undone. Needs a clean working tree on main or a
# release/<x.y> branch, tracking the same branch of origin.
#
# It releases only a commit that passed Solo CI (.github/workflows/solo-ci.yml):
# the latest Solo CI run on the current branch for the current commit (which
# becomes the version commit's parent) must have succeeded, so push the
# commit and wait for CI first. It asks GitHub with `gh run list` (the GitHub CLI,
# signed in); -DryRun checks it too. -SkipCiCheck releases without checking,
# for emergencies only.
#
# To build the Store package locally (for testing only; releases come from
# CI), see docs\development.md ("The Microsoft Store package").

param(
    [ValidateSet("", "patch", "minor", "major")]
    [string]$Bump = "",
    [string]$Version = "",
    [string]$Notes = "",
    [string]$NotesFile = "",
    [switch]$DryRun,
    [switch]$SkipCiCheck
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
$branch = git symbolic-ref -q --short HEAD
if (-not $branch) { throw "Check out main or a release/<x.y> branch; HEAD isn't on a branch." }
$upstreamBranch = git for-each-ref --format="%(upstream:short)" "refs/heads/$branch"
if ($upstreamBranch -ne "origin/$branch") { throw "$branch has to track origin/$branch (it tracks '$upstreamBranch')." }

$cargoToml = "crates\zed\Cargo.toml"
$current = (Select-String -Path $cargoToml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value

if (git status --porcelain --untracked-files=no) { throw "Commit or stash your changes first." }
if ([bool]$Bump -eq [bool]$Version) { throw "Give either -Bump patch|minor|major or -Version." }
$cur = [version]$current
$next = [ordered]@{
    patch = "$($cur.Major).$($cur.Minor).$($cur.Build + 1)"
    minor = "$($cur.Major).$($cur.Minor + 1).0"
    major = "$($cur.Major + 1).0.0"
}
if ($Bump) {
    $Version = $next[$Bump]
} else {
    if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3, not $Version" }
    $Bump = $next.Keys | Where-Object { $next[$_] -eq $Version } | Select-Object -First 1
    if (-not $Bump) { throw "$Version isn't the next patch ($($next.patch)), minor ($($next.minor)) or major ($($next.major)) version after $current." }
}
$v = [version]$Version
$releaseBranch = "release/$($v.Major).$($v.Minor)"
if ($Bump -eq "patch") {
    if ($branch -ne $releaseBranch) { throw "Patch version $Version is released from $releaseBranch, not $branch; cherry-pick the fixes onto $releaseBranch." }
} else {
    if ($branch -ne "main") { throw "The $Bump version $Version is released from main, not $branch." }
    $remoteBranch = git ls-remote --heads origin "refs/heads/$releaseBranch"
    if ($LASTEXITCODE -ne 0) { throw "Couldn't reach origin to check for $releaseBranch" }
    if ($remoteBranch -or (git branch --list $releaseBranch)) { throw "$releaseBranch already exists" }
}
$tag = "v$Version"
if (git tag --list $tag) { throw "Tag $tag already exists" }
$remoteTag = git ls-remote --tags origin "refs/tags/$tag"
if ($LASTEXITCODE -ne 0) { throw "Couldn't reach origin to check for tag $tag" }
if ($remoteTag) { throw "Tag $tag already exists on origin" }
# The workflow reads the notes from here (and checks them again).
$notesPath = "docs\release-notes\$Version.md"
if (Test-Path $notesPath) { throw "$notesPath already exists" }

# Only a commit that passed Solo CI on this branch. The version commit is
# new, so its parent, the current commit, is the one CI checked. (Runs
# started by hand on the branch count too; pull requests' runs are on their
# own branches.)
$head = git rev-parse HEAD
$shortHead = git rev-parse --short HEAD
$ciRuns = "https://github.com/$repo/actions/workflows/solo-ci.yml?query=branch%3A$([uri]::EscapeDataString($branch))"
if ($SkipCiCheck) {
    Write-Warning "Not checking that Solo CI passed on $shortHead (-SkipCiCheck)."
} else {
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        throw "Checking Solo CI needs the GitHub CLI (gh, signed in). Without it, check $ciRuns yourself and pass -SkipCiCheck only if Solo CI passed on $shortHead."
    }
    $runsJson = gh run list --repo $repo --workflow "Solo CI" --branch $branch --limit 100 --json "databaseId,headSha,status,conclusion,url"
    if ($LASTEXITCODE -ne 0) { throw "Couldn't list Solo CI's runs with gh (exit code $LASTEXITCODE); is gh signed in?" }
    # Newest first. (Windows PowerShell passes a JSON array on as one object.)
    $runs = ($runsJson -join "`n") | ConvertFrom-Json
    $ciRun = $null
    foreach ($run in $runs) {
        if ($run.headSha -eq $head) { $ciRun = $run; break }
    }
    if (-not $ciRun) {
        throw "Solo CI hasn't run on $branch for $shortHead, the commit to release: push it and wait for Solo CI to pass ($ciRuns)."
    }
    if ($ciRun.status -ne "completed") {
        throw "Solo CI on $shortHead hasn't finished ($($ciRun.status)); release once it passes: $($ciRun.url)"
    }
    if ($ciRun.conclusion -ne "success") {
        throw "Solo CI didn't pass on $shortHead ($($ciRun.conclusion)): $($ciRun.url). Fix it and release a commit it passes on."
    }
    Write-Host "Solo CI passed on $shortHead`: $($ciRun.url)"
}

# The version of the zed crate is the app's version, here and in Cargo.lock.
function Get-WithVersion([string]$path, [string]$pattern, [string]$replacement) {
    $text = [IO.File]::ReadAllText((Resolve-Path $path))
    $updated = [regex]::Replace($text, $pattern, $replacement, [Text.RegularExpressions.RegexOptions]::Multiline)
    if ($updated -eq $text) { throw "Couldn't set the version in $path" }
    # Written with LF line endings, like the repository stores them, even if
    # the checkout has CRLF: a CRLF file breaks the package workflow's checks.
    return $updated -replace "`r`n", "`n"
}
$newCargoToml = Get-WithVersion $cargoToml '^version = ".+"' "version = `"$Version`""
$newCargoLock = Get-WithVersion "Cargo.lock" '(?<=^name = "zed"\r?\nversion = ")[^"]+' $Version
# Stored with LF line endings, like the rest of the repository.
$notesText = ($Notes -replace "\r\n", "`n").TrimEnd() + "`n"

if ($DryRun) {
    Write-Host "Dry run: nothing is changed, committed, tagged or pushed."
    Write-Host "Would release the $Bump version Solo $Version (was $current) from $shortHead, pushing to $upstreamBranch"
    Write-Host "Would set the version in $cargoToml and Cargo.lock and write $notesPath`:"
    Write-Host $notesText
    Write-Host "Would commit `"Solo $Version`", tag it $tag and push the branch and $tag to $repo,"
    if ($Bump -ne "patch") { Write-Host "create $releaseBranch on the version commit and push it," }
    Write-Host "which starts https://github.com/$repo/actions/workflows/solo-package.yml"
    return
}

Write-Host "Releasing the $Bump version Solo $Version (was $current)"

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
if ($Bump -ne "patch") {
    git push origin "$($tag)^{commit}:refs/heads/$releaseBranch"
    if ($LASTEXITCODE -ne 0) { throw "Creating $releaseBranch failed; create it on the version commit: git push origin $($tag)^{commit}:refs/heads/$releaseBranch" }
    Write-Host "Created $releaseBranch for $Version's patch versions."
}
Write-Host "Pushed $tag. The Solo package workflow builds it and creates a draft release:"
Write-Host "  https://github.com/$repo/actions/workflows/solo-package.yml"
Write-Host "Check the draft at https://github.com/$repo/releases, then publish it."
