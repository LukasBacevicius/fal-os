#!/bin/sh
# Generates local sample media into public/samples (gitignored) for demos and tests:
# images of different aspect ratios, a short video, an audio clip, and a text file.
# Needs ffmpeg.
set -e
out="$(dirname "$0")/../public/samples"
mkdir -p "$out"
ff() { ffmpeg -loglevel error -y "$@"; }
ff -f lavfi -i "gradients=s=1600x1000:c0=0x1d2b64:c1=0xf8cdda:x0=0:y0=0:x1=1600:y1=1000:nb_colors=2,format=rgb24" -frames:v 1 "$out/dusk.jpg"
ff -f lavfi -i "mandelbrot=s=1200x1200:maxiter=200:outer=normalized_iteration_count,format=rgb24" -frames:v 1 "$out/fractal.png"
ff -f lavfi -i "cellauto=s=900x1350:rule=110:scroll=0,scale=900:1350:flags=neighbor,hue=h=200:s=1.5,format=rgb24" -frames:v 1 "$out/poster.png"
ff -f lavfi -i "testsrc2=s=1920x1080,format=rgb24" -frames:v 1 "$out/testcard.png"
ff -f lavfi -i "life=s=640x360:rate=30:mold=10:ratio=0.08:death_color=#202040:life_color=#40c0ff" -t 8 -c:v libx264 -crf 24 -pix_fmt yuv420p -movflags +faststart "$out/life.mp4"
ff -f lavfi -i "sine=frequency=330:duration=6" -f lavfi -i "sine=frequency=495:duration=6" -filter_complex amix=inputs=2 -c:a aac "$out/chord.m4a"
printf '# Brief\n\nMoody dusk palette, 35mm look, no lens flares.\nHero: red car on a coastal road.\nDeliver 4 stills + a 6 s loop.\n' > "$out/brief.md"
cat > "$out/manifest.json" <<EOF
["dusk.jpg", "fractal.png", "poster.png", "testcard.png", "life.mp4", "chord.m4a", "brief.md"]
EOF
ls -la "$out"
