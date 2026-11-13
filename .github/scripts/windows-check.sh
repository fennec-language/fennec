#!/usr/bin/env bash
# The workspace and its tests compile for Windows with every warning an error, as CI's Windows
# jobs build them, so code that's only right elsewhere, like a variable only Windows leaves
# unused, fails here rather than on main. It's `cargo check` for `x86_64-pc-windows-gnu`, with
# MinGW-w64's C compiler for the C that ring, SQLite and mimalloc carry: `brew install
# mingw-w64`. Nothing is linked or run.
#
# Its target folder is this worktree's own, beside the usual one, and warnings are errors only for
# the Windows code, so the build scripts and the rest of `target` are built as always.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
cd "$root"
target=x86_64-pc-windows-gnu
command -v x86_64-w64-mingw32-gcc > /dev/null || {
  echo "windows-check: there's no MinGW-w64 C compiler to check for Windows with: \`brew install mingw-w64\`" >&2
  exit 1
}
rustup target add "$target" > /dev/null 2>&1 || {
  echo "windows-check: Rust's $target target can't be added: \`rustup target add $target\`" >&2
  exit 1
}
CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc \
  AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar \
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-D warnings" \
  cargo check --locked --workspace --tests --target "$target" --target-dir target/windows-check
