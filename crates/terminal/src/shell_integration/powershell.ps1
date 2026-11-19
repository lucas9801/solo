# Solo shell integration for PowerShell.
#
# Marks each prompt and command with OSC 133 (A: prompt start, B: command input
# start, C: command executed, D;<exit code>: command finished) and reports the
# working directory with OSC 7, so the terminal can split its output into
# command blocks. Loaded after the user's profile, so it wraps their prompt.

if ($Global:__SoloShellIntegration) { return }
$Global:__SoloShellIntegration = $true
$Global:__SoloCommandExecuted = $false
$Global:__SoloFirstPrompt = $true
$Global:__SoloOriginalPrompt = $function:prompt
# The terminal's nonce goes with the sequences it trusts; programs run here
# don't inherit it.
$Global:__SoloNonce = $env:SOLO_SHELL_NONCE
Remove-Item Env:SOLO_SHELL_NONCE -ErrorAction SilentlyContinue

# Tell the terminal which PowerShell this is, so its input editor can ask the
# same one for completions.
$__soloShellPath = (Get-Process -Id $PID).Path -replace '\\', '\\' -replace ';', '\x3b'
[Console]::Write("$([char]27)]633;P;Shell=$__soloShellPath;$Global:__SoloNonce$([char]7)")
Remove-Variable __soloShellPath

function Global:prompt {
    $succeeded = $Global:?
    $lastExitCode = $Global:LASTEXITCODE
    # A function (the claude wrapper below) leaves $? set even when the program
    # it ran failed.
    if ($null -ne $Global:__SoloWrappedExitCode) {
        if ($succeeded -and $Global:__SoloWrappedExitCode -ne 0) {
            $succeeded = $false
            $lastExitCode = $Global:__SoloWrappedExitCode
        }
        $Global:__SoloWrappedExitCode = $null
    }
    $esc = [char]27
    $bel = [char]7
    $marks = ""

    # Without PSReadLine there is no C mark; a new history entry still means a
    # command ran.
    # The first prompt follows startup (profile and this script), not a command.
    $lastHistory = Get-History -Count 1
    if ($Global:__SoloFirstPrompt) {
        $Global:__SoloFirstPrompt = $false
        $Global:__SoloCommandExecuted = $false
        $Global:__SoloLastHistoryId = $lastHistory.Id
    } elseif ($lastHistory -and $lastHistory.Id -ne $Global:__SoloLastHistoryId) {
        $Global:__SoloCommandExecuted = $true
        $Global:__SoloLastHistoryId = $lastHistory.Id
    }

    if ($Global:__SoloCommandExecuted) {
        $Global:__SoloCommandExecuted = $false
        $exitCode = if ($succeeded) { 0 } elseif ($lastExitCode -is [int] -and $lastExitCode -ne 0) { $lastExitCode } else { 1 }
        $marks += "$esc]133;D;$exitCode$bel"
    }

    $location = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($location.Provider.Name -eq "FileSystem") {
        $path = $location.ProviderPath -replace "\\", "/"
        if (-not $path.StartsWith("/")) { $path = "/$path" }
        $marks += "$esc]7;file://$env:COMPUTERNAME$([System.Uri]::EscapeUriString($path))$bel"
    }

    $marks += "$esc]133;A$bel"
    $userPrompt = if ($env:SOLO_COMPACT_PROMPT -eq '1') {
        # Solo's input editor shows the full path; keep the scrollback terse.
        $leaf = Split-Path -Leaf $location.Path
        if (-not $leaf) { $leaf = $location.Path }
        "$esc[2m$leaf$esc[0m $esc[36m$([char]0x276F)$esc[0m "
    } elseif ($Global:__SoloOriginalPrompt) { & $Global:__SoloOriginalPrompt } else { "PS $($location.Path)> " }
    # The user's prompt may run native commands; keep the command's exit code.
    $Global:LASTEXITCODE = $lastExitCode
    "$marks$userPrompt$esc]133;B$bel"
}

