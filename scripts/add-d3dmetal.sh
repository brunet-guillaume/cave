#!/bin/bash
# Installs D3DMetal (Apple's Game Porting Toolkit: Direct3D 11/12 → Metal)
# into a runtime built by cave.
#
#   scripts/add-d3dmetal.sh <Game_Porting_Toolkit.dmg> [runtime]   (default: wine-cx-26.3.0)
#   scripts/add-d3dmetal.sh <d3dmetal-….tar.xz> [runtime]
#
# Apple distributes D3DMetal behind a developer account (license: personal
# use, for developing and testing), so it cannot be downloaded here. The
# .dmg contains a second .dmg, "Evaluation environment for Windows games",
# whose redist/lib/ is copied as is: that is what Apple's Read Me does for
# CrossOver (lib64/apple_gptk). We put it in <runtime>/lib/apple_gptk.
#
# The .tar.xz form is that apple_gptk folder, notices included, archived
# once from an installed runtime: the release workflow gets it from a draft
# Release of the repository, since it cannot sign in to Apple.
#
# A bottle set to d3dmetal (cave graphics <bottle> d3dmetal) runs Wine
# with WINEDLLPATH_PREPEND=<runtime>/lib/apple_gptk/wine and
# CX_APPLEGPTK_LIBD3DSHARED_PATH, which ntdll loads at startup.

set -euo pipefail

DMG=${1:?usage: $0 <Game_Porting_Toolkit.dmg | d3dmetal-….tar.xz> [runtime]}
RUNTIME=${2:-wine-cx-26.3.0}

WORK=${CAVE_BUILD:-$HOME/Library/Caches/cave-build}
RUNTIMES=${CAVE_RUNTIMES:-"$HOME/Library/Application Support/fr.umebrunet.cave/runtimes"}
DEST="$RUNTIMES/$RUNTIME"
MNT=$WORK/mnt

[[ -x $DEST/bin/wine ]] || { echo "runtime not found: $DEST" >&2; exit 1; }
if ! strings -a "$DEST/lib/wine/x86_64-unix/ntdll.so" | grep WINEDLLPATH_PREPEND >/dev/null; then
    echo "this runtime does not know WINEDLLPATH_PREPEND: rebuild it with scripts/build-wine.sh" >&2
    exit 1
fi

if [[ $DMG == *.tar.xz ]]; then
    rm -rf "$DEST/lib/apple_gptk"
    tar -xJf "$DMG" -C "$DEST/lib"
    [[ -f $DEST/lib/apple_gptk/License.rtf && -f $DEST/lib/apple_gptk/external/libd3dshared.dylib ]] ||
        { echo "not a D3DMetal archive (apple_gptk/ with License.rtf): $DMG" >&2; exit 1; }
    echo "D3DMetal installed in $DEST/lib/apple_gptk"
    exit 0
fi

mounted=()
cleanup() {
    local m
    for m in "${mounted[@]+"${mounted[@]}"}"; do hdiutil detach "$m" -quiet || true; done
}
trap cleanup EXIT

# hdiutil attach is deprecated, but its replacement (diskutil image attach)
# ignores --mountPoint and mounts in /Volumes, visible in the Finder.
mount_dmg() { # <dmg> <mount point>
    mkdir -p "$2"
    hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$2" "$1" < /dev/null >/dev/null
    mounted=("$2" "${mounted[@]+"${mounted[@]}"}") # unmounted in reverse order
}

mount_dmg "$DMG" "$MNT/gptk"
src=$MNT/gptk
if [[ ! -d $src/redist/lib ]]; then
    inner=$(find "$src" -maxdepth 1 -name 'Evaluation environment*.dmg' | head -1)
    [[ -n $inner ]] || { echo "neither redist/lib nor “Evaluation environment” in $DMG" >&2; exit 1; }
    mount_dmg "$inner" "$MNT/eval"
    src=$MNT/eval
fi

rm -rf "$DEST/lib/apple_gptk"
mkdir -p "$DEST/lib/apple_gptk"
ditto "$src/redist/lib" "$DEST/lib/apple_gptk"
# Apple's license requires its notices to accompany every copy
# (section 2A): they travel with the runtime.
for notice in License.rtf Acknowledgements.rtf; do
    [[ -f $src/$notice ]] && cp "$src/$notice" "$DEST/lib/apple_gptk/"
done
echo "D3DMetal installed in $DEST/lib/apple_gptk"
