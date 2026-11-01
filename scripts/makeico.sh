#!/bin/sh
# Make the Windows icon embedded by build.rs, with rsvg-convert and ImageMagick.
# Run it again after changing the SVGs, and commit the result.
set -e

res="./lan-mouse-gtk/resources"
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

for size in 16 24 32; do
    rsvg-convert -w "$size" -h "$size" "$res/mousebridge-mark.svg" -o "$workdir/$size.png"
done
for size in 48 64 128 256; do
    rsvg-convert -w "$size" -h "$size" "$res/mousebridge.svg" -o "$workdir/$size.png"
done

magick "$workdir/16.png" "$workdir/24.png" "$workdir/32.png" "$workdir/48.png" \
    "$workdir/64.png" "$workdir/128.png" "$workdir/256.png" "$res/mousebridge.ico"
