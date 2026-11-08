#!/usr/bin/env bash
# Holds that a changed compiler never runs what an older one compiled. It builds a program, which
# compiles every function, and again, which takes them all from the cache. Then it changes the
# compiler, by a comment, builds it again, and builds the program once more: every function has
# to be compiled anew. A cache keyed by what's compiled alone would take them all from the cache
# here, and so would run what an older compiler made of them, whatever the new one does.
#
#   .github/scripts/test-stale-cache.sh
#
# It leaves the source as it was, and builds `fennec` again as it was too.
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
cd "$root"
work="$(mktemp -d)"
changed=crates/llvm/src/build.rs
cp "$changed" "$work/original.rs"
put_back() {
  cp "$work/original.rs" "$changed"
  cargo build -q --locked -p fennec -p fennec-runtime || true
  rm -rf "$work"
}
trap put_back EXIT
export FENNEC_HOME="$work/home"
unset FENNEC_NO_CACHE
built() {
  ./target/debug/fennec build examples/hello.fen -o "$work/hello" 2>&1 | sed -n 's/.*compiled \([0-9]*\) of \([0-9]*\) functions.*/\1 \2/p'
}
cargo build -q --locked -p fennec -p fennec-runtime
read -r fresh total <<< "$(built)"
[ "$fresh" = "$total" ] && [ "$total" -gt 0 ] || { echo "the first build compiled $fresh of $total functions" >&2; exit 1; }
read -r fresh _ <<< "$(built)"
[ "$fresh" = 0 ] || { echo "an unchanged build compiled $fresh functions again" >&2; exit 1; }
echo "// A change to the compiler, from test-stale-cache.sh." >> "$changed"
cargo build -q --locked -p fennec -p fennec-runtime
read -r fresh total_after <<< "$(built)"
if [ "$fresh" != "$total_after" ]; then
  echo "after the compiler changed, a build took $((total_after - fresh)) of $total_after functions from the cache" >&2
  exit 1
fi
[ "$("$work/hello")" = "$(cat examples/hello.out)" ]
echo "a changed compiler compiled all $total_after functions again"
