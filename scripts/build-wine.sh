#!/bin/bash
# Builds Wine from the CrossOver sources (LGPL, published by CodeWeavers)
# as a standalone runtime for cave. The runtime is named wine-cx-<version>:
# cave is not CrossOver and is not affiliated with CodeWeavers.
#
#   scripts/build-wine.sh [version]        (default: 26.3.0)
#
# Each step is skipped if it has already been done: the script can be rerun
# after an error. To reconfigure, delete $BUILD/config.status.
#
# Choices:
# - Everything runs under Rosetta (arch -x86_64). Wine for macOS is an
#   x86_64 program: building it "natively" as x86_64 avoids the pitfalls
#   of cross-compiling (tools generated during the build, etc.).
# - The Windows DLLs (PE format) are built with mingw-w64 GCC, as CodeWeavers
#   does (their kernelbase.dll carries "GCC: (GNU) 13.2.0"). With
#   llvm-mingw (clang), Steam never connects to its servers.
# - The x86_64 libraries (gnutls, freetype, SDL2, vulkan…) are taken from
#   Gcenx's Wine (Wine Devel.app); their headers come from Homebrew, as they
#   do not depend on the architecture.
# - configure options: Gcenx's (macports-wine, wine-devel), but targeting
#   macOS 10.15 and with inotify, like the CrossOver build (otool -l, -L).
#
# Requirements: Xcode, brew install bison mingw-w64, Wine Devel.app installed.

set -euo pipefail

VERSION=${1:-26.3.0}
WINE_DEVEL="/Applications/Wine Devel.app/Contents/Resources/wine"

REPO=$(cd "$(dirname "$0")/.." && pwd)
WORK=${CAVE_BUILD:-$HOME/Library/Caches/cave-build}
SRC=$WORK/src/crossover-$VERSION/sources/wine
BUILD=$WORK/build/wine-$VERSION
STAGE=$WORK/install/wine-cx-$VERSION # make install wants a path without spaces
DEPS=$WORK/deps
RUNTIMES=${CAVE_RUNTIMES:-"$HOME/Library/Application Support/fr.umebrunet.cave/runtimes"}
DEST="$RUNTIMES/wine-cx-$VERSION"

step() { printf '\n==> %s\n' "$*"; }

# Downloads into <file>.part, then renames: an interrupted download never
# passes for a finished one. Servers sometimes fail from GitHub's machines:
# CodeWeavers' cuts connections after a minute or two, GStreamer's answers
# 503. The download resumes where it stopped as long as each attempt brings
# something new; a server sending nothing gets 20, 40, then 60 seconds.
fetch() { # <url> <file>
    [[ -f $2 ]] && return 0
    local size=-1 now stalls=0
    until curl -fL -C - -o "$2.part" "$1"; do
        now=$(stat -f %z "$2.part" 2>/dev/null || echo 0)
        if ((now > size)); then stalls=0; else stalls=$((stalls + 1)); fi
        ((stalls < 4)) || return 1
        size=$now
        echo "download interrupted at $now bytes, resuming in $((3 + stalls * 20)) s: $1" >&2
        sleep $((3 + stalls * 20))
    done
    mv "$2.part" "$2"
}

# -- Sources -----------------------------------------------------------------
if [[ ! -d $SRC ]]; then
    step "Downloading the CrossOver $VERSION sources"
    mkdir -p "$WORK/dl" "$WORK/src/crossover-$VERSION"
    tarball=$WORK/dl/crossover-sources-$VERSION.tar.gz
    fetch "https://media.codeweavers.com/pub/crossover/source/crossover-sources-$VERSION.tar.gz" "$tarball"
    tar -xzf "$tarball" -C "$WORK/src/crossover-$VERSION"
fi

# -- Our patches (patches/wine), applied only once ----------------------------
for patch in "$REPO/patches/wine/"*.patch; do
    # If it applies in reverse, it is already in place.
    patch -d "$SRC" -p1 -R -f -s --dry-run < "$patch" >/dev/null 2>&1 && continue
    step "Patch $(basename "$patch")"
    patch -d "$SRC" -p1 -s < "$patch"
done

