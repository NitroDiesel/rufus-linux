#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
    echo "usage: $0 APPIMAGE VERSION MAX_GLIBC" >&2
    exit 2
fi

appimage=$(readlink -f "$1")
version=$2
max_glibc=$3
workdir=$(mktemp -d)
trap 'rm -rf -- "$workdir"' EXIT HUP INT TERM

test -x "$appimage"
test -s "$appimage.zsync"
actual=$(APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" --version)
test "$actual" = "rufus-linux $version"
update_information=$("$appimage" --appimage-updateinformation)
test "$update_information" = \
    'gh-releases-zsync|NitroDiesel|rufus-linux|latest|rufus-linux-*-x86_64.AppImage.zsync'

spaced="$workdir/Rufus Linux $version.AppImage"
cp "$appimage" "$spaced"
chmod 0755 "$spaced"
actual=$(APPIMAGE_EXTRACT_AND_RUN=1 "$spaced" --version)
test "$actual" = "rufus-linux $version"

mkdir "$workdir/extract"
(cd "$workdir/extract" && "$appimage" --appimage-extract >/dev/null)
appdir="$workdir/extract/squashfs-root"

for required in \
    AppRun \
    .DirIcon \
    io.github.nitrodiesel.rufus-linux.desktop \
    io.github.nitrodiesel.rufus-linux.png \
    usr/bin/rufus-linux \
    usr/share/applications/io.github.nitrodiesel.rufus-linux.desktop \
    usr/share/metainfo/io.github.nitrodiesel.rufus-linux.metainfo.xml \
    usr/share/licenses/rufus-linux/LICENSE.txt; do
    test -e "$appdir/$required"
done

test ! -e "$appdir/usr/libexec/rufus-linux-helper"
test ! -e "$appdir/usr/share/polkit-1"
test -z "$(find "$appdir" -xdev -type f -perm /6000 -print -quit)"
test -z "$(find "$appdir" -xdev -type f -perm /0022 -print -quit)"

if readelf -d "$appdir/usr/bin/rufus-linux" | grep -Eq 'RPATH|RUNPATH'; then
    echo "AppImage GUI must not contain RPATH or RUNPATH" >&2
    exit 1
fi

readelf -d "$appdir/usr/bin/rufus-linux" |
    sed -n 's/.*Shared library: \[\(.*\)\]/\1/p' |
    while IFS= read -r library; do
        case "$library" in
            libc.so.6|libdl.so.2|libgcc_s.so.1|libm.so.6|libpthread.so.0|librt.so.1) ;;
            *)
                echo "unexpected linked library in AppImage GUI: $library" >&2
                exit 1
                ;;
        esac
    done

if command -v file >/dev/null 2>&1; then
    file "$appdir/usr/bin/rufus-linux" | grep -q 'x86-64'
fi

required_glibc=$(
    readelf --version-info "$appdir/usr/bin/rufus-linux" |
        grep -o 'GLIBC_[0-9][0-9.]*' |
        sed 's/^GLIBC_//' |
        sort -Vu |
        tail -n 1
)
test -n "$required_glibc"
newest=$(printf '%s\n%s\n' "$required_glibc" "$max_glibc" | sort -V | tail -n 1)
if [ "$newest" != "$max_glibc" ]; then
    echo "GUI requires GLIBC_$required_glibc, newer than GLIBC_$max_glibc" >&2
    exit 1
fi

if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate \
        "$appdir/usr/share/applications/io.github.nitrodiesel.rufus-linux.desktop"
fi
if command -v appstreamcli >/dev/null 2>&1; then
    appstreamcli validate --no-net \
        "$appdir/usr/share/metainfo/io.github.nitrodiesel.rufus-linux.metainfo.xml"
fi

printf 'verified %s (maximum required GLIBC_%s)\n' "$appimage" "$required_glibc"
