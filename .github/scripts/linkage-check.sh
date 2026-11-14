#!/usr/bin/env bash
# What programs that don't ask for SQLite, TLS or cryptography link of them anyway: none of it,
# once everything that should be cut is. A hello world and a program that only reads a file are
# built, their symbols read, and what's found compared with `.github/linkage-known.txt`, the cuts
# still to make on each system:
#
#   .github/scripts/linkage-check.sh target/release/fennec
#
# It fails when a program links something the list doesn't have, which is a new pull, and when it
# no longer links something the list has, so the pull request that cuts it takes its line out. It
# checks itself first: a program that does use SQLite has to be seen linking it.
#
# What it looks for, in the symbols' demangled names: `sqlite3_` or `rusqlite::` (sqlite), `rustls::`
# (tls), `ring::` (crypto), and ring's assembly, `ring_core_..._` (ring-assembly), which has no
# Rust names.
set -u
fennec=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
known=.github/linkage-known.txt
case "$(uname -s)" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *)
    echo "linkage-check: Windows keeps its symbols out of the program, so only macOS and Linux are checked"
    exit 0
    ;;
esac
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# What `program`, built at `file`, links of what it's checked for, a line each.
found() {
  local symbols
  symbols=$(nm -C "$2" 2> /dev/null) || { echo "::error::couldn't read the symbols of $1" >&2; return 1; }
  grep -qE 'sqlite3_|rusqlite::' <<< "$symbols" && echo "$1 sqlite"
  grep -qE '(^|[^A-Za-z0-9_])rustls::' <<< "$symbols" && echo "$1 tls"
  grep -qE '(^|[^A-Za-z0-9_])ring::' <<< "$symbols" && echo "$1 crypto"
  grep -qE 'ring_core_[0-9_]+_' <<< "$symbols" && echo "$1 ring-assembly"
  return 0
}

# Builds `name` from `source`, as a release build ships.
built() {
  printf '%s' "$2" > "$work/$1.fen"
  if ! .github/scripts/within.sh 120 "$fennec" build --release "$work/$1.fen" -o "$work/$1" > "$work/said" 2>&1; then
    echo "::error::$1 didn't build"
    cat "$work/said"
    exit 1
  fi
}

built uses_sqlite "$(cat examples/std_sqlite.fen)"
if ! found uses_sqlite "$work/uses_sqlite" | grep -q sqlite; then
  echo "::error::a program that uses SQLite wasn't seen linking it, so this check can't see it anywhere"
  exit 1
fi

built hello 'def main(stdout: Stdout):
    stdout.print("hello")
    return
'
built fs 'def main(stdout: Stdout, fs: Fs):
    text = fs.read_text("hello.fen") except why:
        stdout.print(why)
        return
    stdout.print(len(text))
    return
'
{
  found hello "$work/hello"
  found fs "$work/fs"
} | sort > "$work/found"
sed -n "s/^$os \([^|]*[^ |]\) *|.*/\1/p" "$known" | sort > "$work/known"

failed=0
while read -r program what; do
  echo "::error::$program links $what, which it doesn't use: cut it, or add \`$os $program $what | why\` to $known"
  failed=1
done < <(comm -23 "$work/found" "$work/known")
while read -r program what; do
  echo "::error::$program no longer links $what: take \`$os $program $what\` out of $known"
  failed=1
done < <(comm -13 "$work/found" "$work/known")
echo "On $os, what programs link that they don't use:"
sed 's/^/  /' "$work/found"
[ -s "$work/found" ] || echo "  nothing"
exit "$failed"