# -- Tools -------------------------------------------------------------------
BISON=/opt/homebrew/opt/bison/bin
[[ -x $BISON/bison ]] || { echo "a recent bison is required: brew install bison" >&2; exit 1; }

MINGW=/opt/homebrew/bin
[[ -x $MINGW/x86_64-w64-mingw32-gcc ]] || { echo "mingw-w64 is required: brew install mingw-w64" >&2; exit 1; }

# -- x86_64 libraries --------------------------------------------------------
if [[ ! -d $DEPS/lib ]]; then
    step "Copying the x86_64 libraries from Wine Devel.app"
    mkdir -p "$DEPS/lib"
    cp -c "$WINE_DEVEL/lib/"*.dylib "$DEPS/lib/"
fi
mkdir -p "$DEPS/no-pkgconfig"
# inotify (file watching): the x86_64 library comes from Gcenx, the header
# from libinotify-kqueue (not in Homebrew).
if [[ ! -f $DEPS/include/sys/inotify.h ]]; then
    mkdir -p "$DEPS/include/sys"
    fetch https://raw.githubusercontent.com/libinotify-kqueue/libinotify-kqueue/master/sys/inotify.h \
        "$DEPS/include/sys/inotify.h"
fi
# configure derives the name to load (dlopen) from each library's install
# name, cutting after the last "/". Gcenx reduced them to "libgnutls.dylib"
# with no path: configure then keeps the whole otool line. We rename them
# to @rpath/<file>; Wine will keep <file>, which sits in lib/ next to it.
if [[ ! -f $DEPS/.renamed ]]; then
    for f in "$DEPS/lib/"*.dylib; do
        install_name_tool -id "@rpath/$(basename "$f")" "$f" 2>/dev/null
        codesign -f -s - "$f" 2>/dev/null
    done
    touch "$DEPS/.renamed"
fi

# -- GStreamer (video) -------------------------------------------------------
# The official macOS package is universal: we extract it without installing
# it (no sudo) and merge its components into a local framework.
GST_VERSION=1.28.8
# The packages are not signed: their published checksums (<pkg>.sha256sum),
# pinned here, which also spares asking GStreamer's server each time.
gst_sha256() { # <kind: "" or devel->
    case $1 in
        "") echo d371ab04a7632713626836c2fc234e863cc98484c74c473eba99b0fbd9b3a3fe ;;
        devel-) echo e6d380bc17be7eccf13e0154567d4718fc9321da822408c7ca0cd808b3d4a3f7 ;;
    esac
}
GST=$WORK/src/gstreamer-$GST_VERSION/fw
if [[ ! -f $GST/lib/pkgconfig/gstreamer-1.0.pc ]]; then
    step "GStreamer $GST_VERSION"
    tmp=$WORK/src/gstreamer-$GST_VERSION/pkg
    for kind in "" devel-; do
        pkg=gstreamer-1.0-$kind$GST_VERSION-universal.pkg
        url=https://gstreamer.freedesktop.org/data/pkg/osx/$GST_VERSION/$pkg
        fetch "$url" "$WORK/dl/$pkg"
        [[ $(shasum -a 256 "$WORK/dl/$pkg" | awk '{print $1}') == "$(gst_sha256 "$kind")" ]] ||
            { echo "wrong SHA-256 checksum: $pkg" >&2; rm -f "$WORK/dl/$pkg"; exit 1; }
        rm -rf "$tmp"
        mkdir -p "$(dirname "$tmp")" # pkgutil does not create the parent folder
        pkgutil --expand-full "$WORK/dl/$pkg" "$tmp"
        for component in "$tmp/"*.pkg; do
            # osx-framework only provides the framework's shell (links to
            # Versions/Current); the other components are laid out flat.
            [[ $component == */osx-framework-* ]] && continue
            [[ -d $component/Payload ]] && ditto "$component/Payload" "$GST"
        done
        rm -rf "$tmp"
    done
fi

