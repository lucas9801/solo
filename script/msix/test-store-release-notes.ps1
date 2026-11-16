# Tests script\msix\store-release-notes.ps1 (the Store listing's "What's new
# in this version" text) with release notes written to the temp folder.
#
#   powershell -ExecutionPolicy Bypass -File script\msix\test-store-release-notes.ps1
#
# Exits with 1 if a test fails.

$ErrorActionPreference = "Stop"
$script = Join-Path $PSScriptRoot "store-release-notes.ps1"
$work = Join-Path ([IO.Path]::GetTempPath()) "solo-store-notes-test-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
New-Item -ItemType Directory -Force $work | Out-Null
$failures = 0

# Chinese text from char codes: this script is ASCII, as Windows PowerShell
# reads it in the ANSI code page.
function Zh([int[]]$codes) { -join [char[]]$codes }
$colon = Zh 0xFF1A
$semicolon = Zh 0xFF1B
$test = Zh 0x6D4B, 0x8BD5                 # "test"
$fixes = Zh 0x4FEE, 0x590D                # "fixes"
$improvements = Zh 0x6539, 0x8FDB         # "improvements"
$title = Zh 0x6807, 0x9898                # "title"
$period = Zh 0x3002
$zhSummary = "Solo 9.9$colon$test$period"
$enSummary = "Solo 9.9: a test."

function Write-Notes([string]$name, [string[]]$lines, [string]$newline = "`n") {
    $path = Join-Path $work $name
    [IO.File]::WriteAllText($path, ($lines -join $newline) + $newline, (New-Object System.Text.UTF8Encoding $false))
    $path
}

function Invoke-Notes([string]$path, $languages, [int]$maxLength = 1500) {
    $result = @{}
    foreach ($item in & $script -Path $path -Languages $languages -MaxLength $maxLength) { $result[$item.Language] = $item }
    $result
}

function Test-Equal([string]$name, $actual, $expected) {
    if ($actual -cne $expected) {
        Write-Host "FAIL ${name}:`n  got      '$actual'`n  expected '$expected'"
        $script:failures++
    } else {
        Write-Host "ok: $name"
    }
}

function Test-Throws([string]$name, [scriptblock]$block, [string]$expect) {
    try {
        & $block | Out-Null
        Write-Host "FAIL ${name}: didn't throw"
        $script:failures++
    } catch {
        if ($expect -and "$_" -notlike "*$expect*") {
            Write-Host "FAIL ${name}: threw '$_', expected '$expect'"
            $script:failures++
        } else {
            Write-Host "ok: $name"
        }
    }
}

try {
    $notes = Write-Notes "notes.md" @(
        $zhSummary,
        $enSummary,
        "",
        "[guide](https://example.com/zh) - [User guide](https://example.com/en)",
        "",
        "## $improvements",
        "",
        "- **$test A$colon** details.",
        "- **$title ``code``$semicolon** more details.",
        "  - **nested$colon** not an item of its own.",
        "",
        "## $fixes",
        "",
        "- **[$title B](https://example.com)$colon** details.",
        "- no bold title, left out",
        "",
        "## Improvements",
        "",
        "- **Feature A:** details.",
        "- **The ``code`` thing;** more details.",
        "",
        "## Fixes",
        "",
        "* **[Fix B](https://example.com):** details.",
        "",
        "Closing paragraph, not an item."
    )
    $zhFull = "$zhSummary`n`n$improvements`n- $test A`n- $title code`n`n$fixes`n- $title B"
    $enFull = "$enSummary`n`nImprovements`n- Feature A`n- The code thing`n`nFixes`n- Fix B"

    $result = Invoke-Notes $notes @("zh-cn", "en-us")
    Test-Equal "Chinese listing" $result["zh-cn"].Text $zhFull
    Test-Equal "English listing" $result["en-us"].Text $enFull
    Test-Equal "not shortened" $result["zh-cn"].Shortened $false

    # Other languages: any zh-* is Chinese, everything else English.
    $result = Invoke-Notes $notes @("ZH-CN", "zh-tw", "zh", "en-gb", "fr-fr", "zhx")
    Test-Equal "upper case zh-cn" $result["ZH-CN"].Text $zhFull
    Test-Equal "zh-tw" $result["zh-tw"].Text $zhFull
    Test-Equal "zh" $result["zh"].Text $zhFull
    Test-Equal "en-gb" $result["en-gb"].Text $enFull
    Test-Equal "fr-fr in English" $result["fr-fr"].Text $enFull
    Test-Equal "zhx isn't Chinese" $result["zhx"].Text $enFull

    # One argument with commas, as powershell -File passes it.
    $result = Invoke-Notes $notes "zh-cn, en-us"
    Test-Equal "comma-separated languages" (@($result.Keys | Sort-Object) -join ",") "en-us,zh-cn"

    # Windows line endings and a blank line before the summaries.
    $crlf = Write-Notes "crlf.md" (@("") + (Get-Content -Encoding UTF8 $notes)) "`r`n"
    $result = Invoke-Notes $crlf @("zh-cn", "en-us")
    Test-Equal "CRLF Chinese" $result["zh-cn"].Text $zhFull
    Test-Equal "CRLF English" $result["en-us"].Text $enFull

    # Too long with the titles: only the summary.
    $result = Invoke-Notes $notes @("zh-cn", "en-us") ($enFull.Length - 1)
    Test-Equal "long English is the summary" $result["en-us"].Text $enSummary
    Test-Equal "long English is shortened" $result["en-us"].Shortened $true
    Test-Equal "shorter Chinese still fits" $result["zh-cn"].Text $zhFull
    $result = Invoke-Notes $notes @("en-us") $enFull.Length
    Test-Equal "exactly the limit" $result["en-us"].Text $enFull

    # Only summaries.
    $plain = Write-Notes "plain.md" @($zhSummary, $enSummary, "", "Some text.")
    $result = Invoke-Notes $plain @("zh-cn", "en-us")
    Test-Equal "no items, Chinese" $result["zh-cn"].Text $zhSummary
    Test-Equal "no items, English" $result["en-us"].Text $enSummary

    # The 1500-character default.
    $many = Write-Notes "many.md" (@($zhSummary, $enSummary, "", "## Fixes") + (1..100 | ForEach-Object { "- **Fix number $_ of many:** details." }))
    $result = Invoke-Notes $many @("en-us")
    Test-Equal "default limit" $result["en-us"].Text $enSummary

    Test-Throws "summary too long" { Invoke-Notes $notes @("en-us") 5 } "at most 5"
    $bad = Write-Notes "bad.md" @($enSummary, $zhSummary)
    Test-Throws "summaries swapped" { Invoke-Notes $bad @("en-us") } "Chinese"
    Test-Throws "no languages" { Invoke-Notes $notes " , " } "language"
    Test-Throws "missing file" { Invoke-Notes (Join-Path $work "missing.md") @("en-us") }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

if ($failures) {
    Write-Host "$failures test(s) failed"
    exit 1
}
Write-Host "All Store release notes tests passed"
exit 0
