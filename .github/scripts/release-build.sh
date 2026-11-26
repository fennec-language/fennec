#!/usr/bin/env bash
# Builds a release's two archives for one platform from the checkout it's run in: the toolchain's,
# `fennec-v<version>-<target>`, and the launcher's, `fennec-launcher-v<version>-<target>`. The
# release workflow runs it for each platform, twice, and so does a person rebuilding a release
# before signing it (`rebuild-release.sh`). Run the same way from the same commit, it makes the
# same bytes, wherever the checkout is and whenever it's run.
#
#   .github/scripts/release-build.sh aarch64-apple-darwin
#
# On Linux, `musl/` in the checkout holds the runtime built for musl (`musl-runtime.sh`). On
# Windows, `windows.sh toolchain` has run first. It prints the two archives' names, one a line, as
# its last two lines, and in CI also writes them to `$GITHUB_ENV` as `ARCHIVE`, `LAUNCHER_ARCHIVE`
# and `PACKAGED`, the toolchain's folder.
set -euo pipefail
target=${1:?give the target to build for, like aarch64-apple-darwin}
os=$(uname -s)
case "$os" in MINGW* | MSYS* | CYGWIN*) os=Windows ;; esac

# shellcheck source=.github/scripts/release-env.sh
source "$(dirname "$0")/release-env.sh" "$target"

# Installed by name before any other `rustup` command, which would otherwise install it on the way,
# as rustup is deprecating. After `windows.sh toolchain`, on Windows it's gnullvm's.
rustup toolchain install
rustup target add "$target"
# `fennec build` links executables against the runtime library, shipped next to `fennec`.
cargo build --release --locked -p fennec -p fennec-runtime --target "$target"
# The launcher people install as `fennec`, with the two programs it hands work to, built on its
# own so it gets none of the toolchain's features.
cargo build --release --locked -p fennec-launcher --bins --target "$target"
# The runtime with threads, for programs that use `parallel`, also next to `fennec`. Built on its
# own, in a folder of its own: built with the others, Cargo would give all of them `threads`.
cargo rustc --release --locked -p fennec-runtime --features threads --target "$target" --target-dir target/threads --crate-type staticlib -- --emit=llvm-bc,link -C lto=fat -C codegen-units=1
cp "target/threads/$target/release/libfennec_runtime.a" "target/$target/release/libfennec_runtime_threads.a"
cp "$(find "target/threads/$target/release/deps" -name 'fennec_runtime-*.bc' -print -quit)" "target/$target/release/libfennec_runtime_threads.bc"
# A release build puts what it uses of the runtime in the program's own unit, from this.
cargo rustc --release --locked -p fennec-runtime --target "$target" --crate-type staticlib -- --emit=llvm-bc,link -C lto=fat -C codegen-units=1
cp "$(find "target/$target/release/deps" -name 'fennec_runtime-*.bc' -print -quit)" "target/$target/release/libfennec_runtime.bc"

channel="$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)"
if [ "$os" = Windows ]; then
  toolchain="$(rustup show home)/toolchains/$channel-x86_64-pc-windows-msvc"
else
  # LLVM's tools for the target's own machine, as `fennec` runs them. An archive lays them out as
  # the toolchain does, which is where `fennec` looks: see `Tools::find`.
  rustup toolchain install "$channel-$target" --profile minimal --component llvm-tools-preview --force-non-host --no-self-update
  toolchain="$(rustup show home)/toolchains/$channel-$target"
fi

# Named by the version it builds as, which a tag has to match: the launcher takes an archive only
# when its folder inside is named as the file is, by that version.
version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml)"
name="fennec-v${version}-${target}"
rm -rf "$name"
mkdir "$name"
cp README.md LICENSE-MIT LICENSE-APACHE RUNTIME-LIBRARY-EXCEPTION THIRD-PARTY.md "$name/"
from="target/${target}/release"
for runtime in libfennec_runtime libfennec_runtime_threads; do
  cp "$from/$runtime.a" "$from/$runtime.bc" "$name/"
done
mkdir -p "$name/licenses"
if [ "$os" = Windows ]; then
  cp "$from/fennec.exe" "$name/"
  # LLVM's tools from Rust's MSVC toolchain, LLD, and the libraries it links.
  .github/scripts/windows.sh folder "$name/llvm"
  cp licenses/mingw-w64-runtime.txt "$name/licenses/"
else
  cp "$from/fennec" "$name/"
  # Which protocol its build daemon speaks, as what version and build, so the launcher can ask
  # that daemon for a plain `fennec run` itself (`fennec_daemon::PROTOCOL_FILE`).
  "$from/fennec" daemon protocol > "$name/daemon-protocol"
  tools="$toolchain/lib/rustlib/$target"
  mkdir -p "$name/llvm/bin" "$name/llvm/lib"
  # LLVM's `ar` takes the runtime's Rust out of its library, so building needs no `ar` from the
  # machine. On Linux LLD links a program too, so it needs no C compiler or linker either; a Mac
  # links through Xcode's command line tools.
  cp "$tools"/bin/{opt,llc,llvm-link,llvm-ar} "$name/llvm/bin/"
  if [ "$os" = Linux ]; then
    cp "$tools/bin/rust-lld" "$name/llvm/bin/"
  fi
  # The tools share LLVM from a library of their own, which they find in `../lib`.
  cp "$toolchain"/lib/libLLVM* "$name/llvm/lib/"
fi
# On Linux, what a release build links a program with to make one that runs on its own. Its
# programs hold musl's notices themselves, and the archive has them too.
if [ -d musl ]; then
  cp -R musl "$name/musl"
  cp licenses/musl.txt "$name/licenses/"
fi
licenses="$toolchain/share/doc/rust/licenses"
cp "$licenses/Apache-2.0.txt" "$licenses/LLVM-exception.txt" "$licenses/NCSA.txt" "$name/licenses/"
cp "$toolchain/share/doc/rust/COPYRIGHT.html" "$name/licenses/rust-COPYRIGHT.html"
archive="$(.github/scripts/pack.sh "$name")"

# The launcher's archive: its three programs in one folder named as the file is, which `fennec self
# update` puts in `~/.fennec/bin`, and the notices they're shipped with.
exe=""
if [ "$os" = Windows ]; then exe=".exe"; fi
launcher="fennec-launcher-v${version}-${target}"
rm -rf "$launcher"
mkdir "$launcher"
cp LICENSE-MIT LICENSE-APACHE THIRD-PARTY.md "$launcher/"
cp "$from/fennec-launcher$exe" "$launcher/fennec$exe"
cp "$from/fennec-setup$exe" "$from/fennec-fetch$exe" "$launcher/"
launcher_archive="$(.github/scripts/pack.sh "$launcher")"

if [ -n "${GITHUB_ENV:-}" ]; then
  {
    echo "ARCHIVE=$archive"
    echo "LAUNCHER_ARCHIVE=$launcher_archive"
    echo "PACKAGED=$name"
  } >> "$GITHUB_ENV"
fi
ls -l "$archive" "$launcher_archive" >&2
echo "$archive"
echo "$launcher_archive"
