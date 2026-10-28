#!/bin/sh
# Creates the small repository that demo.tape records agentutils in, and
# removes its forks from before. It is kept out of /tmp, which confine lets
# commands write to.
set -e
dir=${1:-$HOME/wordfreq}
rm -rf "$dir" "$dir".[0-9]* "$(dirname "$dir")/.$(basename "$dir")".[0-9]*.base
mkdir -p "$dir"
cd "$dir"
git init -q -b main
cat > wordfreq.py <<'EOF'
#!/usr/bin/env python3
"""Count how often each word appears in a file."""

import re
import sys
from collections import Counter


def count(text):
    return Counter(re.findall(r"[a-z']+", text.lower()))


def main():
    with open(sys.argv[1]) as f:
        counts = count(f.read())
    for word, n in counts.most_common():
        print(f"{n:6} {word}")


if __name__ == "__main__":
    main()
EOF
cat > README.md <<'EOF'
# wordfreq

Count how often each word appears in a file, and print the words
from the most common to the least:

    python3 wordfreq.py README.md
EOF
git add -A
git commit -qm "Count word frequencies"
# Let Claude edit without asking, so the recording needs no
# keystrokes beyond the prompt. Kept out of git so it is not committed.
mkdir -p .claude
cat > .claude/settings.local.json <<'EOF'
{
  "permissions": {
    "allow": [
      "Edit",
      "Write",
      "Bash(python3:*)",
      "Bash(git status:*)",
      "Bash(git diff:*)",
      "Bash(git log:*)"
    ]
  }
}
EOF
echo .claude/ >> .git/info/exclude
