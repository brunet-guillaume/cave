#!/bin/bash
# Attaches the legal notices of its components to the runtime, in
# <runtime>/NOTICES. The licenses (LGPL for Wine and GStreamer, MIT…)
# require them to accompany every copy, along with where to find the
# sources. Those of DXMT and D3DMetal are put in place by add-dxmt.sh and
# add-d3dmetal.sh, in each layer's folder.
#
#   scripts/add-notices.sh <runtime folder> [CrossOver sources version]
#
# Called at the end of build-wine.sh; can also be run alone on an existing runtime.

set -euo pipefail

DEST=$1
VERSION=${2:-26.3.0}
GST_VERSION=${GST_VERSION:-1.28.8}
WORK=${CAVE_BUILD:-$HOME/Library/Caches/cave-build}
SRC=$WORK/src/crossover-$VERSION/sources/wine
N=$DEST/NOTICES

[[ -x $DEST/bin/wine ]] || { echo "runtime not found: $DEST" >&2; exit 1; }
rm -rf "$N"
mkdir -p "$N/wine"

# Wine's license: from the sources, or from the archive if they were deleted.
if [[ -f $SRC/COPYING.LIB ]]; then
    cp "$SRC/COPYING.LIB" "$SRC/LICENSE" "$SRC/AUTHORS" "$N/wine/"
else
    tar -xzf "$WORK/dl/crossover-sources-$VERSION.tar.gz" -C "$N/wine" --strip-components 2 \
        sources/wine/COPYING.LIB sources/wine/LICENSE sources/wine/AUTHORS
fi

version_of() { # <folder> <prefix>: an add-on's version, from its folder
    local dir
    dir=$(cd "$DEST/share/wine/$1" 2>/dev/null && ls -d "$2"-* 2>/dev/null | head -1)
    dir=${dir#"$2"-}
    echo "${dir%-x86*}"
}
mono=$(version_of mono wine-mono)
gecko=$(version_of gecko wine-gecko)
libs=$(cd "$DEST/lib" && ls ./*.dylib | sed -E 's|^\./||; s/(\.[0-9]+)*\.dylib$//' | sort -u | tr '\n' ' ')

cat > "$N/README.txt" <<EOF
cave's wine-cx-$VERSION runtime: legal notices
===============================================

This runtime comes with cave (https://github.com/brunet-guillaume/cave), a
personal, free and non-commercial project. cave is not affiliated with
CodeWeavers; CrossOver is a trademark of CodeWeavers, Inc.

Components, licenses and sources:

- Wine, built from the CrossOver $VERSION sources published by CodeWeavers:
  LGPL 2.1 or later (wine/COPYING.LIB, wine/LICENSE, authors in
  wine/AUTHORS).
  Sources: https://media.codeweavers.com/pub/crossover/source/crossover-sources-$VERSION.tar.gz
  cave's changes: the patches/wine folder of the cave repository.

- GStreamer $GST_VERSION (lib/libgst*, lib/gstreamer-1.0): LGPL 2.1 or later,
  same text as wine/COPYING.LIB.
  Sources: https://gstreamer.freedesktop.org/src/

- Wine Mono $mono (share/wine/mono): MIT, with components under other free
  licenses.
  Sources: https://dl.winehq.org/wine/wine-mono/$mono/wine-mono-$mono-src.tar.xz

- Wine Gecko $gecko (share/wine/gecko): Mozilla Public License 2.0.
  Sources: https://dl.winehq.org/wine/wine-gecko/$gecko/wine-gecko-$gecko-src.tar.xz

- x86_64 libraries taken from Wine Devel, built by Gcenx
  (https://github.com/Gcenx/macOS_Wine_builds, macports-wine recipes), each
  under its project's free license (gnutls and glib: LGPL; freetype: FTL;
  SDL2: zlib; MoltenVK: Apache 2.0; ICU: Unicode license; libxml2: MIT…):
  $libs

- winetricks (lib/winetricks), if present: LGPL 2.1 or later, text at the top
  of the script. Sources: https://github.com/Winetricks/winetricks
  It comes with cabextract: GPL 3 (lib/winetricks/COPYING.cabextract).
  Sources: https://www.cabextract.org.uk/

- DXMT (lib/dxmt), if present: license in lib/dxmt/LICENSE.

- D3DMetal (lib/apple_gptk), if present: © Apple Inc., Game Porting Toolkit.
  License and acknowledgements: lib/apple_gptk/License.rtf and
  lib/apple_gptk/Acknowledgements.rtf. Redistributed for non-commercial
  purposes only (sections 2A(iii) and 2C of the license), for Apple hardware.
EOF
echo "Legal notices in $N"