# -- Build environment -------------------------------------------------------
export PATH="$BISON:$PATH"
# PE compilers, forced so that configure does not pick a clang.
export x86_64_CC="$MINGW/x86_64-w64-mingw32-gcc"
export i386_CC="$MINGW/i686-w64-mingw32-gcc"
# No .pc files from Homebrew: they would point to arm64 libraries.
export PKG_CONFIG_LIBDIR=$DEPS/no-pkgconfig
export FREETYPE_CFLAGS="-I/opt/homebrew/opt/freetype/include/freetype2"
export FREETYPE_LIBS="$DEPS/lib/libfreetype.6.dylib"
export GNUTLS_CFLAGS="-I/opt/homebrew/opt/gnutls/include"
export GNUTLS_LIBS="$DEPS/lib/libgnutls.30.dylib"
export SDL2_CFLAGS="-I/opt/homebrew/opt/sdl2/include/SDL2 -D_THREAD_SAFE"
export SDL2_LIBS="$DEPS/lib/libSDL2-2.0.0.dylib"
export INOTIFY_CFLAGS="-I$DEPS/include"
export INOTIFY_LIBS="$DEPS/lib/libinotify.0.dylib"
# GStreamer's .pc files point to /Library/Frameworks; --define-prefix makes
# them point to our copy.
gst_pc() {
    PKG_CONFIG_LIBDIR=$GST/lib/pkgconfig pkg-config --define-prefix "$@" \
        gstreamer-1.0 gstreamer-video-1.0 gstreamer-audio-1.0 gstreamer-tag-1.0
}
GSTREAMER_CFLAGS=$(gst_pc --cflags)
GSTREAMER_LIBS=$(gst_pc --libs)
export GSTREAMER_CFLAGS GSTREAMER_LIBS
# The .so files in lib/wine/x86_64-unix find the dylibs in lib/ (as Gcenx does).
# The absolute rpath to deps/ is for the tools run during the build
# (sfnt2fon…); it is removed from the installed files at the end.
export LDFLAGS="-L$DEPS/lib -Wl,-rpath,@loader_path/../../ -Wl,-rpath,@loader_path/../lib -Wl,-rpath,$DEPS/lib"
# arch -x86_64 is not enough: the compiler, run by configure through
# /usr/bin/gcc, switches back to arm64. We force the target explicitly.
export CC="/usr/bin/clang -arch x86_64"
export CFLAGS="-O2"
export CROSSCFLAGS="-O2"
export MACOSX_DEPLOYMENT_TARGET=10.15

# -- Configure ---------------------------------------------------------------
if [[ ! -f $BUILD/config.status ]]; then
    step "Configure"
    mkdir -p "$BUILD"
    cd "$BUILD"
    arch -x86_64 "$SRC/configure" \
        --prefix="$STAGE" \
        --enable-archs=i386,x86_64 \
        --disable-tests \
        --disable-winebth_sys \
        --with-coreaudio --with-cups --with-freetype --with-gnutls --with-gstreamer --with-inotify \
        --with-mingw --with-opencl --with-pcap --with-pcsclite \
        --with-pthread --with-sdl --with-vulkan \
        --without-alsa --without-capi --without-dbus --without-ffmpeg \
        --without-fontconfig --without-gettext --without-gettextpo \
        --without-gphoto --without-gssapi \
        --without-krb5 --without-netapi --without-opengl \
        --without-oss --without-pulse --without-sane --without-udev \
        --without-usb --without-v4l2 --without-wayland --without-x \
        INSTALL="$SRC/tools/install-sh -c"
fi

# -- Build and install -------------------------------------------------------
step "Building ($(sysctl -n hw.ncpu) cores)"
cd "$BUILD"
arch -x86_64 make -j"$(sysctl -n hw.ncpu)"

step "Installing into $STAGE"
rm -rf "$STAGE"
# prefix is forced: a build folder configured under another name (the runtime
# used to be called crossover-<version>) still installs in the right place.
arch -x86_64 make install-lib prefix="$STAGE"
cp -c "$DEPS/lib/"*.dylib "$STAGE/lib/"

# GStreamer: what winegstreamer and the plugins need (transitive closure of
# the dependencies), with the same plugins as CrossOver. Thinned to x86_64;
# the libraries already provided by Gcenx (libintl, MoltenVK) are kept.
GST_PLUGINS="applemedia asf audioconvert audioparsers audioresample avi coreelements
    deinterlace id3demux isomp4 opengl playback typefindfunctions videoconvertscale
    videofilter videoparsersbad wavparse"
