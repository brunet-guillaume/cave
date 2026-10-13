#!/bin/bash
# Archives a runtime for publishing (GitHub Release); cave then installs it
# with `cave runtime install` or from its interface.
#
#   scripts/package-runtime.sh [runtime name] [output folder]
#   (defaults: the most recent wine-cx, target/release/bundle)
#
# Produces <name>.tar.xz and its SHA-256 checksum. The GStreamer cache is not
# included: it contains paths from this Mac and rebuilds itself.

set -euo pipefail

RUNTIMES=${CAVE_RUNTIMES:-"$HOME/Library/Application Support/fr.umebrunet.cave/runtimes"}
NAME=${1:-$(cd "$RUNTIMES" && ls -d wine-cx-* | sort -V | tail -1)}
OUT=${2:-$(cd "$(dirname "$0")/.." && pwd)/target/release/bundle}
SRC=$RUNTIMES/$NAME

[[ -x $SRC/bin/wine ]] || { echo "runtime not found: $SRC" >&2; exit 1; }
# The licenses require their notices to accompany every copy.
[[ -f $SRC/NOTICES/README.txt ]] ||
    { echo "legal notices missing: scripts/add-notices.sh \"$SRC\"" >&2; exit 1; }
[[ ! -d $SRC/lib/apple_gptk || -f $SRC/lib/apple_gptk/License.rtf ]] ||
    { echo "Apple's license missing from lib/apple_gptk: run cave runtime add-d3dmetal again" >&2; exit 1; }
[[ ! -d $SRC/lib/dxmt || -f $SRC/lib/dxmt/LICENSE ]] ||
    { echo "DXMT's license missing from lib/dxmt: run cave runtime add-dxmt again" >&2; exit 1; }

mkdir -p "$OUT"
archive=$OUT/$NAME.tar.xz
echo "Archiving $NAME ($(du -sh "$SRC" | cut -f1)), xz compression…"
tar -C "$RUNTIMES" --exclude "$NAME/gstreamer-registry.*" \
    --options xz:compression-level=6,xz:threads=0 -cJf "$archive.part" "$NAME"
mv "$archive.part" "$archive"
(cd "$OUT" && shasum -a 256 "$NAME.tar.xz" > "$NAME.tar.xz.sha256")
echo "→ $archive ($(du -h "$archive" | cut -f1))"
