#!/usr/bin/env bash
# What a night's slice of mutants came to, as Markdown: how many were tried and how each ended,
# then the files with the most mutants no test caught first, each with its survivors. A survivor
# is a change to the code that every test still passes with: a spot no test checks.
#
#   .github/scripts/mutants-ranked.sh mutants.out fennec-rt 17/60
#
# It reads what cargo-mutants wrote in `mutants.out`, which it writes as it goes, so a slice stopped
# at its time limit still says what it found.
set -u
out=$1
crate=$2
slice=$3
count() { [ -f "$out/$1.txt" ] && grep -c . "$out/$1.txt" || echo 0; }
missed=$(count missed)
caught=$(count caught)
timeout=$(count timeout)
unviable=$(count unviable)
echo "## $crate, slice $slice"
echo
echo "$((missed + caught + timeout + unviable)) mutants tried: $missed missed, $caught caught, $timeout hung (which counts as caught), $unviable didn't build."
[ "$missed" -eq 0 ] && exit 0
echo
echo "Missed, by file, most first:"
echo
# The file is what comes before the line number.
sed -E 's/^([^:]+):[0-9]+:.*/\1/' "$out/missed.txt" | sort | uniq -c | sort -k1,1nr -k2 | while read -r n file; do
  echo "- \`$file\`: $n"
  grep -F "$file:" "$out/missed.txt" | sed -E 's/^[^:]+:([0-9]+):[0-9]+: (.*)/  - line \1: \2/'
done
