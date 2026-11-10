# Checks that Solo release notes start the way the update notification
# expects (crates/auto_update/src/solo_update.rs, notes_summary): the first
# line a one-sentence summary in Chinese, the second the same in English, both
# plain text (no markdown, links or addresses). Blank lines before them don't
# count. Throws, saying what's wrong, if they don't.
#
#   script\check-solo-release-notes.ps1 -Notes <text>
#   script\check-solo-release-notes.ps1 -Path <notes.md>

param(
    [string]$Notes = "",
    [string]$Path = ""
)
$ErrorActionPreference = "Stop"

if ($Path) { $Notes = [IO.File]::ReadAllText((Resolve-Path $Path)) }
$lines = @($Notes -split "\r?\n" | ForEach-Object { $_.Trim() } | Where-Object { $_ })

# ASCII only: Windows PowerShell reads a script without a BOM in the ANSI code page.
$example = @"
Release notes must start with two summary lines, one sentence each, plain text:
the first in Chinese, the second in English, e.g.
  Solo 2.1: more accurate agent status and more reliable terminals.
The update notification shows the one in Solo's language; the rest of the notes follow.
"@

function Test-SummaryLine([string]$line, [bool]$chinese) {
    if ($line -match '^[#\-*>\[!|`<+]' -or $line.Contains('](') -or $line.Contains('://')) {
        return "is markdown or has a link"
    }
    $hasChinese = $line -match '[\p{IsCJKUnifiedIdeographs}\p{IsCJKUnifiedIdeographsExtensionA}\p{IsCJKCompatibilityIdeographs}]'
    if ($chinese -and -not $hasChinese) { return "has no Chinese" }
    if (-not $chinese -and $hasChinese) { return "has Chinese" }
    if (-not $chinese -and $line -notmatch '[A-Za-z]') { return "has no English" }
    return $null
}

if ($lines.Count -lt 2) { throw "The release notes have fewer than two lines.`n$example" }
$problems = @()
$problem = Test-SummaryLine $lines[0] $true
if ($problem) { $problems += "The first line (the Chinese summary) $problem`: $($lines[0])" }
$problem = Test-SummaryLine $lines[1] $false
if ($problem) { $problems += "The second line (the English summary) $problem`: $($lines[1])" }
if ($problems) { throw (($problems -join "`n") + "`n" + $example) }
