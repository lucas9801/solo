# Solo shell integration for zsh: see .zshenv.
__solo_zdotdir=$ZDOTDIR
ZDOTDIR=$SOLO_USER_ZDOTDIR
if [[ -f $ZDOTDIR/.zprofile ]]; then
    . $ZDOTDIR/.zprofile
fi
SOLO_USER_ZDOTDIR=$ZDOTDIR
ZDOTDIR=$__solo_zdotdir
