#!/bin/sh
# Installs Arro for the current user from the unpacked archive:
#
#   ./install.sh              copy into ~/.local/share/arro and add the menu entry
#   ./install.sh DIR          copy into DIR instead
#   ./install.sh --uninstall  remove the folder, the link, the menu entry and the icons
#
# Nothing here needs root and nothing outside the home folder of the user is written.
set -eu

usage() {
    cat <<'EOF'
usage: install.sh [--uninstall] [DIR]

  DIR           where the program and its files go, ~/.local/share/arro by default
  --uninstall   remove an installation made by this script
EOF
}

uninstall=false
target=
for argument in "$@"; do
    case $argument in
        --uninstall) uninstall=true ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*)
            echo "install.sh: unknown option $argument" >&2
            exit 2
            ;;
        *)
            if [ -n "$target" ]; then
                echo "install.sh: only one folder can be given" >&2
                exit 2
            fi
            target=$argument
            ;;
    esac
done

if [ -z "${HOME:-}" ]; then
    echo "install.sh: HOME is not set" >&2
    exit 1
fi
data=${XDG_DATA_HOME:-$HOME/.local/share}
applications=$data/applications
icons=$data/icons/hicolor
link=$HOME/.local/bin/arro
target=${target:-$data/arro}
while [ "$target" != "/" ] && [ "$target" != "${target%/}" ]; do
    target=${target%/}
done
case $target in
    /*) ;;
    *) target=$(pwd)/$target ;;
esac

# The menu and the icon cache are refreshed only where the tools exist; without them
# the desktop picks the files up on its own.
refresh() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$applications" >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t "$icons" >/dev/null 2>&1 || true
    fi
}

if $uninstall; then
    rm -f "$applications/arro.desktop"
    for icon in "$icons"/*/apps/arro.png; do
        if [ -f "$icon" ] || [ -L "$icon" ]; then
            rm -f "$icon"
        fi
    done
    if [ -L "$link" ]; then
        rm -f "$link"
    elif [ -e "$link" ]; then
        echo "install.sh: $link is not a link, left in place" >&2
    fi
    # A folder is removed only when it is an installation of Arro: a wrong DIR must not
    # take anything else with it, and neither must a folder equal to the home folder.
    if [ "$target" = "$HOME" ] || [ "$target" = "/" ]; then
        echo "install.sh: refusing to remove $target" >&2
    elif [ -d "$target" ]; then
        if [ -f "$target/arro" ] && [ -d "$target/tools" ]; then
            rm -rf "$target"
        else
            echo "install.sh: $target is not an Arro folder, left in place" >&2
        fi
    fi
    refresh
    echo "Removed $target"
    exit 0
fi

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ ! -f "$here/arro" ]; then
    echo "install.sh: $here/arro is missing; run this script inside the unpacked archive" >&2
    exit 1
fi
if [ -e "$link" ] && [ ! -L "$link" ]; then
    echo "install.sh: $link exists and is not a link; not replacing it" >&2
    exit 1
fi
if [ "$target" = "/" ] || [ "$target" = "$HOME" ]; then
    echo "install.sh: refusing to install into $target" >&2
    exit 1
fi
if [ -L "$target" ]; then
    echo "install.sh: $target is a link; give the folder it points to" >&2
    exit 1
fi
if [ -d "$target" ] && [ -n "$(ls -A "$target")" ] && [ ! -f "$target/arro" ]; then
    echo "install.sh: $target is not empty and holds no arro program; choose an empty folder" >&2
    exit 1
fi

mkdir -p "$(dirname -- "$target")" "$applications" "$(dirname -- "$link")"
source=$(cd "$here" && pwd -P)
installed="$(cd "$(dirname -- "$target")" && pwd -P)/$(basename "$target")"
# A copy into the archive itself would copy the copy, and a folder that contains the
# archive would be moved away while it is read.
case $installed in
    "$source"/*)
        echo "install.sh: $target is inside the archive; choose another folder" >&2
        exit 1
        ;;
esac
case $source in
    "$installed"/*)
        echo "install.sh: $here is inside $target; choose another folder" >&2
        exit 1
        ;;
esac
mkdir -p "$target"
if [ "$source" != "$installed" ]; then
    # The files are copied into a folder of their own first, so a copy that stops
    # halfway leaves the previous installation in place.
    stage=$target.new.$$
    old=$target.old.$$
    rm -rf "$stage" "$old"
    mkdir -p "$stage"
    cp -R "$here/." "$stage/"
    chmod 755 "$stage/arro"
    if [ -d "$target" ]; then
        mv "$target" "$old"
    fi
    mv "$stage" "$target"
    rm -rf "$old"
fi

chmod 755 "$target/arro"
ln -sfn "$target/arro" "$link"

# The desktop entry names the folder of the program. A path with spaces, ampersands or
# backslashes is escaped for the pattern of sed and for the quotes of the entry.
escape=$(printf '%s' "$target" | sed -e 's/[\\&|]/\\&/g' -e 's/"/\\"/g')
if [ ! -f "$here/arro.desktop" ]; then
    echo "install.sh: $here/arro.desktop is missing" >&2
    exit 1
fi
sed "s|@ARRO_DIR@|$escape|g" "$here/arro.desktop" > "$applications/arro.desktop"
chmod 644 "$applications/arro.desktop"

# Every render the archive carries goes to the size folder of the hicolor theme.
for source in "$here"/arro-*.png; do
    if [ ! -f "$source" ]; then
        continue
    fi
    name=${source##*/}
    size=${name#arro-}
    size=${size%.png}
    case $size in
        '' | *[!0-9]*) continue ;;
    esac
    folder=$icons/${size}x${size}/apps
    mkdir -p "$folder"
    cp "$source" "$folder/arro.png"
    chmod 644 "$folder/arro.png"
done

refresh
echo "Installed Arro in $target"
case :$PATH: in
    *:$HOME/.local/bin:*) ;;
    *) echo "Add $HOME/.local/bin to PATH to start arro by its name." >&2 ;;
esac
