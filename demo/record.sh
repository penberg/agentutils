#!/bin/sh
# Records demo.tape and renders .github/assets/demo.gif, for README.md,
# and .github/assets/demo.mp4.
#
# vhs renders only the terminal's frames here, and this script pads and
# encodes them itself, as vhs 0.12 does not run ffmpeg 9.
set -e
cd "$(dirname "$0")/.."
rm -rf demo/frames
vhs demo/demo.tape
# Width, height, and background of demo.tape's window and theme: 16:9.
pad="pad=1280:720:(ow-iw)/2:(oh-ih)/2:color=0x1e1e2e"
# Squeeze Claude's run, from just after "confine" is entered to just before
# its summary, into FAST seconds. HEAD and TAIL are the lengths, in
# seconds, of the parts of the tape before and after it.
HEAD=2.7 TAIL=7.1 FAST=4
n=$(ls demo/frames | grep -c '^frame-text-')
a=$(echo "$HEAD * 50 / 1" | bc)
b=$(echo "$n - $TAIL * 50 / 1" | bc)
k=$(( (b - a) / (FAST * 50) + 1 ))
fast="select='lt(n\\,$a)+gt(n\\,$b)+not(mod(n-$a\\,$k))',setpts=N/50/TB"
in="-framerate 50 -i demo/frames/frame-text-%05d.png -framerate 50 -i demo/frames/frame-cursor-%05d.png"
ffmpeg -loglevel error -y $in -filter_complex \
  "[0][1]overlay,$fast,$pad,fps=20,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=none" \
  .github/assets/demo.gif
ffmpeg -loglevel error -y $in -filter_complex "[0][1]overlay,$fast,$pad,format=yuv420p" \
  -c:v libx264 -crf 20 -movflags +faststart .github/assets/demo.mp4
rm -rf demo/frames
ls -lh .github/assets/demo.gif .github/assets/demo.mp4
