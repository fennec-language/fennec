#!/usr/bin/env bash
# Installs Fennec from this checkout as a release is installed: the launcher as the `fennec` in
# `~/.fennec/bin`, and this checkout's build as the toolchain `dev`, laid out as a release's folder
# is. That's the command built for release, every runtime library it links programs with, built
# from the same source so none is stale, and LLVM's tools beside it. `dev` becomes the version for
# scripts outside a kit unless another is set, and `fennec new` makes kits that ask for `dev`.
#
#   .github/scripts/install.sh            installs into ~/.fennec, or the folder FENNEC_HOME names
#
# It needs Rust's toolchain, which `rust-toolchain.toml` names and rustup installs by itself, and a
# C compiler for the runtime's C parts. Run it again after pulling to bring everything up to date.
set -euo pipefail

if [ "$#" -ne 0 ]; then
  echo "install.sh takes no arguments: it installs into ~/.fennec, or the folder FENNEC_HOME names" >&2
  exit 1
fi
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
home="${FENNEC_HOME:-$HOME/.fennec}"
bin="$home/bin"
into="$home/toolchains/dev"
# Its user's alone, as fennec makes Fennec's folder: what's kept there is run.
(umask 077 && mkdir -p "$home" "$bin" "$into")

# What the build needs, said before it starts rather than as an error deep inside it.
if ! command -v cargo > /dev/null 2>&1; then
  echo "install.sh needs Rust: install rustup from https://rustup.rs, open a new terminal, and run this again" >&2
  exit 1
fi
if ! command -v cc > /dev/null 2>&1; then
  echo "install.sh needs a C compiler for the runtime's C parts: \`sudo apt install build-essential\` on Debian or Ubuntu, \`sudo dnf install gcc\` on Fedora, or \`xcode-select --install\` on macOS; then run this again" >&2
  exit 1
fi

echo "building fennec, its runtime libraries and the launcher for release; this takes a few minutes the first time" >&2
cargo build --release --locked -p fennec -p fennec-runtime -p fennec-launcher
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

# Each program is swapped in last, so one that was running keeps working until it's replaced whole.
mv -f "$into/fennec.new" "$into/fennec"
for program in fennec-setup fennec-fetch fennec-launcher; do
  installed="$program"
  [ "$program" = fennec-launcher ] && installed=fennec
  cp "$from/$program" "$bin/$installed.new"
  mv -f "$bin/$installed.new" "$bin/$installed"
done
# An install from before the launcher put the toolchain itself in `bin`. What's left of it goes.
rm -rf "$bin/llvm" "$bin"/libfennec_runtime*

export FENNEC_HOME="$home"
"$bin/fennec" toolchain link dev "$into"
default="$("$bin/fennec" default 2> /dev/null || true)"
if [ -z "$default" ]; then
  "$bin/fennec" default dev
  default=dev
fi
echo "installed $("$bin/fennec" +dev --version) as the toolchain \`dev\`, run by the launcher $bin/fennec" >&2
if [ "$default" != dev ]; then
  echo "scripts outside a kit run Fennec $default, as \`fennec default\` set it: \`fennec default dev\` runs this build for them" >&2
fi
case ":$PATH:" in
  *":$bin:"*) ;;
  *) echo "add it to your PATH: export PATH=\"$bin:\$PATH\"" >&2 ;;
esac
