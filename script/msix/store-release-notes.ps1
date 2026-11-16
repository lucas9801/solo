# Makes the "What's new in this version" text of Solo Code's Store listing
# (ReleaseNotes in a Partner Center submission) from Solo's release notes,
# docs\release-notes\<version>.md, for each listing language. Used by
# .github/workflows/solo-store.yml; tested by
# script\msix\test-store-release-notes.ps1.
#
#   script\msix\store-release-notes.ps1 -Path <notes.md> -Languages zh-cn,en-us [-MaxLength 1500]
#
# The release notes start with a one-sentence summary in Chinese and one in
# English (script\check-solo-release-notes.ps1, which is run first), followed
# by Chinese sections and then English ones ("## Fixes"), whose items start
# with a bold title ("- **Title:** details"). A language whose code starts
# with "zh" gets the Chinese summary and the titles of the items under the
# Chinese headings; every other language gets the English ones:
#
#   <summary>
#
#   <heading>
#   - <item title>
#   ...
#
# Partner Center takes at most 1500 characters there; a text that would be
# longer is just the summary. Throws if the summary alone is too long.
#
# Writes one object per language: Language, Text, and Shortened (true when
# the item titles were left out for length).

param(
    [Parameter(Mandatory = $true)]
    [string]$Path,
    [Parameter(Mandatory = $true)]
    [string[]]$Languages,
    [int]$MaxLength = 1500
)
$ErrorActionPreference = "Stop"

# ASCII only: Windows PowerShell reads a script without a BOM in the ANSI
# code page, so Chinese characters are written as \u escapes in the regexes.
$cjk = '[\p{IsCJKUnifiedIdeographs}\p{IsCJKUnifiedIdeographsExtensionA}\p{IsCJKCompatibilityIdeographs}]'

$notes = [IO.File]::ReadAllText((Resolve-Path $Path))
& (Join-Path (Split-Path -Parent $PSScriptRoot) "check-solo-release-notes.ps1") -Notes $notes

# "zh-cn,en-us" as one argument (powershell -File) or as an array.
$Languages = @($Languages | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
if (-not $Languages) { throw "Give at least one listing language with -Languages." }

$lines = @($notes -split "\r?\n")
$nonBlank = @($lines | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$summary = @{ zh = $nonBlank[0]; en = $nonBlank[1] }

# A title as plain text: links become their text, code its content, and the
# punctuation that ends the bold part (":", full-width colon or semicolon...)
# is dropped.
function ConvertTo-PlainTitle([string]$title) {
    $title = $title -replace '\[([^\]]*)\]\([^)]*\)', '$1'
    $title = $title.Replace('`', '').Replace('**', '')
    return ($title -replace '[\s:;,.!\uFF1A\uFF1B\uFF0C\u3002\uFF01\u3001]+$', '').Trim()
}

# The sections of each language, in order: heading and item titles.
$sections = @{ zh = New-Object System.Collections.ArrayList; en = New-Object System.Collections.ArrayList }
$current = $null
foreach ($line in $lines) {
    if ($line -match '^#{1,6}\s+(.+?)\s*#*\s*$') {
        $heading = ConvertTo-PlainTitle $Matches[1]
        $language = if ($heading -match $cjk) { "zh" } else { "en" }
        $current = [pscustomobject]@{ Heading = $heading; Titles = New-Object System.Collections.ArrayList }
        [void]$sections[$language].Add($current)
    } elseif ($current -and $line -match '^ ?[-*+]\s+\*\*(.+?)\*\*') {
        $title = ConvertTo-PlainTitle $Matches[1]
        if ($title) { [void]$current.Titles.Add($title) }
    }
}

foreach ($listing in $Languages) {
    $language = if ($listing -match '^zh(-|$)') { "zh" } else { "en" }
    $short = $summary[$language]
    if ($short.Length -gt $MaxLength) {
        throw "The $language summary is $($short.Length) characters; the Store takes at most $MaxLength."
    }
    $text = $short
    foreach ($section in $sections[$language]) {
        if ($section.Titles.Count -eq 0) { continue }
        $text += "`n`n" + $section.Heading
        foreach ($title in $section.Titles) { $text += "`n- " + $title }
    }
    $shortened = $text.Length -gt $MaxLength
    if ($shortened) { $text = $short }
    [pscustomobject]@{ Language = $listing; Text = $text; Shortened = $shortened }
}
