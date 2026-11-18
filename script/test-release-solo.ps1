# Tests script\release-solo.ps1's version rules in a throwaway repository
# (with a bare repository as its origin) in the temp folder: which version
# each -Bump gives, from which branch each kind of version may be released,
# and that a minor version creates its release branch. Nothing outside the
# temp folder is touched, and GitHub isn't asked (-SkipCiCheck).
#
#   script\test-release-solo.ps1

$ErrorActionPreference = "Stop"
$scriptDir = $PSScriptRoot
$work = Join-Path ([IO.Path]::GetTempPath()) "solo-release-test-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
$origin = Join-Path $work "origin.git"
$repo = Join-Path $work "repo"
# The notes, with a Chinese first line (written with char codes: this
# script is ASCII, as Windows PowerShell reads it in the ANSI code page).
$notesFile = Join-Path $work "notes.md"
$failures = 0

function Invoke-Git {
    $output = & git @args 2>&1
    if ($LASTEXITCODE -ne 0) { throw "git $args failed: $output" }
    $output
}

# Runs the release script in its own PowerShell, from the test repository.
function Invoke-Release([string[]]$arguments) {
    $script = Join-Path $repo "script\release-solo.ps1"
    # Its errors go to stderr, which mustn't stop this script.
    $ErrorActionPreference = "Continue"
    $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script -NotesFile $notesFile -SkipCiCheck @arguments 2>&1 | Out-String
    [pscustomobject]@{ Ok = ($LASTEXITCODE -eq 0); Output = $output }
}

function Test-Release([string]$name, [string[]]$arguments, [bool]$ok, [string]$expect) {
    $result = Invoke-Release $arguments
    if ($result.Ok -ne $ok -or ($expect -and $result.Output -notlike "*$expect*")) {
        Write-Host "FAIL: $name (expected $(if ($ok) { 'success' } else { 'failure' }) with '$expect')"
        Write-Host $result.Output
        $script:failures++
    } else {
        Write-Host "ok: $name"
    }
}

try {
    New-Item -ItemType Directory -Force $repo | Out-Null
    $chinese = -join [char[]](0x6D4B, 0x8BD5, 0x3002)
    [IO.File]::WriteAllText($notesFile, "$chinese`nTest.`n", (New-Object System.Text.UTF8Encoding $false))
    Invoke-Git init -q --bare $origin | Out-Null
    Push-Location $repo
    Invoke-Git init -q -b main | Out-Null
    Invoke-Git config user.name test | Out-Null
    Invoke-Git config user.email test@example.com | Out-Null
    Invoke-Git config core.autocrlf false | Out-Null
    New-Item -ItemType Directory -Force "crates\solo", "script" | Out-Null
    [IO.File]::WriteAllText("$repo\crates\solo\Cargo.toml", "[package]`nname = `"solo`"`nversion = `"2.4.1`"`n")
    [IO.File]::WriteAllText("$repo\Cargo.lock", "[[package]]`nname = `"solo`"`nversion = `"2.4.1`"`n")
    Copy-Item "$scriptDir\release-solo.ps1", "$scriptDir\check-solo-release-notes.ps1" "$repo\script"
    Invoke-Git add -A | Out-Null
    Invoke-Git commit -q -m "Solo 2.4.1" | Out-Null
    Invoke-Git remote add origin $origin | Out-Null
    Invoke-Git push -q -u origin main | Out-Null
    Invoke-Git branch -q release/2.4 | Out-Null
    Invoke-Git push -q -u origin release/2.4 | Out-Null

    Test-Release "neither -Bump nor -Version" @("-DryRun") $false "Give either"
    Test-Release "both -Bump and -Version" @("-Bump", "minor", "-Version", "2.5.0", "-DryRun") $false "Give either"
    Test-Release "a patch version from main" @("-Bump", "patch", "-DryRun") $true "patch version Solo 2.4.2"
    Test-Release "a version that isn't the next one is refused" @("-Version", "2.7.0", "-DryRun") $false "isn't the next"
    Test-Release "a minor version from main" @("-Bump", "minor", "-DryRun") $true "create release/2.5"
    Test-Release "-Version names the next minor version" @("-Version", "2.5.0", "-DryRun") $true "minor version Solo 2.5.0"
    Test-Release "a major version from main" @("-Bump", "major", "-DryRun") $true "create release/3.0"

    Invoke-Git checkout -q release/2.4 | Out-Null
    Test-Release "a minor version from a release branch is refused" @("-Bump", "minor", "-DryRun") $false "released from main"
    Test-Release "a patch version from its release branch" @("-Bump", "patch", "-DryRun") $true "patch version Solo 2.4.2"

    Invoke-Git checkout -q main | Out-Null
    Test-Release "releasing a minor version" @("-Bump", "minor") $true "Created release/2.5"
    $branchCommit = Invoke-Git --git-dir $origin rev-parse release/2.5
    $tagCommit = Invoke-Git --git-dir $origin rev-parse "v2.5.0^{commit}"
    if ($branchCommit -ne $tagCommit) {
        Write-Host "FAIL: release/2.5 ($branchCommit) isn't on v2.5.0's commit ($tagCommit)"
        $failures++
    } else {
        Write-Host "ok: release/2.5 is on v2.5.0's commit"
    }

    Invoke-Git fetch -q origin | Out-Null
    Invoke-Git checkout -q -b release/2.5 --track origin/release/2.5 | Out-Null
    Test-Release "a patch version from the new release branch" @("-Bump", "patch", "-DryRun") $true "patch version Solo 2.5.1"
    Invoke-Git checkout -q main | Out-Null
    Invoke-Git reset -q --hard HEAD~1 | Out-Null
    Test-Release "a minor version whose release branch exists is refused" @("-Version", "2.5.0", "-DryRun") $false "release/2.5 already exists"
} finally {
    Pop-Location -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

if ($failures) {
    Write-Host "$failures release test(s) failed"
    exit 1
}
Write-Host "All release tests passed"
exit 0