mkdir -p "$STAGE/lib/gstreamer-1.0"
queue=(libgstreamer-1.0.0.dylib libgstbase-1.0.0.dylib libgstvideo-1.0.0.dylib
       libgstaudio-1.0.0.dylib libgsttag-1.0.0.dylib)
for p in $GST_PLUGINS; do queue+=("gstreamer-1.0/libgst$p.dylib"); done
seen=" "
while ((${#queue[@]})); do
    f=${queue[0]}
    queue=("${queue[@]:1}")
    [[ $seen == *" $f "* ]] && continue
    seen+="$f "
    if [[ ! -f $STAGE/lib/$f ]]; then
        lipo "$GST/lib/$f" -thin x86_64 -output "$STAGE/lib/$f" 2>/dev/null ||
            cp "$GST/lib/$f" "$STAGE/lib/$f"
    fi
    # @rpath/… dependencies that exist in lib/ (the first one is the file itself).
    for dep in $(otool -L "$GST/lib/$f" | sed -n 's|^[[:space:]]*@rpath/\([^ ]*\) .*|\1|p' | sort -u); do
        [[ -f $GST/lib/$dep ]] && queue+=("$dep")
    done
done
find "$STAGE/bin" "$STAGE/lib/wine" -type f | while read -r f; do
    if otool -l "$f" 2>/dev/null | grep -q "path $DEPS/lib (offset"; then
        install_name_tool -delete_rpath "$DEPS/lib" "$f" && codesign -f -s - "$f" 2>/dev/null
    fi
done

# Mono (.NET) and Gecko (HTML engine), at the exact versions the sources
# expect: without them, Wine offers to download them for every new bottle.
step "Mono, Gecko and Vulkan configuration"
mono=$(sed -n 's/^#define WINE_MONO_VERSION "\(.*\)"/\1/p' "$SRC/dlls/mscoree/mscoree_private.h")
gecko=$(sed -n 's/^#define GECKO_VERSION "\(.*\)"/\1/p' "$SRC/dlls/appwiz.cpl/addons.c")
fetch_addon() { # <share/wine folder> <url>
    local archive=$WORK/dl/$(basename "$2")
    fetch "$2" "$archive"
    mkdir -p "$STAGE/share/wine/$1"
    tar -xJf "$archive" -C "$STAGE/share/wine/$1"
}
fetch_addon mono "https://dl.winehq.org/wine/wine-mono/$mono/wine-mono-$mono-x86.tar.xz"
for a in x86 x86_64; do
    fetch_addon gecko "https://dl.winehq.org/wine/wine-gecko/$gecko/wine-gecko-$gecko-$a.tar.xz"
done
# Tells the Vulkan loader where to find MoltenVK (relative path, as Gcenx does).
mkdir -p "$STAGE/lib/wine/x86_64-unix/vulkan/icd.d"
cp "$WINE_DEVEL/lib/wine/x86_64-unix/vulkan/icd.d/MoltenVK_icd.json" \
    "$STAGE/lib/wine/x86_64-unix/vulkan/icd.d/"

step "Copying the runtime to cave"
mkdir -p "$(dirname "$DEST")"
# The graphics layers added afterwards (scripts/add-dxmt.sh,
# scripts/add-d3dmetal.sh) are kept.
keep=$WORK/keep
rm -rf "$keep"
mkdir -p "$keep"
for layer in dxmt apple_gptk; do
    [[ -d $DEST/lib/$layer ]] && mv "$DEST/lib/$layer" "$keep/"
done
rm -rf "$DEST"
ditto "$STAGE" "$DEST"
for layer in dxmt apple_gptk; do
    [[ -d $keep/$layer ]] && mv "$keep/$layer" "$DEST/lib/"
done
rm -rf "$keep"
CAVE_RUNTIMES=$RUNTIMES "$REPO/scripts/add-winetricks.sh" "$(basename "$DEST")"
GST_VERSION=$GST_VERSION "$REPO/scripts/add-notices.sh" "$DEST" "$VERSION"
echo "Runtime ready: $DEST/bin/wine"
