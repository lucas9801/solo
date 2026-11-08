# Solo shell integration for bash.
#
# Marks each prompt and command with OSC 133 (A: prompt start, B: command input
# start, C: command executed, D;<exit code>: command finished), reports the
# command line with OSC 633 E and the working directory with OSC 7. Loaded with
# --rcfile in place of ~/.bashrc, so it sources that first.

if [[ -n "${__solo_integration-}" ]]; then return; fi
__solo_integration=1
# The terminal's nonce goes with the sequences it trusts; programs run here
# don't inherit it.
__solo_nonce=${SOLO_SHELL_NONCE-}
unset SOLO_SHELL_NONCE

if [[ -r ~/.bashrc ]]; then
    . ~/.bashrc
fi

__solo_original_prompt_command=
if [[ -n "${PROMPT_COMMAND[*]-}" ]]; then
    printf -v __solo_original_prompt_command '%s;' "${PROMPT_COMMAND[@]}"
fi
__solo_at_prompt=
__solo_command_running=
__solo_has_cygpath=
if command -v cygpath >/dev/null 2>&1; then
    __solo_has_cygpath=1
fi

__solo_preexec() {
    [[ -n "$__solo_at_prompt" ]] || return
    [[ "$BASH_COMMAND" == __solo_prompt_command* ]] && return
    [[ -n "${COMP_LINE-}" ]] && return
    __solo_at_prompt=
    __solo_command_running=1

    local command
    command=$(HISTTIMEFORMAT= builtin history 1)
    if [[ "$command" =~ ^[[:space:]]*[0-9]+[*[:space:]]+(.*)$ ]]; then
        command=${BASH_REMATCH[1]}
    else
        command=$BASH_COMMAND
    fi
    # OSC 633 E escaping: backslash doubled, ';' and newlines as \xAB.
    command=${command//\\/\\\\}
    command=${command//;/\\x3b}
    command=${command//$'\n'/\\x0a}
    printf '\e]633;E;%s;%s\a\e]133;C\a' "$command" "$__solo_nonce"
}

__solo_prompt_command() {
    local status=$?
    if [[ -n "$__solo_command_running" ]]; then
        __solo_command_running=
        printf '\e]133;D;%s\a' "$status"
    fi

    local path=$PWD
    if [[ -n "$__solo_has_cygpath" ]]; then
        path=/$(cygpath -m "$PWD")
    fi
    path=${path//%/%25}
    path=${path// /%20}
    printf '\e]7;file://%s%s\a' "${HOSTNAME-}" "$path"

    if [[ -n "$__solo_original_prompt_command" ]]; then
        (exit "$status")
        eval "$__solo_original_prompt_command"
    fi
    # Solo's input editor shows the full path; keep the scrollback terse.
    if [[ "${SOLO_COMPACT_PROMPT-}" == 1 ]]; then
        PS1='\[\e[2m\]\W\[\e[0m\] \[\e[36m\]❯\[\e[0m\] '
    fi
    # The user's prompt command may have replaced PS1; wrap it again.
    if [[ "$PS1" != *'133;A'* ]]; then
        PS1='\[\e]133;A\a\]'"$PS1"'\[\e]133;B\a\]'
    fi
    __solo_at_prompt=1
    return "$status"
}

# Tell the terminal which bash this is (a Windows path in Git Bash), so its
# input editor can run completion generators with the same one.
__solo_shell_path=$BASH
if [[ -n "$__solo_has_cygpath" ]]; then
    __solo_shell_path=$(cygpath -w "$BASH")
fi
__solo_shell_path=${__solo_shell_path//\\/\\\\}
printf '\e]633;P;Shell=%s;%s\a' "${__solo_shell_path//;/\\x3b}" "$__solo_nonce"
unset __solo_shell_path

# Agents' hooks (Claude Code's, Codex's) write to this terminal.
if [[ -n "${SOLO_CLAUDE_SETTINGS-}${SOLO_CODEX_ARGS-}" ]]; then
    if [[ -r /proc/$$/winpid ]]; then
        # Git Bash: the hooks attach to this shell's Windows console.
        export SOLO_AGENT_CONSOLE_PID=$(</proc/$$/winpid)
    elif __solo_tty=$(tty 2>/dev/null); then
        export SOLO_AGENT_TTY=$__solo_tty
    fi
    unset __solo_tty
fi

# Claude Code reports what it's doing through hooks in the settings file Solo
# passes it (see SOLO_CLAUDE_SETTINGS); the hooks write to this terminal.
# A claude function of the user's own stays.
if [[ -n "${SOLO_CLAUDE_SETTINGS-}" ]] && ! declare -F claude >/dev/null; then
    claude() {
        local arg
        # Subcommands, background sessions and the user's own --settings run as typed.
        case "${1-}" in
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
        if [[ -r "$SOLO_CLAUDE_SETTINGS" ]]; then
            command claude --settings "$SOLO_CLAUDE_SETTINGS" "$@"
        else
            command claude "$@"
        fi
    }
fi

# Codex reports what it's doing through hooks Solo adds to the session as
# config overrides, one argument per line in SOLO_CODEX_ARGS.
# A codex function of the user's own stays.
if [[ -n "${SOLO_CODEX_ARGS-}" ]] && ! declare -F codex >/dev/null; then
    codex() {
        # Commands that don't run a session run as typed.
        case "${1-}" in
            login|logout|mcp|mcp-server|plugin|completion|update|doctor|sandbox|debug|apply|app|app-server|remote-control|exec-server|cloud|features|help|agents|archive|delete|unarchive|migrate-rollouts|queue)
                command codex "$@"
                return
                ;;
        esac
        local -a hooks
        IFS=$'\n' read -r -d '' -a hooks <<<"$SOLO_CODEX_ARGS" || true
        command codex "${hooks[@]}" "$@"
    }
fi

unset PROMPT_COMMAND
PROMPT_COMMAND=__solo_prompt_command
# Another DEBUG trap means the user has their own preexec; keep it and go without
# command lines and C marks.
if [[ -z "$(trap -p DEBUG)" ]]; then
    trap '__solo_preexec' DEBUG
fi
