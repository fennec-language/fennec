#!/usr/bin/env bash
# The workspace and its tests compile for Windows, and pass clippy there, with every warning an
# error, as the gate's clippy step holds the code for this machine. So code that's only right
# elsewhere, like a variable only Windows leaves unused or a lint only Windows-only code trips,
# fails here rather than on main, where nothing else lints Windows' code (#1765). It's `cargo
# clippy` for `x86_64-pc-windows-gnu`, with MinGW-w64's C compiler for the C that ring, SQLite and
# mimalloc carry: `brew install mingw-w64`. Nothing is linked or run.
#
# Its target folder is this worktree's own, beside the usual one.
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
  cargo clippy --locked --workspace --all-targets --target "$target" --target-dir target/windows-check -- -D warnings
