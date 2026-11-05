#!/bin/bash
# What building Fennec on Windows and shipping it there take, for CI and releases alike.
#
#   windows.sh toolchain       puts Rust's GNU toolchain for LLVM and llvm-mingw in place
#   windows.sh folder DEST     gathers the `llvm` folder that goes beside `fennec`
#
# Nothing here is GCC's. Rust's `x86_64-pc-windows-gnullvm` unwinds with LLVM's libunwind, and
# llvm-mingw has that, compiler-rt and MinGW-w64's runtime, built with Clang.
set -euo pipefail

mingw_name=llvm-mingw-20251104-ucrt-x86_64
mingw_url="https://github.com/mstorsjo/llvm-mingw/releases/download/20251104/$mingw_name.zip"
mingw_sha256=171b298f46fa4b5653dc7b01cc5e709caca645991be8e1b0724466c4a282a1ac
mingw_root="$RUNNER_TEMP/$mingw_name"

toolchain() {
  curl -sSfL --retry 3 -o "$RUNNER_TEMP/llvm-mingw.zip" "$mingw_url"
  echo "$mingw_sha256  $RUNNER_TEMP/llvm-mingw.zip" | sha256sum --check --quiet
  7z x -y -o"$RUNNER_TEMP" "$RUNNER_TEMP/llvm-mingw.zip" > /dev/null
  cygpath -w "$mingw_root/bin" >> "$GITHUB_PATH"
  rustup set default-host x86_64-pc-windows-gnullvm
  # Setting `RUSTFLAGS` replaces what `.cargo/config.toml` says.
  echo "RUSTFLAGS=${RUSTFLAGS:-} -C target-feature=+crt-static" >> "$GITHUB_ENV"
}

# LLVM's tools from Rust's MSVC toolchain: they're half the size of the GNU one's, and need only
# Windows' own libraries. They run as programs of their own, so what built them doesn't matter.
folder() {
  local dest="$1"
  local channel libs msvc
  channel="$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)"
  rustup toolchain install "$channel-x86_64-pc-windows-msvc" --profile minimal --component llvm-tools-preview --force-non-host --no-self-update
  msvc="$(rustup show home)/toolchains/$channel-x86_64-pc-windows-msvc/lib/rustlib/x86_64-pc-windows-msvc/bin"
  mkdir -p "$dest/bin" "$dest/lib/self-contained"
  cp "$msvc"/{opt,llc,llvm-link,llvm-ar,rust-lld}.exe "$dest/bin/"
  libs="$mingw_root/x86_64-w64-mingw32/lib"
  cp "$libs"/{crt2.o,libunwind.a,libmingw32.a,libmingwex.a,libucrt.a} "$dest/lib/self-contained/"
  cp "$libs"/lib{kernel32,ntdll,userenv,ws2_32,dbghelp,bcrypt,advapi32,synchronization,user32}.a "$dest/lib/self-contained/"
  cp "$mingw_root"/lib/clang/*/lib/windows/libclang_rt.builtins-x86_64.a "$dest/lib/self-contained/"
}

case "${1:-}" in
  toolchain) toolchain ;;
  folder) folder "$2" ;;
  *) echo "usage: windows.sh toolchain | folder DEST" >&2; exit 2 ;;
esac
