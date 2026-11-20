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
# One count, from what cargo-mutants listed: 0 for a list that's empty or not there. `grep -c`
# prints its 0 and fails then, so `|| echo 0` once made it "0" twice, and the line below was never
# written.
count() {
  local n
  n=$(grep -c . "$out/$1.txt" 2> /dev/null) || true
  echo "${n:-0}"
}
missed=$(count missed)
caught=$(count caught)
timeout=$(count timeout)
unviable=$(count unviable)
echo "## $crate, slice $slice"
echo
tried=$((missed + caught + timeout + unviable))
# How many the slice has, which cargo-mutants lists before it starts.
found=$(jq length "$out/mutants.json" 2> /dev/null) || found=""
said=$tried
[ -z "$found" ] || said="$tried of $found"
echo "$said mutants tried: $caught caught, $missed missed, $timeout timed out, $unviable didn't build."
if [ -n "$found" ] && [ "$tried" -lt "$found" ]; then
  echo
  echo "**$((found - tried)) weren't tried before the slice's time ran out.**"
fi
# A timeout is neither: the tests ran past the limit before saying either way.
if [ $((2 * timeout)) -gt $((caught + missed + timeout)) ]; then
  echo
  echo "**Most of those that built timed out, so this slice checked next to nothing.**"
fi
[ "$missed" -eq 0 ] && exit 0
echo
echo "Missed, by file, most first:"
echo
# The file is what comes before the line number.
sed -E 's/^([^:]+):[0-9]+:.*/\1/' "$out/missed.txt" | sort | uniq -c | sort -k1,1nr -k2 | while read -r n file; do
  echo "- \`$file\`: $n"
  grep -F "$file:" "$out/missed.txt" | sed -E 's/^[^:]+:([0-9]+):[0-9]+: (.*)/  - line \1: \2/'
done
