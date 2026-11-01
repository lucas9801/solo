# Solo shell integration for fish.
#
# Marks each prompt and command with OSC 133 (A: prompt start, B: command input
# start, C: command executed, D;<exit code>: command finished), reports the
# command line with OSC 633 E and the working directory with OSC 7. Loaded with
# --init-command, after the user's config.

if set -q __solo_integration
    return
end
set -g __solo_integration 1
set -g __solo_command_running

function __solo_preexec --on-event fish_preexec
    set -g __solo_command_running 1
    # OSC 633 E escaping: backslash doubled, ';' and newlines as \xAB.
    set -l command (string join \n -- $argv)
    set command (string replace -a -- '\\' '\\\\' $command)
    set command (string replace -a -- ';' '\x3b' $command)
    set command (string join '\x0a' -- $command)
    printf '\e]633;E;%s\a\e]133;C\a' "$command"
end

function __solo_postexec --on-event fish_postexec
    set -l last_status $status
    if test -n "$__solo_command_running"
        set -g __solo_command_running
        printf '\e]133;D;%s\a' $last_status
    end
end

function __solo_report_cwd --on-variable PWD
    set -l path (string replace -a '%' '%25' -- $PWD)
    set path (string replace -a ' ' '%20' -- $path)
    printf '\e]7;file://%s%s\a' $hostname $path
end
__solo_report_cwd

# Wrap the user's prompt with the A and B marks.
if functions -q fish_prompt
    functions -c fish_prompt __solo_original_prompt
end
function __solo_set_status
    return $argv[1]
end
function fish_prompt
    set -l last_status $status
    printf '\e]133;A\a'
    if test "$SOLO_COMPACT_PROMPT" = 1
        # Solo's input editor shows the full path; keep the scrollback terse.
        printf '%s%s%s %s❯%s ' (set_color brblack) (basename $PWD) (set_color normal) (set_color cyan) (set_color normal)
    else if functions -q __solo_original_prompt
        # Keep $status for the user's prompt.
        __solo_set_status $last_status
        __solo_original_prompt
    end
    printf '\e]133;B\a'
end

printf '\e]633;P;Shell=%s\a' (command -s fish)
