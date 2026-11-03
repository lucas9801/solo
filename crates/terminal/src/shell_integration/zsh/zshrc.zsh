# Solo shell integration for zsh: see .zshenv. Sources the user's .zshrc, then
# hands ZDOTDIR back to the user (so .zlogin and later reads are theirs) and
# loads the integration.
ZDOTDIR=$SOLO_USER_ZDOTDIR
# The global zshrc (e.g. macOS's /etc/zshrc) set HISTFILE from Solo's ZDOTDIR.
if [[ ${HISTFILE-} == $__solo_zdotdir/* ]]; then
    HISTFILE=$ZDOTDIR/.zsh_history
fi
if [[ -f $ZDOTDIR/.zshrc ]]; then
    . $ZDOTDIR/.zshrc
fi
unset SOLO_USER_ZDOTDIR __solo_zdotdir
if [[ -o interactive && -r ${SOLO_ZSH_INTEGRATION-} ]]; then
    . $SOLO_ZSH_INTEGRATION
fi
