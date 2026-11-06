#!/bin/sh
# Copy a Hyprland theme file into Grimoir's config directory.
#
# Grimoir understands both its own theme schema and the Hyprland theme schema
# (colors.background / colors.foreground), so this is enough to make the app
# follow a generated desktop theme. Re-run this script whenever theme-apply
# rewrites the source file; Grimoir hot-reloads the destination.
set -eu

src=${1:-${GRIMOIR_HYPR_THEME:-$HOME/.local/state/hypr/theme.json}}
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
dst=${2:-$config_home/grimoir/theme.json}

if [ ! -f "$src" ]; then
    printf 'theme source not found: %s\n' "$src" >&2
    exit 1
fi

mkdir -p "$(dirname -- "$dst")"
tmp="$dst.tmp.$$"
cp "$src" "$tmp"
mv "$tmp" "$dst"
printf 'Grimoir theme updated from %s\n' "$src"
