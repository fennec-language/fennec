#!/usr/bin/env bash
# A release's dry run: installs a toolchain archive the release workflow built through the launcher,
# into a Fennec home with nothing in it, and then builds, runs and tests every example, every
# example kit and a kit with a dependency, from that install alone. Nothing it runs reaches this
# checkout, Rust's toolchain, or anything set up for development: a step that needs them fails
# here rather than on a person's machine.
#
#   .github/scripts/release-dry-run.sh fennec-v0.4.2-aarch64-apple-darwin.tar.gz [target]
#
# The launcher installs the archive for the platform it's built for, so it's built for the
# archive's own: on Windows that's `x86_64-pc-windows-gnullvm`, as the toolchain is, not the MSVC
# target Rust builds for there by default.
#
# Nobody has signed the archive, so `crates/launcher/examples/dry_run.rs` signs it with a key made
# for the run, which only that run trusts, and installs it. From then on it's the launcher people
# install, run as `fennec`, which trusts the release keys alone and finds the toolchain installed.
set -euo pipefail
archive=$(cd "$(dirname "${1:?give the archive to try}")" && pwd)/$(basename "$1")
repo=$(cd "$(dirname "$0")/../.." && pwd)
exe=""
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) exe=".exe" ;; esac

target=${2:-}
# Built while the checkout and Rust are still in reach: the launcher, and the dry run's installer.
(cd "$repo" && cargo build --locked --quiet --release -p fennec-launcher --bins --example dry_run ${target:+--target "$target"})
built="$repo/target/${target:+$target/}release"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# The launcher's folder as bash's own `PATH` takes it, which is `/c/...` on Windows.
bin="$work/bin"
# On Windows the programs are Windows' own, which read `C:/...` and not Git Bash's `/tmp/...`.
if [ -n "$exe" ]; then
  bin=$(cygpath -u "$work/bin")
  work=$(cygpath -m "$work")
  archive=$(cygpath -m "$archive")
fi
home="$work/home"
mkdir -p "$home" "$work/bin" "$work/dist"
cp "$built/fennec-launcher$exe" "$work/bin/fennec$exe"
cp "$built/examples/dry_run$exe" "$work/dry_run$exe"
cp -R "$repo/examples" "$work/examples"
within="$repo/.github/scripts/within.sh"

# What a person has who's installed nothing but Fennec: the launcher and the shell's own tools,
# with no C compiler, linker or `ar`, and nothing of Rust's, Cargo's, llvm-mingw's or Visual
# Studio's. On Linux and Windows the archive's own LLD links, and its own `llvm-ar` takes a
# library apart everywhere. On Linux and
# macOS a compiler sits in `/usr/bin` beside everything else, so the tools this script uses are
# linked into a folder of their own; Git for Windows' folders hold none.
if [ -n "$exe" ]; then
  reach="$bin:/usr/bin:/mingw64/bin:$(cygpath -u "$SYSTEMROOT")/System32"
else
  tools="$work/tools"
  mkdir -p "$tools"
  # `echo` too, which an example's program runs.
  for tool in bash sh env diff cat sed grep awk sort paste tr mkdir rm cp mv ls find dirname basename uname mktemp head tail sleep kill wc echo; do
    found=$(type -P "$tool") && ln -s "$found" "$tools/$tool"
  done
  reach="$bin:$tools"
fi
unreached=(cc c++ gcc clang ld ld.lld ld64.lld lld ar)
# A Mac links through Xcode's command line tools, which a person installs with
# `xcode-select --install`: `cc` is its way in, and finds the linker itself, not on the path.
if [ "$(uname -s)" = Darwin ]; then
  ln -s "$(type -P cc)" "$tools/cc"
  unreached=(c++ gcc clang ld ld.lld ld64.lld lld ar)
fi
for linker in "${unreached[@]}"; do
  if PATH="$reach" command -v "$linker" > /dev/null 2>&1; then
    echo "::error::$linker is in reach of the dry run: $(PATH="$reach" command -v "$linker")"
    exit 1
  fi
