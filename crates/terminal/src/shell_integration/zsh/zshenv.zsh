# Solo shell integration for zsh: Solo starts zsh with ZDOTDIR pointing here,
# and each startup file sources the user's own with their ZDOTDIR restored.
__solo_zdotdir=$ZDOTDIR
ZDOTDIR=${SOLO_USER_ZDOTDIR:-$HOME}
if [[ -f $ZDOTDIR/.zshenv ]]; then
    . $ZDOTDIR/.zshenv
fi
SOLO_USER_ZDOTDIR=$ZDOTDIR
ZDOTDIR=$__solo_zdotdir
