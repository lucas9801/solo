# Solo shell integration for zsh.
#
# Marks each prompt and command with OSC 133 (A: prompt start, B: command input
# start, C: command executed, D;<exit code>: command finished), reports the
# command line with OSC 633 E and the working directory with OSC 7.

if [[ -n ${__solo_integration-} ]]; then return; fi
__solo_integration=1
__solo_command_running=

__solo_preexec() {
    __solo_command_running=1
    # OSC 633 E escaping: backslash doubled, ';' and newlines as \xAB.
    # Parameters keep the replacements literal.
    local command=$1 bs='\' semicolon='\x3b' newline='\x0a'
    command=${command//$bs/$bs$bs}
    command=${command//;/$semicolon}
    command=${command//$'\n'/$newline}
    printf '\e]633;E;%s\a\e]133;C\a' "$command"
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

# Claude Code reports what it's doing through hooks in the settings file Solo
# passes it (see SOLO_CLAUDE_SETTINGS); the hooks write to this terminal.
# A claude function of the user's own stays.
if [[ -n ${SOLO_CLAUDE_SETTINGS-} ]] && (( ! ${+functions[claude]} )); then
    [[ -n ${TTY-} ]] && export SOLO_AGENT_TTY=$TTY
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

printf '\e]633;P;Shell=%s\a' "${commands[zsh]:-zsh}"

precmd_functions=(__solo_precmd_status $precmd_functions __solo_precmd_prompt)
preexec_functions+=(__solo_preexec)
