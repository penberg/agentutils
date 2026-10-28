# The shell that confine starts in demo.tape: the prompt names the
# directory, and history is off, as confine does not let zsh lock
# ~/.zsh_history.
PS1='%F{magenta}%1~%f $ '
unset HISTFILE
