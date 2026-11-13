#!/bin/sh
# Adds Arro to the application menu of this user. Nothing is installed automatically:
# run this script only if you want the shortcut, and say yes to the question.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ ! -x "$here/arro" ]; then
    echo "install.sh: $here/arro is missing or not executable" >&2
    exit 1
fi

printf 'Add Arro to the application menu? [y/N] ' >&2
answer=
IFS= read -r answer || answer=
case $answer in
    y | Y | yes | YES) ;;
    *)
        echo "Nothing is installed."
        exit 0
        ;;
esac

data=${XDG_DATA_HOME:-$HOME/.local/share}
apps=$data/applications
icons=$data/icons/hicolor/256x256/apps
mkdir -p "$apps" "$icons"

# The desktop entry keeps the folder you unpacked Arro into; move the folder and run
# this script again. Backslashes, ampersands and the sed delimiter are escaped.
escaped=$(printf '%s' "$here" | sed 's/[\\&|]/\\&/g')
sed "s|@ARRO_DIR@|$escaped|g" "$here/arro.desktop" > "$apps/arro.desktop"
chmod 644 "$apps/arro.desktop"
cp "$here/arro-256.png" "$icons/arro.png"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$apps" >/dev/null 2>&1 || true
fi

echo "Installed $apps/arro.desktop"
