#!/usr/bin/env bash
# Builds what `fennec build --release` needs on Linux to make a program that runs on its own, with
# no C library or loader from the machine it runs on: the runtime libraries built for musl, with
# their bitcode, and musl's own start-up files and C library from Rust's toolchain. They go in the
# folder given, which `fennec` looks for as `musl` beside itself.
#
#   .github/scripts/musl-runtime.sh target/release/musl
#
# It needs a C compiler for musl, for the runtime's C parts: `musl-gcc`, from `musl-tools` on
# Debian and Ubuntu. `--debug` builds them quickly, without optimizing, for trying things out.
set -euo pipefail

out="${1:?say where the folder goes, like target/release/musl}"
profile=release
[ "${2:-}" = --debug ] && profile=debug
# The bitcode comes from a build with LTO, which the release profile has. Without it, Cargo tells
# rustc to leave the bitcode out, which `-C lto` below refuses.
[ "$profile" = debug ] && export CARGO_PROFILE_DEV_LTO=true
release=()
[ "$profile" = release ] && release=(--release)
target="$(uname -m)-unknown-linux-musl"
cc_variable="CC_$(echo "$target" | tr - _)"
export "$cc_variable=${!cc_variable:-musl-gcc}"

rustup target add "$target" > /dev/null
mkdir -p "$out/self-contained"
for features in "" threads; do
    dir="target/musl-${features:-plain}"
    dir="${dir//,/-}"
    name="libfennec_runtime${features:+_${features//,/_}}"
    cargo rustc --locked -p fennec-runtime ${features:+--features "$features"} --target "$target" \
        --target-dir "$dir" ${release[@]+"${release[@]}"} --crate-type staticlib \
        -- --emit=llvm-bc,link -C lto=fat -C codegen-units=1
    cp "$dir/$target/$profile/libfennec_runtime.a" "$out/$name.a"
    cp "$(find "$dir/$target/$profile/deps" -name 'fennec_runtime-*.bc' -print -quit)" "$out/$name.bc"
done
from="$(rustc --print sysroot)/lib/rustlib/$target/lib/self-contained"
cp "$from"/{crt1.o,crti.o,crtn.o,crtbegin.o,crtend.o,libc.a,libunwind.a} "$out/self-contained/"
# The compiler's helpers, like `__multf3`, taken out of the runtime: musl's C library calls them,
# and a release build that links only the runtime's C parts gets them from here, so linking needs
# no `libgcc` from the machine.
helpers="$(mktemp -d)"
trap 'rm -rf "$helpers"' EXIT
at="$(cd "$out" && pwd)"
ar t "$at/libfennec_runtime.a" | grep '^compiler_builtins-' > "$helpers/names"
[ -s "$helpers/names" ] || { echo "musl-runtime: the runtime holds no compiler_builtins" >&2; exit 1; }
(cd "$helpers" && xargs ar x "$at/libfennec_runtime.a" < names && xargs ar rcs "$at/self-contained/libcompiler_builtins.a" < names)