# Agents' hooks (Claude Code's, Codex's, Copilot's, OpenCode's, and the others
# Solo's hook program reports for) write to this console.
if ($env:SOLO_CLAUDE_SETTINGS -or $env:SOLO_CODEX_ARGS -or $env:SOLO_COPILOT_PLUGIN -or $env:SOLO_OPENCODE_CONFIG_DIR -or $env:SOLO_AGENT_HOOK) {
    $env:SOLO_AGENT_CONSOLE_PID = "$PID"
}

# SOLO: Whether Solo's files an agent wrapper passes to its agent are there.
# If one is gone (a temp or disk cleaner, by hand), Solo is asked to write its
# files again, and this waits a moment for them; if they don't come back, it
# says the agent runs without status reports here, and tells Solo so.
function Global:__SoloEnsureFiles {
    param([string]$Agent, [string[]]$Paths)
    $missing = @($Paths | Where-Object { $_ -and -not (Test-Path -LiteralPath $_) })
    if ($missing.Count -eq 0) { return $true }
    $esc = [char]27
    $bel = [char]7
    [Console]::Write("$esc]6973;agent;nonce=$Global:__SoloNonce;restore-files$bel")
    $deadline = [DateTime]::UtcNow.AddSeconds(2)
    while ([DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 50
        $missing = @($missing | Where-Object { -not (Test-Path -LiteralPath $_) })
        if ($missing.Count -eq 0) { return $true }
    }
    [Console]::Write("$esc]6973;agent;nonce=$Global:__SoloNonce;no-hooks;$Agent$bel")
    [Console]::Error.WriteLine("Solo: $Agent runs without status reports in this terminal (Solo's files for it are missing). A new terminal fixes it.")
    return $false
}

# Claude Code reports what it's doing through hooks in the settings file Solo
# passes it (see SOLO_CLAUDE_SETTINGS); the hooks write to this console.
# A claude function or alias of the user's own stays.
if ($env:SOLO_CLAUDE_SETTINGS -and -not (Get-Command -Name claude -CommandType Function, Alias -ErrorAction SilentlyContinue)) {
    function Global:claude {
        $program = Get-Command -Name claude -CommandType Application, ExternalScript -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $program) {
            Write-Error "claude: command not found"
            return
        }
        # Subcommands, background sessions and the user's own --settings run as typed.
        $asTyped = ($args.Count -gt 0 -and "$($args[0])" -in @('agents', 'attach', 'auth', 'auto-mode', 'doctor', 'gateway', 'import', 'install', 'logs', 'mcp', 'plugin', 'plugins', 'purge', 'respawn', 'rm', 'setup-token', 'stop', 'kill', 'ultrareview', 'update', 'upgrade')) -or
            ($args | Where-Object { "$_" -in @('--settings', '--bg', '--background') -or "$_" -like '--settings=*' })
        $settings = @()
        if (-not $asTyped -and (__SoloEnsureFiles -Agent claude -Paths @($env:SOLO_CLAUDE_SETTINGS, $env:SOLO_CLAUDE_MCP_CONFIG))) {
            $settings = @('--settings', $env:SOLO_CLAUDE_SETTINGS)
            # Solo's MCP servers (its browser, its IDE tools). --mcp-config adds to
            # the user's own MCP servers; it takes several files, so it goes
            # before --settings, which ends the list.
            if ($env:SOLO_CLAUDE_MCP_CONFIG -and (Test-Path -LiteralPath $env:SOLO_CLAUDE_MCP_CONFIG)) {
                $settings = @('--mcp-config', $env:SOLO_CLAUDE_MCP_CONFIG) + $settings
            }
        }
        $Global:LASTEXITCODE = 0
        if ($MyInvocation.ExpectingInput) {
            $input | & $program @settings @args
        } else {
            & $program @settings @args
        }
        $Global:__SoloWrappedExitCode = $Global:LASTEXITCODE
    }
}

