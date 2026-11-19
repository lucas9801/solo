# Fails when a commit message in the given range names the upstream editor.
# Commit, merge and tag messages and branch names in this repository describe
# what changed in Solo's own words; the editor Solo is based on is credited
# in the files (README, UPSTREAM.md, licenses), not in the history.
#
#   script\check-commit-messages.ps1 <revision range>    e.g. origin/main..HEAD
param([Parameter(Mandatory = $true)][string]$Range)
$ErrorActionPreference = "Stop"
$name = 'z' + 'ed'
$pattern = "(?i)(?<![a-z])$name(?![a-z])"
$commits = @(git rev-list $Range)
if ($LASTEXITCODE -ne 0) { throw "git rev-list $Range failed" }
$problems = @()
foreach ($commit in $commits) {
    $message = (git log -1 --format=%B $commit) -join "`n"
    $lineNumber = 0
    foreach ($line in $message -split "`n") {
        $lineNumber++
        if ($line -match $pattern) { $problems += "$($commit.Substring(0, 10)) line ${lineNumber}: $line" }
    }
}
if ($problems) {
    $problems | ForEach-Object { Write-Host $_ }
    throw "$($problems.Count) commit message line(s) name the upstream editor; describe the change without it."
}
Write-Host "Checked $($commits.Count) commit message(s)."
