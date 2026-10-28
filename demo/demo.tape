# Records an agent deleting everything in a fork of a repository, which
# leaves the repository as it was.
#
#   demo/record.sh
#
# vhs writes only frames; record.sh turns them into .github/assets/demo.*.

Output demo/frames/

Require fsfork
Require confine
Require fsdiff

Set Shell zsh
Set FontSize 20
Set Width 1280
Set Height 720
Set Padding 28
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms
Set CursorBlink false

Hide
Type "./demo/setup.sh ~/wordfreq && export ZDOTDIR=$PWD/demo/zsh && cd ~" Enter
Type "export PS1='%F{magenta}%1~%f $ ' && clear" Enter
Sleep 1s
Show

Type "fsfork wordfreq" Sleep 300ms Enter
Sleep 1s
Type "confine -d wordfreq.1" Sleep 300ms Enter
Sleep 1s
Type "rm -rf *" Sleep 300ms Enter
Sleep 800ms
Type "y"
Sleep 1s
Type "ls" Sleep 300ms Enter
Sleep 1s
Type "exit" Sleep 300ms Enter
Sleep 800ms
Type "ls wordfreq" Sleep 300ms Enter
Sleep 1.2s
Type "fsdiff --stat wordfreq.1" Sleep 300ms Enter
Sleep 3s
