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
# The hello world, built for release, is held to the size the list's `<system> size hello <bytes>`
# line gives too, on macOS without the padding of its segments to whole pages (below): a release program carries none of the runtime's debug information, and nothing
# it doesn't call, so one that grows past it has taken on something new.
#
# What it looks for, in the symbols' demangled names: `sqlite3_` or `rusqlite::` (sqlite), `rustls::`
# (tls), `ring::` (crypto), and ring's assembly, `ring_core_..._` (ring-assembly), which has no
# Rust names.
#
# A name that's only a second name for other code isn't counted. A release build of the runtime
# keeps one body for functions that come out the same and every one's name for it, so the code
# that grows a list of anything two words wide is also named `RawVec<rusqlite::Connection>::grow_one`
# in a program with nothing of SQLite's in it. What's counted is a name at an address that no name
# outside what's looked for has too.
set -u
fennec=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
known=.github/linkage-known.txt
# Windows keeps a program's symbols out of it, so there only the hello world's size is checked.
symbols=yes
case "$(uname -s)" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) os=windows symbols=no ;;
esac
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# What `program`, built at `file`, links of what it's checked for, a line each.
found() {
  local symbols
  symbols=$(nm -C "$2" 2> /dev/null) || { echo "::error::couldn't read the symbols of $1" >&2; return 1; }
  symbols=$(awk -v wanted='sqlite3_|rusqlite::|(^|[^A-Za-z0-9_])rustls::|(^|[^A-Za-z0-9_])ring::|ring_core_[0-9_]+_' '
    { line[NR] = $0 }
    $1 ~ /^[0-9a-fA-F]+$/ && NF >= 3 {
      name = $0
      sub(/^[0-9a-fA-F]+ +[A-Za-z] +/, "", name)
      if (name ~ wanted) at[NR] = $1; else other[$1] = 1
    }
    END { for (n = 1; n <= NR; n++) if (!(n in at) || !(at[n] in other)) print line[n] }
  ' <<< "$symbols")
  grep -qE 'sqlite3_|rusqlite::' <<< "$symbols" && echo "$1 sqlite"
  grep -qE '(^|[^A-Za-z0-9_])rustls::' <<< "$symbols" && echo "$1 tls"
  grep -qE '(^|[^A-Za-z0-9_])ring::' <<< "$symbols" && echo "$1 crypto"
  grep -qE 'ring_core_[0-9_]+_' <<< "$symbols" && echo "$1 ring-assembly"
  return 0
}

# Builds `name` from `source`, as a release build ships.
# A program, from its file, built as `$work/<name>`.
built() {
  cp "$2" "$work/$1.fen"
  if ! .github/scripts/within.sh 120 "$fennec" build --release "$work/$1.fen" -o "$work/$1" > "$work/said" 2>&1; then
    echo "::error::$1 didn't build"
    cat "$work/said"
    exit 1
  fi
}

if [ "$symbols" = yes ]; then
  built uses_sqlite examples/std_sqlite.fen
  if ! found uses_sqlite "$work/uses_sqlite" | grep -q sqlite; then
    echo "::error::a program that uses SQLite wasn't seen linking it, so this check can't see it anywhere"
    exit 1
  fi
fi

built hello .github/programs/hello.fen
built fs .github/programs/reads-a-file.fen
if [ "$symbols" = yes ]; then
  {
    found hello "$work/hello"
    found fs "$work/fs"
  } | sort > "$work/found"
else
  # What a program links can't be told here, so nothing's said about it either way.
  grep -v "^$os size " "$known" | sed -n "s/^$os \([^|]*[^ |]\) *|.*/\1/p" | sort > "$work/found"
fi
grep -v "^$os size " "$known" | sed -n "s/^$os \([^|]*[^ |]\) *|.*/\1/p" | sort > "$work/known"

failed=0
while read -r program what; do
  echo "::error::$program links $what, which it doesn't use: cut it, or add \`$os $program $what | why\` to $known"
  failed=1
done < <(comm -23 "$work/found" "$work/known")
while read -r program what; do
  echo "::error::$program no longer links $what: take \`$os $program $what\` out of $known"
  failed=1
done < <(comm -13 "$work/found" "$work/known")
budget=$(sed -n "s/^$os size hello \([0-9]*\) *|.*/\1/p" "$known")
# Built as `hello.exe` on Windows.
size=$(cat "$work/hello" "$work/hello.exe" 2> /dev/null | wc -c | tr -d ' ')
# On macOS, the file grows a whole 16 KB page at a time: each segment but the last is padded to a
# page. A change of 104 bytes once took the file a page past its budget. So what's held there is
# what the program holds: every section's own size, but those that are only zeros when it runs,
# like `__bss`, which take no room in the file, and all of `__LINKEDIT`, its symbols and signature,
# which isn't padded. On Linux, LLD lays each segment right after the last in the file, and on
# Windows a section is padded to 512 bytes at most, so their file sizes grow as the program does.
if [ "$os" = macos ]; then
  file_size=$size
  size=$(size -m -l "$work/hello" | awk -v whole="$file_size" '
    $1 == "Section" && !/zerofill/ { sections += $3 }
    $1 == "Segment" && $2 == "__LINKEDIT:" { linkedit = whole - $NF }
    END { if (sections > 0 && linkedit > 0) print sections + linkedit }
  ')
  if [ -z "$size" ] || [ "$size" -gt "$file_size" ]; then
    echo "::error::couldn't read the hello world's sections with \`size -m -l\`"
    size=$file_size
    failed=1
  fi
fi
if [ -z "$budget" ]; then
  echo "::error::$known has no \`$os size hello <bytes> | why\` line, to hold a hello world's size to"
  failed=1
elif [ "$size" -gt "$budget" ]; then
  echo "::error::a hello world built for release is $size bytes, past its budget of $budget: see what it took on"
  failed=1
fi
if [ "$os" = macos ]; then
  echo "On $os, a hello world built for release holds $size bytes of sections and \`__LINKEDIT\`, in a file of $file_size, of a budget of ${budget:-none}."
else
  echo "On $os, a hello world built for release is $size bytes, of a budget of ${budget:-none}."
fi
echo "On $os, what programs link that they don't use:"
sed 's/^/  /' "$work/found"
[ -s "$work/found" ] || echo "  nothing"
exit "$failed"
