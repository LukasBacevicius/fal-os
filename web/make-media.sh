#!/bin/sh
# Generates the sample clips used by `?realvideo` into web/media (needs ffmpeg).
# 640x360 H.264 yuv420p, 30 fps, 10 s, looping-friendly lavfi test sources.
set -e
out="$(dirname "$0")/media"
mkdir -p "$out"
i=0
for src in \
  "testsrc2=size=640x360:rate=30" \
  "mandelbrot=size=640x360:rate=30" \
  "smptebars=size=640x360:rate=30,hue=h=36*t" \
  "life=size=640x360:rate=30:mold=10:ratio=0.08:death_color=#202040:life_color=#40c0ff"; do
  ffmpeg -loglevel error -y -f lavfi -i "$src" -t 10 -c:v libx264 -crf 26 -pix_fmt yuv420p -movflags +faststart "$out/clip$i.mp4"
  i=$((i+1))
done
ls -la "$out"
