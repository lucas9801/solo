# Solo completion helper: answers completion requests from the terminal input
# with PowerShell's own TabExpansion2, so commands, parameters, paths and the
# user's custom argument completers all work. It also runs the commands of
# completion spec generators (e.g. listing git branches) and returns their
# output.
#
# Protocol: one JSON request per input line, either {input, cursor, cwd}
# (cursor in UTF-16 code units) or {exec, cwd}, and one "SOLO:"-prefixed JSON
# response per request. Any other output (e.g. from the user's profile) is
# ignored by the terminal.

$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::InputEncoding = New-Object System.Text.UTF8Encoding $false
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
# Native commands' output (git, npm, ...) is UTF-8 too.
$OutputEncoding = New-Object System.Text.UTF8Encoding $false

# The profile registers argument completers (posh-git, winget, ...).
if (Test-Path -LiteralPath $PROFILE) {
    try { . $PROFILE *> $null } catch {}
}

while ($true) {
    $line = [Console]::In.ReadLine()
    if ($null -eq $line) { break }
    $line = $line.TrimStart([char]0xFEFF)
    $response = @{ index = 0; length = 0; items = @() }
    try {
        $request = $line | ConvertFrom-Json
        if ($request.cwd) { Set-Location -LiteralPath $request.cwd }
        if ($request.exec) {
            $output = Invoke-Expression $request.exec 2>$null | Out-String -Width 4096
            $response = @{ output = "$output" }
        } else {
            $result = TabExpansion2 -inputScript $request.input -cursorColumn ([int]$request.cursor)
            $response.index = $result.ReplacementIndex
            $response.length = $result.ReplacementLength
            $response.items = @($result.CompletionMatches | Select-Object -First 500 | ForEach-Object {
                @{ text = $_.CompletionText; label = $_.ListItemText; kind = "$($_.ResultType)"; tip = $_.ToolTip }
            })
        }
    } catch {}
    [Console]::Out.WriteLine("SOLO:" + (ConvertTo-Json -InputObject $response -Compress -Depth 4))
    [Console]::Out.Flush()
}
