# Records Claude Code changing a fork, and fsdiff applying the change.
#
#   demo/record.sh
#
# vhs writes only frames; record.sh turns them into .github/assets/demo.*.
# Claude's run is live, so each recording differs a little.

Output demo/frames/

Require fsfork
Require confine
Require fsdiff
Require claude

Set Shell zsh
Set FontSize 20
Set Width 1280
Set Height 720
Set Padding 28
Set Theme "Catppuccin Mocha"
Set TypingSpeed 25ms
Set CursorBlink false

Hide
Type "./demo/setup.sh ~/wordfreq && cd ~/wordfreq" Enter
Type "unset CLAUDECODE $(env | grep -o '^CLAUDE_CODE_[A-Z_]*')" Enter
Type "export PS1='%F{magenta}wordfreq%f $ ' && clear" Enter
Sleep 1s
Show

# record.sh speeds this up from here...
Type `confine -w ~/.claude $(fsfork) claude "Add a --top N flag to wordfreq.py"` Sleep 300ms Enter
Wait+Screen@5m /· done/
# ...to here. What follows must last TAIL seconds in record.sh.
Sleep 2s
Ctrl+C
Ctrl+C
Hide
Wait /^wordfreq \$ *$/
Type "clear" Enter
Sleep 300ms
Show

Type "fsdiff --stat ../wordfreq.1" Sleep 200ms Enter
Sleep 1.2s
Type "fsdiff ../wordfreq.1 | git apply && git status --short" Sleep 200ms Enter
Sleep 2.5s
