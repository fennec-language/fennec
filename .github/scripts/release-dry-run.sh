#!/usr/bin/env bash
# A release's dry run: installs the launcher's archive and the toolchain's archive the release
# workflow built, through the launcher's own code, into a Fennec home with nothing in it. Then,
# through that launcher alone, it makes a kit with `fennec new`, and builds, runs and tests it, every
# example, every example kit and a kit with a dependency. Nothing it runs reaches this
# checkout, Rust's toolchain, or anything set up for development: a step that needs them fails
# here rather than on a person's machine.
#
#   .github/scripts/release-dry-run.sh fennec-v0.4.2-aarch64-apple-darwin.tar.gz #     fennec-launcher-v0.4.2-aarch64-apple-darwin.tar.gz [target]
#
# The dry run's installer installs the archives for the platform it's built for, so it's built for
# the archives' own: on Windows that's `x86_64-pc-windows-gnullvm`, as the toolchain is, not the
# MSVC target Rust builds for there by default.
#
# Nobody has signed the archives, so `crates/launcher/examples/dry_run.rs` signs them with keys
# made for the run, which only that run trusts, lays them out as the bucket is, and installs them:
# the launcher as `fennec self update` replaces one, and the toolchain as a kit's first run installs
# it. From then on it's the launcher from the archive, run as `fennec`, which trusts the release
# keys alone and finds the toolchain installed.
set -euo pipefail
archive=$(cd "$(dirname "${1:?give the toolchain archive to try}")" && pwd)/$(basename "$1")
launcher=$(cd "$(dirname "${2:?and the launcher archive}")" && pwd)/$(basename "$2")
repo=$(cd "$(dirname "$0")/../.." && pwd)
exe=""
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) exe=".exe" ;; esac

target=${3:-}
# Built while the checkout and Rust are still in reach: the dry run's installer. The launcher is
# the archive's.
(cd "$repo" && cargo build --locked --quiet --release -p fennec-launcher --example dry_run ${target:+--target "$target"})
built="$repo/target/${target:+$target/}release"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# The launcher's folder as bash's own `PATH` takes it, which is `/c/...` on Windows.
bin="$work/home/bin"
# On Windows the programs are Windows' own, which read `C:/...` and not Git Bash's `/tmp/...`.
if [ -n "$exe" ]; then
  bin=$(cygpath -u "$work/home/bin")
  work=$(cygpath -m "$work")
  archive=$(cygpath -m "$archive")
  launcher=$(cygpath -m "$launcher")
fi
home="$work/home"
mkdir -p "$home" "$work/dist"
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
  # And what examples' programs run for real: `echo`, and `true`, `printf`, `sleep` and `yes`
  # in `std_process_hostile.fen`, which this list lacked until its dry run failed on them.
  for tool in bash sh env diff cat sed grep awk sort paste tr mkdir rm cp mv ls find dirname basename uname mktemp head tail sleep kill wc echo true printf yes; do
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
step "install $(basename "$launcher") and $(basename "$archive") into an empty home"
installed=$(run "./dry_run$exe" "$archive" "$launcher" "$work/dist" "$home")
version=$("$installed" --version | awk '{print $NF}')
step "installed Fennec $version at $installed"
for program in fennec fennec-setup fennec-fetch; do
  [ -x "$home/bin/$program$exe" ] || { echo "::error::the launcher's archive put no $program in bin"; exit 1; }
done
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

step "the launcher from the archive trusts only the release keys"
if run fennec self update > "$work/said.txt" 2>&1; then
  echo "::error::fennec self update ran with no release keys to check a launcher against"
  failed=1
elif ! grep -q "trusts 0 release keys" "$work/said.txt"; then
  echo "::error::fennec self update refused for another reason:"
  cat "$work/said.txt"
  failed=1
fi

step "a new kit, made, run and tested"
check "fennec new" "" fennec new fresh
printf 'Hello from fresh!\n' > fresh.out
check "fennec run in the new kit" fresh.out sh -c "cd fresh && fennec run"
check "fennec test in the new kit" "" sh -c "cd fresh && fennec test"
grep -qx "fennec = \"$version\"" fresh/kit.toml || { echo "::error::the new kit doesn't ask for $version"; failed=1; }
grep -q "^toolchain $version " fresh/kit.lock || { echo "::error::the new kit's lock doesn't pin $version"; failed=1; }

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
cp -R "$repo/.github/programs/kits" kits
for kit in kits/greeting kits/hello; do
  sed -i.bak "s/^fennec = .*/fennec = \"$version\"/" "$kit/kit.toml" && rm -f "$kit/kit.toml.bak"
done
printf 'hello, kit\n' > kits/hello.out
check "fennec run in a kit with a dependency" kits/hello.out sh -c "cd kits/hello && fennec run"
check "fennec test in a kit with a dependency" "" sh -c "cd kits/greeting && fennec test"
check "fennec build in a kit with a dependency" "" sh -c "cd kits/hello && fennec build -o ../../built$exe"
check "the kit's program on its own" kits/hello.out "./built$exe"
check "fennec build --release in a kit with a dependency" "" sh -c "cd kits/hello && fennec build --release -o ../../built_release$exe"
check "the kit's release program on its own" kits/hello.out "./built_release$exe"

step "a program built on its own takes the arguments it's given"
cp "$repo/.github/programs/told.fen" told.fen
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
