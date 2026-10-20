#!/bin/sh
# Cross-compile the Windows build from Linux and gather it into dist-windows/.
# Needs: mingw-w64, `rustup target add x86_64-pc-windows-gnu`, npm install.
set -e
cd "$(dirname "$0")/.."
ROOT=$PWD
TARGET=x86_64-pc-windows-gnu
REL=src-tauri/target/$TARGET/release

# CFLAGS paths in .cargo/config.toml are relative to src-tauri
export CFLAGS_x86_64_pc_windows_gnu="-march=haswell -mtune=generic -include $ROOT/src-tauri/cross/mingw-compat.h"

build() { npx tauri build --target $TARGET --no-bundle; }
if ! build; then
  # ggml names its MinGW static libs without the "lib" prefix rustc looks for.
  for f in $REL/build/whisper-rs-sys-*/out/build/ggml/src/ggml*.a; do
    cp "$f" "$(dirname "$f")/lib$(basename "$f")"
  done
  build
fi

OUT=dist-windows
rm -rf $OUT && mkdir -p $OUT
cp $REL/sts2-callout.exe "$OUT/StS2 Callout.exe"
cp "$(find ~/.cargo/registry/src -path '*webview2-com-sys*/x64/WebView2Loader.dll' | head -1)" $OUT/
GCC=$(dirname "$(x86_64-w64-mingw32-g++-posix -print-libgcc-file-name)")
cp $GCC/libstdc++-6.dll $GCC/libgcc_s_seh-1.dll /usr/x86_64-w64-mingw32/lib/libwinpthread-1.dll $OUT/
x86_64-w64-mingw32-strip --strip-unneeded $OUT/*.dll "$OUT/StS2 Callout.exe"
echo "Windows build in $OUT/"
