#!/bin/sh
# Records demo.tape and renders .github/assets/demo.gif, for README.md,
# and .github/assets/demo.mp4.
#
# vhs renders only the terminal's frames here, and this script pads and
# encodes them itself, as vhs 0.12 does not run ffmpeg 9.
set -e
cd "$(dirname "$0")/.."
rm -rf demo/frames && mkdir -p .github/assets
vhs demo/demo.tape
# Width, height, and background of demo.tape's window and theme: 16:9.
pad="pad=1280:720:(ow-iw)/2:(oh-ih)/2:color=0x1e1e2e"
in="-framerate 50 -i demo/frames/frame-text-%05d.png -framerate 50 -i demo/frames/frame-cursor-%05d.png"
ffmpeg -loglevel error -y $in -filter_complex \
  "[0][1]overlay,$pad,fps=20,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=none" \
  .github/assets/demo.gif
ffmpeg -loglevel error -y $in -filter_complex "[0][1]overlay,$pad,format=yuv420p" \
  -c:v libx264 -crf 20 -movflags +faststart .github/assets/demo.mp4
rm -rf demo/frames && mkdir -p .github/assets
ls -lh .github/assets/demo.gif .github/assets/demo.mp4
