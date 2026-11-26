#!/usr/bin/env bash
# Installs the exact baseline encoders used in the benchmark into $TOOLS (default: bench/.tools).
# Linux x86-64. The same versions are available for macOS from the same upstream projects.
set -euo pipefail
export GIT_CONFIG_PARAMETERS="'advice.detachedHead=false'"

HERE="$(cd "$(dirname "$0")" && pwd)"
TOOLS="${TOOLS:-$HERE/.tools}"
mkdir -p "$TOOLS/bin" "$TOOLS/src"
cd "$TOOLS/src"

JXL_VER=0.13.0
WEBP_VER=1.6.0
AVIF_VER=1.4.2
OXIPNG_VER=10.2.1
RAR_VER=723
SEVENZIP_VER=2606

if command -v apt-get >/dev/null 2>&1; then
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq lzip unzip zip xz-utils libpng-dev pkg-config >/dev/null
fi

fetch() { [ -f "$2" ] || curl -fsSL --retry 4 -o "$2" "$1"; }

# JPEG XL (cjxl/djxl), official static build
fetch "https://github.com/libjxl/libjxl/releases/download/v$JXL_VER/jxl-linux-x86_64-static.tar.lz" jxl.tar.lz
mkdir -p jxl && lzip -dc jxl.tar.lz | tar -x -C jxl
cp "$(find jxl -name cjxl -type f | head -1)" "$(find jxl -name djxl -type f | head -1)" "$TOOLS/bin/"

# WebP (cwebp/dwebp), official prebuilt
fetch "https://storage.googleapis.com/downloads.webmproject.org/releases/webp/libwebp-$WEBP_VER-linux-x86-64.tar.gz" webp.tgz
tar -xzf webp.tgz
cp "libwebp-$WEBP_VER-linux-x86-64/bin/cwebp" "libwebp-$WEBP_VER-linux-x86-64/bin/dwebp" "$TOOLS/bin/"

# AVIF (avifenc/avifdec), official CI artifacts
fetch "https://github.com/AOMediaCodec/libavif/releases/download/v$AVIF_VER/linux-artifacts.zip" avif.zip
mkdir -p avif && unzip -oq avif.zip -d avif
for b in avifenc avifdec; do cp "$(find avif -name $b -type f | head -1)" "$TOOLS/bin/"; done

# oxipng
fetch "https://github.com/oxipng/oxipng/releases/download/v$OXIPNG_VER/oxipng-$OXIPNG_VER-x86_64-unknown-linux-musl.tar.gz" oxipng.tgz
tar -xzf oxipng.tgz
cp "oxipng-$OXIPNG_VER-x86_64-unknown-linux-musl/oxipng" "$TOOLS/bin/"

# zopflipng (built from source, zopfli-1.0.3)
[ -d zopfli ] || git clone -q --depth 1 --branch zopfli-1.0.3 https://github.com/google/zopfli.git
make -s -C zopfli zopflipng >/dev/null
cp zopfli/zopflipng "$TOOLS/bin/"

# RAR (rarlab)
fetch "https://www.rarlab.com/rar/rarlinux-x64-$RAR_VER.tar.gz" rar.tgz
tar -xzf rar.tgz
cp rar/rar "$TOOLS/bin/"

# 7-Zip (7zz)
fetch "https://github.com/ip7z/7zip/releases/download/${SEVENZIP_VER:0:2}.${SEVENZIP_VER:2:2}/7z$SEVENZIP_VER-linux-x64.tar.xz" 7z.txz
mkdir -p 7z && tar -xJf 7z.txz -C 7z
cp 7z/7zz "$TOOLS/bin/"

# QOI reference encoder (qoiconv)
[ -d qoi ] || git clone -q https://github.com/phoboslab/qoi.git
( cd qoi && fetch https://raw.githubusercontent.com/nothings/stb/master/stb_image.h stb_image.h \
  && fetch https://raw.githubusercontent.com/nothings/stb/master/stb_image_write.h stb_image_write.h \
  && cc -std=gnu99 -O3 -I. qoiconv.c -o qoiconv )
cp qoi/qoiconv "$TOOLS/bin/"

# FLIF 0.4 (reference encoder, built from source)
[ -d FLIF ] || git clone -q --depth 1 --branch v0.4 https://github.com/FLIF-hub/FLIF.git
make -s -C FLIF/src flif >/dev/null 2>&1 || make -C FLIF/src flif
cp FLIF/src/flif "$TOOLS/bin/"

echo "Installed into $TOOLS/bin:"
ls "$TOOLS/bin"
