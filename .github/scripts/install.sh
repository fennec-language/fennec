#!/usr/bin/env bash
# Installs a working `fennec` from this checkout: the command built for release, every runtime
# library it links programs with, built from the same source so none is stale, and LLVM's tools
# beside it, laid out as a release archive is.
#
#   .github/scripts/install.sh            installs into ~/.fennec/bin
#   .github/scripts/install.sh FOLDER     installs into FOLDER
#
# It needs Rust's toolchain, which `rust-toolchain.toml` names and rustup installs by itself, and a
# C compiler for the runtime's C parts. Run it again after pulling to bring everything up to date.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
into="${1:-${FENNEC_HOME:-$HOME/.fennec}/bin}"
# Its user's alone, as fennec makes Fennec's folder: what's kept there is run.
(umask 077 && mkdir -p "$into")

echo "building fennec and its runtime libraries for release; this takes a few minutes the first time" >&2
cargo build --release --locked -p fennec -p fennec-runtime
from="target/release"
cp "$from/fennec" "$into/fennec.new"
cp "$from/libfennec_runtime.a" "$into/libfennec_runtime.a"
# Each runtime with features is built in a folder of its own, since built with the others, Cargo
# would give all of them its features.
for features in threads; do
  dir="target/install-${features//,/-}"
  name="libfennec_runtime_${features//,/_}"
  cargo build --release --locked -p fennec-runtime --features "$features" --target-dir "$dir"
  cp "$dir/release/libfennec_runtime.a" "$into/$name.a"
done
# A release from an earlier install has bitcode beside the libraries, which a release build would
# take in place of the new libraries, so it goes.
rm -f "$into"/libfennec_runtime*.bc

# LLVM's tools from the toolchain, where `fennec` looks for them beside itself.
rustup component add llvm-tools-preview > /dev/null 2>&1 || true
sysroot="$(rustc --print sysroot)"
host="$(rustc -vV | sed -n 's/^host: //p')"
tools="$sysroot/lib/rustlib/$host/bin"
mkdir -p "$into/llvm/bin" "$into/llvm/lib"
for tool in opt llc llvm-link; do
  cp "$tools/$tool" "$into/llvm/bin/"
done
# The tools share LLVM from a library of their own, which they find in `../lib`.
cp "$sysroot"/lib/libLLVM* "$into/llvm/lib/" 2> /dev/null || true

# Swapped in last, so a `fennec` that was running keeps working until it's replaced whole.
mv -f "$into/fennec.new" "$into/fennec"
echo "installed $("$into/fennec" --version) in $into" >&2
case ":$PATH:" in
  *":$into:"*) ;;
  *) echo "add it to your PATH: export PATH=\"$into:\$PATH\"" >&2 ;;
esac
