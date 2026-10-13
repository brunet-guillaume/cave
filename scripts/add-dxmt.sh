#!/bin/bash
# Installs DXMT (Direct3D 10/11 → Metal) into a runtime built by cave.
#
#   scripts/add-dxmt.sh [runtime] [DXMT version]   (default: wine-cx-26.3.0 v0.80)
#
# As in CrossOver, DXMT is kept separate, in <runtime>/lib/dxmt: bottles
# keep wined3d by default. A bottle that chooses DXMT
# (cave graphics <bottle> dxmt) runs Wine with
# WINEDLLPATH_PREPEND=<runtime>/lib/dxmt, which our ntdll patch puts
# before Wine's DLLs (patches/wine).

set -euo pipefail

RUNTIME=${1:-wine-cx-26.3.0}
DXMT=${2:-v0.80}

WORK=${CAVE_BUILD:-$HOME/Library/Caches/cave-build}
RUNTIMES=${CAVE_RUNTIMES:-"$HOME/Library/Application Support/fr.umebrunet.cave/runtimes"}
DEST="$RUNTIMES/$RUNTIME"
DXMT_DIR=$WORK/src/dxmt-$DXMT

[[ -x $DEST/bin/wine ]] || { echo "runtime not found: $DEST" >&2; exit 1; }
if ! strings -a "$DEST/lib/wine/x86_64-unix/ntdll.so" | grep WINEDLLPATH_PREPEND >/dev/null; then
    echo "this runtime does not know WINEDLLPATH_PREPEND: rebuild it with scripts/build-wine.sh" >&2
    exit 1
fi

# "builtin" release: DLLs marked as built into Wine, plus winemetal.so,
# the Unix part that talks to Metal.
if [[ ! -d $DXMT_DIR ]]; then
    mkdir -p "$WORK/dl" "$DXMT_DIR"
    archive=$WORK/dl/dxmt-$DXMT-builtin.tar.gz
    [[ -f $archive ]] || curl -fL -o "$archive" \
        "https://github.com/3Shain/dxmt/releases/download/$DXMT/dxmt-$DXMT-builtin.tar.gz"
    tar -xzf "$archive" -C "$DXMT_DIR" --strip-components 1
fi

rm -rf "$DEST/lib/dxmt"
mkdir -p "$DEST/lib/dxmt"
cp -R "$DXMT_DIR/"{x86_64-windows,i386-windows,x86_64-unix} "$DEST/lib/dxmt/"
# License of this exact version (MIT up to v0.80, LGPL afterwards):
# it must accompany every copy.
license=$WORK/dl/dxmt-$DXMT-LICENSE
[[ -f $license ]] || curl -fsSL -o "$license" "https://raw.githubusercontent.com/3Shain/dxmt/$DXMT/LICENSE"
cp "$license" "$DEST/lib/dxmt/LICENSE"
echo "DXMT $DXMT installed in $DEST/lib/dxmt"
