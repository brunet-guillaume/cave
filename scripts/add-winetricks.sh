#!/bin/bash
# Adds winetricks to a runtime built by cave, with cabextract, which it
# needs to extract Microsoft's packages (Visual C++, fonts…).
#
#   scripts/add-winetricks.sh [runtime]   (default: wine-cx-26.3.0)
#
# winetricks installs the Microsoft components that many applications
# lack (cave components, "Install Components…" in the interface). Both
# tools are kept in <runtime>/lib/winetricks: another Mac has neither
# Homebrew nor cabextract.
#
# Pinned versions and verified checksums: winetricks is a script that will
# be executed, cabextract a program built here.

set -euo pipefail

RUNTIME=${1:-wine-cx-26.3.0}
WINETRICKS=20260125
WINETRICKS_SHA256=431f82fc74000e6c864409f1d8fb495d696c03928808e3e8acffc45179312a7b
CABEXTRACT=1.11
CABEXTRACT_SHA256=b5546db1155e4c718ff3d4b278573604f30dd64c3c5bfd4657cd089b823a3ac6

WORK=${CAVE_BUILD:-$HOME/Library/Caches/cave-build}
RUNTIMES=${CAVE_RUNTIMES:-"$HOME/Library/Application Support/fr.umebrunet.cave/runtimes"}
DEST="$RUNTIMES/$RUNTIME"

[[ -x $DEST/bin/wine ]] || { echo "runtime not found: $DEST" >&2; exit 1; }
mkdir -p "$WORK/dl" "$WORK/src"

fetch() { # <url> <file> <sha256>
    [[ -f $2 ]] || curl -fL -o "$2" "$1"
    [[ $(shasum -a 256 "$2" | awk '{print $1}') == "$3" ]] ||
        { echo "wrong SHA-256 checksum: $2" >&2; rm -f "$2"; exit 1; }
}

script=$WORK/dl/winetricks-$WINETRICKS
fetch "https://raw.githubusercontent.com/Winetricks/winetricks/$WINETRICKS/src/winetricks" \
    "$script" "$WINETRICKS_SHA256"

# cabextract: a native macOS program (arm64) that depends only on the system.
cab=$WORK/src/cabextract-$CABEXTRACT
if [[ ! -x $cab/cabextract ]]; then
    archive=$WORK/dl/cabextract-$CABEXTRACT.tar.gz
    fetch "https://www.cabextract.org.uk/cabextract-$CABEXTRACT.tar.gz" "$archive" "$CABEXTRACT_SHA256"
    rm -rf "$cab"
    tar -xzf "$archive" -C "$WORK/src"
    (cd "$cab" && MACOSX_DEPLOYMENT_TARGET=11.0 ./configure --quiet && make -s)
fi

out=$DEST/lib/winetricks
rm -rf "$out"
mkdir -p "$out"
install -m 755 "$script" "$out/winetricks"
install -m 755 "$cab/cabextract" "$out/cabextract"
# Licenses: winetricks carries its own at the top of the script (LGPL 2.1+),
# cabextract is under GPL 3.
cp "$cab/COPYING" "$out/COPYING.cabextract"
echo "winetricks $WINETRICKS and cabextract $CABEXTRACT installed in $out"
