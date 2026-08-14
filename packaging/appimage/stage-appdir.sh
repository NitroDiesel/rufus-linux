#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 GUI_BINARY APPDIR" >&2
    exit 2
fi

binary=$1
appdir=$2
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)

if [ ! -f "$binary" ] || [ ! -x "$binary" ]; then
    echo "GUI binary is missing or not executable: $binary" >&2
    exit 1
fi
if [ -e "$appdir" ]; then
    echo "AppDir already exists: $appdir" >&2
    exit 1
fi

install -d \
    "$appdir/usr/bin" \
    "$appdir/usr/share/applications" \
    "$appdir/usr/share/metainfo" \
    "$appdir/usr/share/licenses/rufus-linux" \
    "$appdir/usr/share/doc/rufus-linux"

install -m0755 "$binary" "$appdir/usr/bin/rufus-linux"
install -m0755 "$script_dir/AppRun" "$appdir/AppRun"
install -m0644 \
    "$repo_root/packaging/desktop/io.github.nitrodiesel.rufus-linux.desktop" \
    "$appdir/usr/share/applications/io.github.nitrodiesel.rufus-linux.desktop"
install -m0644 \
    "$repo_root/packaging/metainfo/io.github.nitrodiesel.rufus-linux.metainfo.xml" \
    "$appdir/usr/share/metainfo/io.github.nitrodiesel.rufus-linux.metainfo.xml"
ln -s io.github.nitrodiesel.rufus-linux.metainfo.xml \
    "$appdir/usr/share/metainfo/io.github.nitrodiesel.rufus-linux.appdata.xml"
for size in 16 24 32 48 64 128 256 512; do
    install -Dm0644 "$repo_root/assets/icons/rufus-linux-$size.png" \
        "$appdir/usr/share/icons/hicolor/${size}x${size}/apps/io.github.nitrodiesel.rufus-linux.png"
done
install -m0644 "$repo_root/LICENSE.txt" \
    "$appdir/usr/share/licenses/rufus-linux/LICENSE.txt"
install -m0644 "$repo_root/assets/icons/LICENSE.txt" \
    "$appdir/usr/share/licenses/rufus-linux/ICON-LICENSE.txt"
install -m0644 "$repo_root/README.md" "$repo_root/THIRD_PARTY.md" \
    "$appdir/usr/share/doc/rufus-linux/"

ln -s usr/share/applications/io.github.nitrodiesel.rufus-linux.desktop \
    "$appdir/io.github.nitrodiesel.rufus-linux.desktop"
ln -s usr/share/icons/hicolor/256x256/apps/io.github.nitrodiesel.rufus-linux.png \
    "$appdir/io.github.nitrodiesel.rufus-linux.png"
ln -s io.github.nitrodiesel.rufus-linux.png "$appdir/.DirIcon"
