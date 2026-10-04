#!/usr/bin/env bash
# Regenerates the theme fixtures: every stock Omarchy palette as <name>/colors.toml, what
# omarchy-theme-color --all resolves it to as <name>/resolved.tsv, and the shell.toml Omarchy
# generated for the active theme. Reads system files only; needs an installed Omarchy.
# Usage: bash tests/theme/regenerate.sh
set -euo pipefail
out=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
themes=/usr/share/omarchy/themes
script=/usr/share/omarchy/bin/omarchy-theme-color
version=$(pacman -Q omarchy | cut -d' ' -f2)
for dir in "$themes"/*/; do
    name=$(basename "$dir")
    mkdir -p "$out/$name"
    {
        echo "# $name from Omarchy $version ($themes/$name/colors.toml), MIT licence."
        cat "$dir/colors.toml"
    } >"$out/$name/colors.toml"
    {
        echo "# omarchy-theme-color --all for colors.toml, Omarchy $version."
        "$script" --file "$out/$name/colors.toml" --all
    } >"$out/$name/resolved.tsv"
done
current="$HOME/.local/state/omarchy/current"
{
    echo "# shell.toml that Omarchy $version generated for $(cat "$current/theme.name")."
    cat "$current/theme/shell.toml"
} >"$out/shell.toml"
find "$out" -mindepth 1 -maxdepth 1 -type d | wc -l