# Codex reports what it's doing through hooks Solo adds to the session as
# config overrides, one argument per line in SOLO_CODEX_ARGS.
# A codex function or alias of the user's own stays.
if ($env:SOLO_CODEX_ARGS -and -not (Get-Command -Name codex -CommandType Function, Alias -ErrorAction SilentlyContinue)) {
    function Global:codex {
        $programs = @(Get-Command -Name codex -CommandType Application, ExternalScript -All -ErrorAction SilentlyContinue)
        if ($programs.Count -eq 0) {
            Write-Error "codex: command not found"
            return
        }
        # cmd.exe would expand % and treat & specially in the hooks' command
        # (the hook's path), so a .cmd shim (npm installs a .ps1 beside it)
        # runs as typed.
        $program = $programs | Where-Object { $_.Extension -notin @('.cmd', '.bat') } | Select-Object -First 1
        # Commands that don't run a session run as typed too.
        $hooks = @()
        if ($program -and -not ($args.Count -gt 0 -and "$($args[0])" -in @('login', 'logout', 'mcp', 'mcp-server', 'plugin', 'completion', 'update', 'doctor', 'sandbox', 'debug', 'apply', 'app', 'app-server', 'remote-control', 'exec-server', 'cloud', 'features', 'help', 'agents', 'archive', 'delete', 'unarchive', 'migrate-rollouts', 'queue'))) {
            $hooks = @($env:SOLO_CODEX_ARGS -split "`n")
        }
        if (-not $program) {
            $program = $programs[0]
        }
        $Global:LASTEXITCODE = 0
        if ($MyInvocation.ExpectingInput) {
            $input | & $program @hooks @args
        } else {
            & $program @hooks @args
        }
        $Global:__SoloWrappedExitCode = $Global:LASTEXITCODE
    }
}

# GitHub Copilot CLI reports what it's doing through hooks in a plugin Solo
# loads for the session (see SOLO_COPILOT_PLUGIN).
# A copilot function or alias of the user's own stays.
if ($env:SOLO_COPILOT_PLUGIN -and -not (Get-Command -Name copilot -CommandType Function, Alias -ErrorAction SilentlyContinue)) {
    function Global:copilot {
        $program = Get-Command -Name copilot -CommandType Application, ExternalScript -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $program) {
            Write-Error "copilot: command not found"
            return
        }
        # Commands that don't run a session run as typed.
        $plugin = @()
        if (-not ($args.Count -gt 0 -and "$($args[0])" -in @('app', 'login', 'help', 'init', 'config', 'update', 'version', 'sessions', 'memories', 'plugin', 'mcp', 'skill', 'instruction', 'lsp', 'sandbox', 'completion')) -and (__SoloEnsureFiles -Agent copilot -Paths @($env:SOLO_COPILOT_PLUGIN))) {
            $plugin = @('--plugin-dir', $env:SOLO_COPILOT_PLUGIN)
        }
        $Global:LASTEXITCODE = 0
        if ($MyInvocation.ExpectingInput) {
            $input | & $program @plugin @args
        } else {
            & $program @plugin @args
        }
        $Global:__SoloWrappedExitCode = $Global:LASTEXITCODE
    }
}

