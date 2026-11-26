# Sourced, with a target, before anything of a release is built: a release's musl runtime, then
# its archives (`release-build.sh`). It sets the environment that has the same commit build the
# same bytes wherever it's checked out and whenever. C's `__DATE__` is the commit's, and no path of
# this machine's is left in what's built: not from Rust, and not from C or assembly, through flags
# for this target that add to `.cargo/config.toml`'s.
#
#   source .github/scripts/release-env.sh aarch64-unknown-linux-gnu
# shellcheck shell=bash
release_target=${1:?give the target being built, like aarch64-unknown-linux-gnu}
release_os=$(uname -s)
case "$release_os" in MINGW* | MSYS* | CYGWIN*) release_os=Windows ;; esac

# In a container the checkout is someone else's, which git won't read without being told.
SOURCE_DATE_EPOCH="$(git -c safe.directory="$PWD" log -1 --format=%ct)"
[ -n "$SOURCE_DATE_EPOCH" ] || { echo "release-env: couldn't tell when the commit was made" >&2; return 1; }
export SOURCE_DATE_EPOCH
if [ "$release_os" = Windows ]; then
  # The compiler sees Windows' own paths, like `D:\a\fennec\fennec`.
  release_here="$(cygpath -w "$PWD")"
  release_cargo_home="$(cygpath -w "${CARGO_HOME:-$USERPROFILE/.cargo}")"
  release_rustup_home="$(cygpath -w "${RUSTUP_HOME:-$USERPROFILE/.rustup}")"
  # And the linker writes no time into the program's header.
  export RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-Wl,--no-insert-timestamp --remap-path-prefix=$release_here=/fennec --remap-path-prefix=$release_cargo_home=/cargo --remap-path-prefix=$release_rustup_home=/rustup"
else
  release_cargo_home="${CARGO_HOME:-$HOME/.cargo}"
  release_flags="--remap-path-prefix=$PWD=/fennec --remap-path-prefix=$release_cargo_home=/cargo --remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup"
  release_cflags="-ffile-prefix-map=$release_cargo_home=/cargo -ffile-prefix-map=$PWD=/fennec"
  if [ "$release_os" = Darwin ]; then
    # macOS's linker names a program by a hash of it before it's stripped, which notes where each
    # object it linked was and when it was made: the checkout's path is taken out of those, and
    # their times are zero.
    release_flags="$release_flags -C link-arg=-Wl,-reproducible -C link-arg=-Wl,-oso_prefix,$PWD/"
    export ZERO_AR_DATE=1
    # The runtime's C parts are built for the macOS a program is built for, as LLVM's code is,
    # rather than for the machine's SDK.
    export MACOSX_DEPLOYMENT_TARGET=14.0
    # Xcode's linker and compiler lay out what they make differently from one version to the next,
    # so each Mac target is built with one Xcode, by its build number, here and in a rebuild before
    # signing: GitHub's default Xcode moved from 26.5 to 26.6 between two runs. One of that build
    # is used: the one selected, or one named for its version in /Applications, as GitHub's images
    # name theirs. Without it, nothing is built. When the signing Mac's Xcode moves on, the pin is
    # moved here first, in a pull request, and a rebuild has to match before the next signing.
    case "$release_target" in
      aarch64-apple-darwin) release_xcode=27.2 release_xcode_build=27B52 ;;
      x86_64-apple-darwin) release_xcode=26.3 release_xcode_build=17C529 ;;
      *) echo "release-env: no Xcode is pinned for $release_target" >&2; return 1 ;;
    esac
    release_found=""
    for release_developer in "${DEVELOPER_DIR:-}" "$(xcode-select -p 2> /dev/null)" \
      "/Applications/Xcode_$release_xcode.app/Contents/Developer" "/Applications/Xcode_$release_xcode.0.app/Contents/Developer"; do
      [ -n "$release_developer" ] && [ -d "$release_developer" ] || continue
      if [ "$(DEVELOPER_DIR="$release_developer" xcodebuild -version 2> /dev/null | sed -n 's/^Build version //p')" = "$release_xcode_build" ]; then
        release_found=$release_developer
        break
      fi
    done
    if [ -z "$release_found" ]; then
      echo "release-env: $release_target is built with Xcode $release_xcode ($release_xcode_build), and this machine has no Xcode of that build: install it beside yours, as /Applications/Xcode_$release_xcode.app, or select it with DEVELOPER_DIR. This one has $(xcodebuild -version 2> /dev/null | paste -s -d ' ' -)" >&2
      return 1
    fi
    export DEVELOPER_DIR="$release_found"
  else
    # GCC doesn't hand its prefix maps to the assembler, which notes where each assembly file
    # was, as ring's are, in what it writes: `/github/home/.cargo/registry/...` in CI's container.
    release_cflags="$release_cflags -Wa,--debug-prefix-map,$release_cargo_home=/cargo -Wa,--debug-prefix-map,$PWD=/fennec"
  fi
  export RUSTFLAGS="$release_flags"
  export "CFLAGS_${release_target//-/_}=$release_cflags"
fi
