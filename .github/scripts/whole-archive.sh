#!/usr/bin/env bash
# Says whether a runtime library is whole: everything of Fennec's own that its objects use is
# defined in it. An incremental build has left one that wasn't, with `fennec_syntax`'s symbols
# missing, which only showed when a program failed to link, as if the program were at fault.
#
#   .github/scripts/whole-archive.sh target/threads/debug/libfennec_runtime.a
#
# Exits 1, naming what's missing, when it isn't.
set -euo pipefail
archive=${1:?give the library to check}
[ -f "$archive" ] || { echo "whole-archive: there's no $archive" >&2; exit 1; }
listed="$(mktemp)"
trap 'rm -f "$listed"' EXIT
# The toolchain's own `nm`, which reads what its compiler writes, or else the system's.
host="$(rustc -vV 2> /dev/null | sed -n 's/^host: //p')"
nm="$(rustc --print sysroot 2> /dev/null)/lib/rustlib/$host/bin/llvm-nm"
[ -x "$nm" ] || nm="nm"
# Every external symbol each object defines or uses, as `U name` or `<kind> name`. `nm` fails on
# an object with no symbols, which is no fault of the library's, so an empty list is what fails.
{ "$nm" -g "$archive" 2> /dev/null || true; } | awk 'NF >= 2 { print $(NF - 1), $NF }' > "$listed"
[ -s "$listed" ] || { echo "whole-archive: $nm read no symbols from $archive" >&2; exit 1; }
missing="$(awk '
  $1 == "U" { used[$2] = 1; next }
  { defined[$2] = 1 }
  END { for (name in used) if (!(name in defined) && name ~ /fennec_/) print name }
' "$listed" | sort | awk 'NR <= 5')"
if [ -n "$missing" ]; then
  echo "whole-archive: $archive uses what it doesn't define, like:" >&2
  echo "$missing" >&2
  exit 1
fi
