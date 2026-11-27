#!/usr/bin/env bash
# Builds the lossless JPEG recompressors used in the JPEG benchmark into $TOOLS/bin
# (default bench/.tools/bin), at pinned versions. Linux x86-64; needs git, cmake, a C/C++
# compiler and Rust. cjxl/djxl come from bench/install-baselines.sh (libjxl 0.13.0).
set -euo pipefail
export GIT_CONFIG_PARAMETERS="'advice.detachedHead=false'"

HERE="$(cd "$(dirname "$0")" && pwd)"
TOOLS="${TOOLS:-$HERE/.tools}"
mkdir -p "$TOOLS/bin" "$TOOLS/src"
cd "$TOOLS/src"
JOBS="$(nproc 2>/dev/null || echo 4)"
export CC="${CC:-gcc}" CXX="${CXX:-g++}"

checkout() { # repo dir ref (tag or commit)
  [ -d "$2" ] || git clone -q --depth 1 "$1" "$2"
  git -C "$2" fetch -q --depth 1 origin "$3"
  git -C "$2" checkout -q --force FETCH_HEAD
}

# libjpeg-turbo 3.2.1: cjpeg/djpeg/jpegtran, used to build the re-encoded Kodak sets
checkout https://github.com/libjpeg-turbo/libjpeg-turbo.git libjpeg-turbo 3.2.1
cmake -S libjpeg-turbo -B libjpeg-turbo/build -DCMAKE_BUILD_TYPE=Release -DENABLE_SHARED=0 -DWITH_TURBOJPEG=0 >/dev/null
cmake --build libjpeg-turbo/build -j "$JOBS" --target cjpeg-static djpeg-static jpegtran-static >/dev/null
for b in cjpeg djpeg jpegtran; do cp "libjpeg-turbo/build/$b-static" "$TOOLS/bin/$b"; done

# Dropbox Lepton (final master, f34c7f4)
checkout https://github.com/dropbox/lepton.git lepton f34c7f42d2e0d688239cb880648dc152713c4eae
cmake -S lepton -B lepton/build -DCMAKE_BUILD_TYPE=Release -DCMAKE_CXX_FLAGS="-Wno-error -fpermissive" -DCMAKE_C_FLAGS="-Wno-error" >/dev/null
cmake --build lepton/build -j "$JOBS" --target lepton >/dev/null
cp lepton/build/lepton "$TOOLS/bin/lepton"

# Microsoft's Rust port of Lepton (format-compatible, maintained)
checkout https://github.com/microsoft/lepton_jpeg_rust.git lepton_jpeg_rust v0.5.9
cargo build --release --quiet --manifest-path lepton_jpeg_rust/Cargo.toml -p lepton_jpeg_util
cp lepton_jpeg_rust/target/release/lepton_jpeg_util "$TOOLS/bin/lepton-rust"

# packJPG 2.5n
checkout https://github.com/packjpg/packJPG.git packJPG 2.5n
make -s -C packJPG/source -j "$JOBS" >/dev/null 2>&1 || make -C packJPG/source
cp packJPG/source/packJPG "$TOOLS/bin/packJPG"

# paq8px v217
checkout https://github.com/hxim/paq8px.git paq8px v217
cmake -S paq8px -B paq8px/build -DCMAKE_BUILD_TYPE=Release -DNATIVECPU=OFF >/dev/null
cmake --build paq8px/build -j "$JOBS" >/dev/null
cp paq8px/build/paq8px "$TOOLS/bin/paq8px"

# brunsli (google/brunsli master, c18b962)
checkout https://github.com/google/brunsli.git brunsli c18b9625ef045b0b89ebb4439ae2339e2160df71
git -C brunsli submodule update -q --init --recursive
cmake -S brunsli -B brunsli/build -DCMAKE_BUILD_TYPE=Release -DBUILD_TESTING=OFF >/dev/null
cmake --build brunsli/build -j "$JOBS" --target cbrunsli dbrunsli >/dev/null
cp brunsli/build/artifacts/cbrunsli brunsli/build/artifacts/dbrunsli "$TOOLS/bin/"

echo "Installed into $TOOLS/bin:"
ls "$TOOLS/bin"
