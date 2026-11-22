#!/usr/bin/env bash
# mutants-equivalent.sh OUT: takes the mutants `.github/mutants-equivalent.txt` names out of what
# cargo-mutants listed in OUT (`mutants.out`) as missed, into `equivalent.txt` beside it. Each is a
# change no test can catch, since the program still does just what it did, at most slower, and the
# list says why. Run before anything reads `missed.txt`.
#
# A mutant is matched by its file, what cargo-mutants says it does, and the whole text of the line
# it changes, leading and trailing spaces aside, never by the line's number. So the list holds as
# code above it moves, and a line changed anywhere is a missed mutant again, to be looked at
# afresh.
#
# `MUTANTS_EQUIVALENT` names another list and `MUTANTS_SOURCE` another folder the files are in,
# for the tests in `test-mutants.sh`.
set -euo pipefail
out=$1
here="$(cd "$(dirname "$0")/../.." && pwd)"
list=${MUTANTS_EQUIVALENT:-$here/.github/mutants-equivalent.txt}
source=${MUTANTS_SOURCE:-$here}
missed=$out/missed.txt
[ -f "$missed" ] || exit 0
# The list's entries, one a line: the file, the mutant, the line's whole text, tab-separated. An entry is
# four lines with a blank line after; a line starting with `#` is a comment.
entries=$(awk 'BEGIN { RS = ""; FS = "\n" } $1 !~ /^#/ && NF >= 4 { print $1 "\t" $2 "\t" $3 }' "$list")
kept=$(mktemp)
trap '/bin/rm -f "$kept"' EXIT
: > "$out/equivalent.txt"
while IFS= read -r mutant; do
  equivalent=no
  # `<file>:<line>:<column>: <what it does>`, as cargo-mutants lists it.
  if [[ "$mutant" =~ ^([^:]+):([0-9]+):[0-9]+:\ (.*)$ ]]; then
    file=${BASH_REMATCH[1]} line=${BASH_REMATCH[2]} what=${BASH_REMATCH[3]}
    text=$(sed -n "${line}s/^[[:space:]]*//; ${line}s/[[:space:]]*\$//; ${line}p" "$source/$file" 2> /dev/null) || text=""
    while IFS=$'\t' read -r named does whole; do
      if [ "$named" = "$file" ] && [ "$does" = "$what" ] && [ -n "$whole" ] && [ "$text" = "$whole" ]; then
        equivalent=yes
        break
      fi
    done <<< "$entries"
  fi
  if [ "$equivalent" = yes ]; then
    echo "$mutant" >> "$out/equivalent.txt"
  else
    echo "$mutant" >> "$kept"
  fi
done < "$missed"
/bin/cp -f "$kept" "$missed"
