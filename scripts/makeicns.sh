#!/bin/sh
set -e

usage() {
    cat <<EOF
$0: Make a macOS icns file from an SVG with rsvg-convert, ImageMagick and iconutil.

The SVG already follows the Big Sur+ icon template: a 1024x1024 canvas with an
824x824 rounded square and transparent padding, so the Dock/Finder render it
like other first-party macOS apps. Sizes up to 32px come from the simpler
mousebridge-mark.svg next to it, and the menu bar icon from
mousebridge-menubar.svg.

usage: $0 [SVG [ICNS [ICONSET]]]

ARGUMENTS
    SVG     The SVG file to convert
            Defaults to ./lan-mouse-gtk/resources/mousebridge.svg
    ICNS    The icns file to create
            Defaults to ./target/icon.icns
    ICONSET The iconset directory to create
            Defaults to ./target/icon.iconset
            This is just a temporary directory
EOF
}

if [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
    usage
    exit 0
fi

svg="${1:-./lan-mouse-gtk/resources/mousebridge.svg}"
icns="${2:-./target/icon.icns}"
iconset="${3:-./target/icon.iconset}"

set -u

workdir="$(dirname "$iconset")/icon-work"
rm -rf "$iconset" "$workdir"
mkdir -p "$iconset" "$workdir"

small_svg="$(dirname "$svg")/mousebridge-mark.svg"
menubar_svg="$(dirname "$svg")/mousebridge-menubar.svg"

# Render every size from its source, so small sizes stay legible.
for size in 1024 512 256 128 64; do
    rsvg-convert -w "$size" -h "$size" "$svg" -o "$workdir/${size}.png"
done
for size in 32 16; do
    rsvg-convert -w "$size" -h "$size" "$small_svg" -o "$workdir/${size}.png"
done

cp "$workdir/1024.png" "$iconset"/icon_512x512@2x.png
cp "$workdir/512.png"  "$iconset"/icon_512x512.png
cp "$workdir/512.png"  "$iconset"/icon_256x256@2x.png
cp "$workdir/256.png"  "$iconset"/icon_256x256.png
cp "$workdir/256.png"  "$iconset"/icon_128x128@2x.png
cp "$workdir/128.png"  "$iconset"/icon_128x128.png
cp "$workdir/64.png"   "$iconset"/icon_32x32@2x.png
cp "$workdir/32.png"   "$iconset"/icon_32x32.png
cp "$workdir/32.png"   "$iconset"/icon_16x16@2x.png
cp "$workdir/16.png"   "$iconset"/icon_16x16.png

mkdir -p "$(dirname "$icns")"

# Menu bar template icon: flatten all RGB channels to 0 (black) while keeping
# alpha so the artwork reads as a clean silhouette. NSStatusBarButton tints
# template images to match the menu bar appearance in light and dark modes.
menubar_template="$(dirname "$icns")/menubar-template.png"
rsvg-convert -w 44 -h 44 "$menubar_svg" -o "$workdir/menubar-44.png"
magick "$workdir/menubar-44.png" -channel RGB -evaluate set 0 +channel \
    "$menubar_template"

if ! iconutil -c icns "$iconset" -o "$icns"; then
    if ! command -v perl >/dev/null 2>&1; then
        echo "iconutil failed and perl is not available for the fallback icns writer" >&2
        exit 1
    fi

    echo "iconutil rejected the iconset; writing icns directly" >&2
    perl - "$icns" "$iconset" <<'PERL'
use strict;
use warnings;

my ($icns, $iconset) = @ARGV;
my @icons = (
    [ 'icp4', "$iconset/icon_16x16.png" ],
    [ 'ic11', "$iconset/icon_16x16\@2x.png" ],
    [ 'icp5', "$iconset/icon_32x32.png" ],
    [ 'ic12', "$iconset/icon_32x32\@2x.png" ],
    [ 'ic07', "$iconset/icon_128x128.png" ],
    [ 'ic13', "$iconset/icon_128x128\@2x.png" ],
    [ 'ic08', "$iconset/icon_256x256.png" ],
    [ 'ic14', "$iconset/icon_256x256\@2x.png" ],
    [ 'ic09', "$iconset/icon_512x512.png" ],
    [ 'ic10', "$iconset/icon_512x512\@2x.png" ],
);

my $body = '';
for my $icon (@icons) {
    my ($type, $path) = @$icon;
    open my $fh, '<:raw', $path or die "$path: $!";
    local $/;
    my $png = <$fh>;
    $body .= $type . pack('N', length($png) + 8) . $png;
}

open my $out, '>:raw', $icns or die "$icns: $!";
print {$out} 'icns' . pack('N', length($body) + 8) . $body;
PERL
fi
