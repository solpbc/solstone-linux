#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Run in a disposable native Ubuntu 22.04 build image.
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
version=2.54.3
sha=2779c1b46f88762e8400650271ba14452dc4e30b37059981552d8b562b7d0862
build=$(mktemp -d)
trap 'rm -rf -- "$build"' EXIT
cd "$build"
archive="webkitgtk-$version.tar.xz"
url="https://webkitgtk.org/releases/$archive"
curl -fLSs --retry 3 "$url" -o "$archive"
curl -fLSs --retry 3 "$url.asc" -o "$archive.asc"
printf '%s  %s\n' "$sha" "$archive" > source.sha256
sha256sum -c source.sha256
gpgv --keyring "$here/upstream.gpg" "$archive.asc" "$archive"
tar -xf "$archive"
cd "webkitgtk-$version"
patch --fuzz=0 -p1 -i "$here/gcc12-discarded-assert.patch"
patch --fuzz=0 -p1 -i "$here/gtk4-cursor-local-skimage.patch"
export CC=gcc-12 CXX=g++-12
cmake -S . -B ../out -GNinja -DPORT=GTK -DUSE_GTK4=ON -DCMAKE_BUILD_TYPE=Release \
    '-DCMAKE_CXX_FLAGS_RELEASE=-O2 -DNDEBUG' -DCMAKE_INSTALL_PREFIX=/opt/solstone-webkit \
    -DENABLE_BUBBLEWRAP_SANDBOX=ON -DENABLE_VIDEO=ON -DENABLE_WEB_AUDIO=ON -DENABLE_WEBGL=ON \
    -DENABLE_WAYLAND_TARGET=ON -DENABLE_X11_TARGET=ON -DENABLE_MINIBROWSER=OFF \
    -DENABLE_DOCUMENTATION=OFF -DENABLE_INTROSPECTION=OFF -DENABLE_WEBDRIVER=OFF \
    -DENABLE_SPEECH_SYNTHESIS=OFF -DUSE_JPEGXL=OFF -DUSE_AVIF=OFF -DUSE_LIBBACKTRACE=OFF
cmake --build ../out --parallel "${WEBKIT_BUILD_JOBS:-4}"
cmake --install ../out --strip

python3 "$here/stage.py" /opt/solstone-journal-dist --source "$PWD"