done
run() {
  env -u FENNEC_LLVM_BIN -u FENNEC_THREADS_RUNTIME -u FENNEC_EXPECT_LLVM -u FENNEC_DIST -u CARGO \
    PATH="$reach" RUSTUP_HOME="$work/no-rustup" CARGO_HOME="$work/no-cargo" FENNEC_HOME="$home" "$@"
}
step() { echo "--- $*" >&2; }

cd "$work"
step "install $(basename "$archive") into an empty home"
installed=$(run "./dry_run$exe" "$archive" "$work/dist" "$home")
version=$("$installed" --version | awk '{print $NF}')
step "installed Fennec $version at $installed"
# What isn't in a kit runs with the version set for scripts.
run fennec default "$version"

printed="$work/printed.txt"
failed=0
check() {
  local what=$1 expected=$2
  shift 2
  if ! run "$within" 120 "$@" > "$printed" 2> "$work/said.txt"; then
    echo "::error::$what failed:"
    cat "$work/said.txt"
    failed=1
  elif [ -n "$expected" ] && ! diff --strip-trailing-cr "$expected" "$printed" > /dev/null; then
    echo "::error::$what printed what it shouldn't:"
    diff --strip-trailing-cr "$expected" "$printed" || true
    failed=1
  fi
}

step "every example"
for program in examples/*.fen; do
  check "fennec run $program" "${program%.fen}.out" fennec run "$program"
  check "fennec test $program" "" fennec test "$program"
done
step "every example kit"
for kit in examples/projects/*/; do
  [ -f "$kit/kit.toml" ] || continue
  sed -i.bak "s/^fennec = .*/fennec = \"$version\"/" "$kit/kit.toml" && rm -f "$kit/kit.toml.bak"
  check "fennec run $kit" "${kit%/}.out" fennec run "$kit"
  check "fennec test $kit" "" fennec test "$kit"
done

step "a kit with a dependency, built and run on its own"
mkdir -p kits/greeting/src kits/hello/src
printf 'name = "greeting"\nversion = "0.1.0"\nfennec = "%s"\n' "$version" > kits/greeting/kit.toml
printf 'export def greet(name: str) -> str:\n    f"hello, {name}"\n\ntest "greets":\n    assert greet("Ann") == "hello, Ann"\n' > kits/greeting/src/lib.fen
printf 'name = "hello"\nversion = "0.1.0"\nfennec = "%s"\n\n[dependencies]\ngreeting = { path = "../greeting" }\n' "$version" > kits/hello/kit.toml
printf 'from greeting import greet\n\ndef main(stdout: Stdout):\n    stdout.print(greet("kit"))\n' > kits/hello/src/main.fen
printf 'hello, kit\n' > kits/hello.out
check "fennec run in a kit with a dependency" kits/hello.out sh -c "cd kits/hello && fennec run"
check "fennec test in a kit with a dependency" "" sh -c "cd kits/greeting && fennec test"
check "fennec build in a kit with a dependency" "" sh -c "cd kits/hello && fennec build -o ../../built$exe"
check "the kit's program on its own" kits/hello.out "./built$exe"
check "fennec build --release in a kit with a dependency" "" sh -c "cd kits/hello && fennec build --release -o ../../built_release$exe"
check "the kit's release program on its own" kits/hello.out "./built_release$exe"

step "a program built on its own takes the arguments it's given"
printf 'def main(stdout: Stdout, args: Args):\n    stdout.print(args.len(), args[0], args[1])\n' > told.fen
printf '2 one two\n' > told.out
check "fennec build --release of a program taking arguments" "" fennec build --release told.fen -o "told$exe"
check "a program built on its own, given arguments" told.out "./told$exe" one two

# Nothing it did reached for the checkout: no path of it is in what the install wrote.
if grep -rqF "$repo" "$home" 2> /dev/null; then
  echo "::error::the Fennec home holds a path into the checkout:"
  grep -rlF "$repo" "$home"
  failed=1
fi
exit "$failed"
