# Solo shell integration for zsh.
#
# Marks each prompt and command with OSC 133 (A: prompt start, B: command input
# start, C: command executed, D;<exit code>: command finished), reports the
# command line with OSC 633 E and the working directory with OSC 7.

if [[ -n ${__solo_integration-} ]]; then return; fi
__solo_integration=1
# The terminal's nonce goes with the sequences it trusts; programs run here
# don't inherit it.
__solo_nonce=${SOLO_SHELL_NONCE-}
unset SOLO_SHELL_NONCE
__solo_command_running=

__solo_preexec() {
    __solo_command_running=1
    # OSC 633 E escaping: backslash doubled, ';' and newlines as \xAB.
    # Parameters keep the replacements literal.
    local command=$1 bs='\' semicolon='\x3b' newline='\x0a'
    command=${command//$bs/$bs$bs}
    command=${command//;/$semicolon}
    command=${command//$'\n'/$newline}
    printf '\e]633;E;%s;%s\a\e]133;C\a' "$command" "$__solo_nonce"
}

# Runs first, to see the command's exit status.
__solo_precmd_status() {
    local exit_code=$?
    if [[ -n $__solo_command_running ]]; then
        __solo_command_running=
        printf '\e]133;D;%s\a' "$exit_code"
    fi
    local uri_path=${PWD//\%/%25}
    uri_path=${uri_path// /%20}
    printf '\e]7;file://%s%s\a' "${HOST-}" "$uri_path"
    return $exit_code
}

# Runs last, after prompt themes have set PS1.
__solo_precmd_prompt() {
    # Solo's input editor shows the full path; keep the scrollback terse.
    if [[ ${SOLO_COMPACT_PROMPT-} == 1 ]]; then
        PS1='%{%F{8}%}%1~%{%f%} %{%F{cyan}%}❯%{%f%} '
    fi
    if [[ $PS1 != *'133;A'* ]]; then
        PS1=$'%{\e]133;A\a%}'"$PS1"$'%{\e]133;B\a%}'
    fi
}

# Agents' hooks (Claude Code's, Codex's, Copilot's, OpenCode's) write to this terminal.
if [[ -n ${SOLO_CLAUDE_SETTINGS-}${SOLO_CODEX_ARGS-}${SOLO_COPILOT_PLUGIN-}${SOLO_OPENCODE_CONFIG_DIR-} && -n ${TTY-} ]]; then
    export SOLO_AGENT_TTY=$TTY
fi

# Claude Code reports what it's doing through hooks in the settings file Solo
# passes it (see SOLO_CLAUDE_SETTINGS); the hooks write to this terminal.
# A claude function of the user's own stays.
if [[ -n ${SOLO_CLAUDE_SETTINGS-} ]] && (( ! ${+functions[claude]} )); then
    claude() {
        local arg
        # Subcommands, background sessions and the user's own --settings run as typed.
        case ${1-} in
            agents|attach|auth|auto-mode|doctor|gateway|import|install|logs|mcp|plugin|plugins|purge|respawn|rm|setup-token|stop|kill|ultrareview|update|upgrade)
                command claude "$@"
                return
                ;;
        esac
        for arg in "$@"; do
            case $arg in
                --settings | --settings=* | --bg | --background)
                    command claude "$@"
                    return
                    ;;
            esac
        done
        if [[ -r $SOLO_CLAUDE_SETTINGS ]]; then
            command claude --settings "$SOLO_CLAUDE_SETTINGS" "$@"
        else
            command claude "$@"
        fi
    }
fi

# Codex reports what it's doing through hooks Solo adds to the session as
# config overrides, one argument per line in SOLO_CODEX_ARGS.
# A codex function of the user's own stays.
if [[ -n ${SOLO_CODEX_ARGS-} ]] && (( ! ${+functions[codex]} )); then
    codex() {
        # Commands that don't run a session run as typed.
        case ${1-} in
            login|logout|mcp|mcp-server|plugin|completion|update|doctor|sandbox|debug|apply|app|app-server|remote-control|exec-server|cloud|features|help|agents|archive|delete|unarchive|migrate-rollouts|queue)
                command codex "$@"
                return
                ;;
        esac
        command codex "${(@f)SOLO_CODEX_ARGS}" "$@"
    }
fi

# GitHub Copilot CLI reports what it's doing through hooks in a plugin Solo
# loads for the session (see SOLO_COPILOT_PLUGIN).
# A copilot function of the user's own stays.
if [[ -n "${SOLO_COPILOT_PLUGIN-}" ]] && (( ! ${+functions[copilot]} )); then
    copilot() {
        # Commands that don't run a session run as typed.
        case "${1-}" in
            app|login|help|init|config|update|version|sessions|memories|plugin|mcp|skill|instruction|lsp|sandbox|completion)
                command copilot "$@"
                return
                ;;
        esac
        if [[ -d "$SOLO_COPILOT_PLUGIN" ]]; then
            command copilot --plugin-dir "$SOLO_COPILOT_PLUGIN" "$@"
        else
            command copilot "$@"
        fi
    }
fi

# OpenCode reports what it's doing through plugins Solo loads for the session:
# OpenCode 2's from SOLO_OPENCODE_CLI_CONFIG, OpenCode 1's from the folder in
# SOLO_OPENCODE_CONFIG_DIR. Variables of the user's own stay.
# An opencode function of the user's own stays.
if [[ -n "${SOLO_OPENCODE_CONFIG_DIR-}" ]] && (( ! ${+functions[opencode]} )); then
    opencode() {
        (
            if [[ -z "${OPENCODE_CLI_CONFIG_CONTENT+set}" && -n "${SOLO_OPENCODE_CLI_CONFIG-}" ]]; then
                export OPENCODE_CLI_CONFIG_CONTENT="$SOLO_OPENCODE_CLI_CONFIG"
            fi
            if [[ -z "${OPENCODE_CONFIG_DIR+set}" && -d "$SOLO_OPENCODE_CONFIG_DIR" ]]; then
                export OPENCODE_CONFIG_DIR="$SOLO_OPENCODE_CONFIG_DIR"
            fi
            command opencode "$@"
        )
    }
fi

printf '\e]633;P;Shell=%s;%s\a' "${commands[zsh]:-zsh}" "$__solo_nonce"

precmd_functions=(__solo_precmd_status $precmd_functions __solo_precmd_prompt)
preexec_functions+=(__solo_preexec)
