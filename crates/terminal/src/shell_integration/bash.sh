# Solo shell integration for bash.
#
# Marks each prompt and command with OSC 133 (A: prompt start, B: command input
# start, C: command executed, D;<exit code>: command finished), reports the
# command line with OSC 633 E and the working directory with OSC 7. Loaded with
# --rcfile in place of ~/.bashrc, so it sources that first.

if [[ -n "${__solo_integration-}" ]]; then return; fi
__solo_integration=1

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
    printf '\e]633;E;%s\a\e]133;C\a' "$command"
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
    # The user's prompt command may have replaced PS1; wrap it again.
    if [[ "$PS1" != *'133;A'* ]]; then
        PS1='\[\e]133;A\a\]'"$PS1"'\[\e]133;B\a\]'
    fi
    __solo_at_prompt=1
    return "$status"
}

printf '\e]633;P;Shell=bash\a'

unset PROMPT_COMMAND
PROMPT_COMMAND=__solo_prompt_command
# Another DEBUG trap means the user has their own preexec; keep it and go without
# command lines and C marks.
if [[ -z "$(trap -p DEBUG)" ]]; then
    trap '__solo_preexec' DEBUG
fi