# OpenCode reports what it's doing through plugins Solo loads for the session:
# OpenCode 2's from SOLO_OPENCODE_CLI_CONFIG, OpenCode 1's from the folder in
# SOLO_OPENCODE_CONFIG_DIR. Variables of the user's own stay.
# An opencode function or alias of the user's own stays.
if ($env:SOLO_OPENCODE_CONFIG_DIR -and -not (Get-Command -Name opencode -CommandType Function, Alias -ErrorAction SilentlyContinue)) {
    function Global:opencode {
        $program = Get-Command -Name opencode -CommandType Application, ExternalScript -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $program) {
            Write-Error "opencode: command not found"
            return
        }
        $setCliConfig = $env:SOLO_OPENCODE_CLI_CONFIG -and -not (Test-Path Env:OPENCODE_CLI_CONFIG_CONTENT)
        $setConfigDir = -not (Test-Path Env:OPENCODE_CONFIG_DIR) -and (__SoloEnsureFiles -Agent opencode -Paths @($env:SOLO_OPENCODE_CONFIG_DIR))
        if ($setCliConfig) { $env:OPENCODE_CLI_CONFIG_CONTENT = $env:SOLO_OPENCODE_CLI_CONFIG }
        if ($setConfigDir) { $env:OPENCODE_CONFIG_DIR = $env:SOLO_OPENCODE_CONFIG_DIR }
        try {
            $Global:LASTEXITCODE = 0
            if ($MyInvocation.ExpectingInput) {
                $input | & $program @args
            } else {
                & $program @args
            }
            $Global:__SoloWrappedExitCode = $Global:LASTEXITCODE
        } finally {
            if ($setCliConfig) { Remove-Item Env:OPENCODE_CLI_CONFIG_CONTENT -ErrorAction SilentlyContinue }
            if ($setConfigDir) { Remove-Item Env:OPENCODE_CONFIG_DIR -ErrorAction SilentlyContinue }
        }
    }
}

# SOLO: Pi and oh-my-pi report what they're doing through an extension Solo
# loads for the session with -e (see SOLO_PI_EXTENSION).
# Functions or aliases of the user's own stay.
if ($env:SOLO_PI_EXTENSION) {
    function Global:__SoloPiLike {
        param([string]$Name, [object[]]$Arguments, $Pipeline, [bool]$HasInput)
        $program = Get-Command -Name $Name -CommandType Application, ExternalScript -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $program) {
            Write-Error "${Name}: command not found"
            return
        }
        $extension = @()
        if (-not ($Arguments.Count -gt 0 -and "$($Arguments[0])" -in @('install', 'remove', 'uninstall', 'update', 'list', 'config', 'plugin', 'plugins', 'help')) -and (__SoloEnsureFiles -Agent $Name -Paths @($env:SOLO_PI_EXTENSION))) {
            $extension = @('-e', $env:SOLO_PI_EXTENSION)
        }
        $Global:LASTEXITCODE = 0
        if ($HasInput) {
            $Pipeline | & $program @extension @Arguments
        } else {
            & $program @extension @Arguments
        }
        $Global:__SoloWrappedExitCode = $Global:LASTEXITCODE
    }
    if (-not (Get-Command -Name pi -CommandType Function, Alias -ErrorAction SilentlyContinue)) {
        function Global:pi { __SoloPiLike -Name pi -Arguments $args -Pipeline $input -HasInput $MyInvocation.ExpectingInput }
    }
    if (-not (Get-Command -Name omp -CommandType Function, Alias -ErrorAction SilentlyContinue)) {
        function Global:omp { __SoloPiLike -Name omp -Arguments $args -Pipeline $input -HasInput $MyInvocation.ExpectingInput }
    }
}

if (Get-Command PSConsoleHostReadLine -ErrorAction SilentlyContinue) {
    $Global:__SoloOriginalReadLine = $function:PSConsoleHostReadLine
    function Global:PSConsoleHostReadLine {
        $line = & $Global:__SoloOriginalReadLine
        $Global:__SoloCommandExecuted = $true
        # OSC 633 E escaping: backslash doubled, ';' and control characters as \xAB.
        $escaped = [System.Text.StringBuilder]::new()
        foreach ($char in "$line".ToCharArray()) {
            if ($char -eq '\') { [void]$escaped.Append('\\') }
            elseif ($char -eq ';' -or [int]$char -lt 0x20) { [void]$escaped.Append(('\x{0:x2}' -f [int]$char)) }
            else { [void]$escaped.Append($char) }
        }
        [Console]::Write("$([char]27)]633;E;$escaped;$Global:__SoloNonce$([char]7)$([char]27)]133;C$([char]7)")
        $line
    }
}
