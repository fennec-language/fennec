#!/usr/bin/env bash
# A real program run for a long time under its own scripted load, to see that what it holds levels
# off: its resident memory and the runtime's count of live blocks are read every minute, and a line
# fitted through them after a warm-up has to rise by less than a small part of where it started.
#
#   .github/scripts/soak.sh target/release/fennec bench/realworld/game_server.fen soak.md [minutes]
#
# The program's `main` runs its load once and ends, so it's built as a copy whose `main` runs the
# original's over and over, with the same capabilities, until it's stopped: one process, round
# after round, as a server lives. It's built for release, as it ships. The live count is what
# `FENNEC_LIVE_REPORT` writes a few times a second (see the runtime's `live`), which says what's
# live rather than what's cached for reuse, as the process's memory can't.
#
# It writes the curve's start, end and slope for each to `soak.md`, and exits 1 when either rises
# past its budget after the warm-up, or the program stops before the time is up. SOAK_WARM is the
# warm-up, 10 minutes unless set, and SOAK_RISE the most either may rise past its start over the
# rest, as a percentage, 10 unless set.
set -u
fennec=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
program=$2
report=$3
minutes=${4:-60}
warm=${SOAK_WARM:-10}
rise=${SOAK_RISE:-10}
name=$(basename "$program" .fen)
work=$(mktemp -d)
trap 'kill "$pid" 2> /dev/null; rm -rf "$work"' EXIT
pid=

# The copy: the original's `main` renamed, and a `main` with the same parameters and failures that
# runs it round after round. Its parameters are the lines between `def main(` and the line that
# closes it, a parameter a line, as `fennec fmt` writes a long signature.
perl -e '
  my ($text) = do { local $/; <STDIN> };
  $text =~ s/^def main\((.*?)\)( fails [^:]+)?:\n/def soak_round($1)$2:\n/ms or die "no main\n";
  my ($params, $fails) = ($1, $2 // "");
  my @names = map { /^\s*(\w+)\s*:/ ? $1 : () } split /,/, $params;
  my $args = join ", ", @names;
  print $text, "\n\n# Round after round, until the soak stops it.\ndef main($params)$fails:\n    while True:\n        soak_round($args)?\n";
' < "$program" > "$work/$name.fen" || exit 1
if ! "$fennec" build --release "$work/$name.fen" -o "$work/$name" > "$work/built" 2>&1; then
  echo "::error::$name didn't build for the soak"
  cat "$work/built"
  exit 1
fi

# Started in a folder of its own, its output thrown away, and read every minute.
FENNEC_LIVE_REPORT="$work/live" TMPDIR="$work" "$work/$name" > /dev/null 2> "$work/said" &
pid=$!
echo "minute rss_kb live_blocks" > "$work/samples"
stopped=no
for minute in $(seq 1 "$minutes"); do
  sleep 60
  if ! kill -0 "$pid" 2> /dev/null; then
    stopped=$minute
    break
  fi
  rss=$(ps -o rss= -p "$pid" | tr -d ' ')
  live=$(sed -n 's/^blocks //p' "$work/live" 2> /dev/null)
  echo "$minute ${rss:-0} ${live:-0}" >> "$work/samples"
  echo "$name, minute $minute: ${rss:-?} KB resident, ${live:-?} live blocks"
done
kill "$pid" 2> /dev/null
wait "$pid" 2> /dev/null

# A line fitted through the samples after the warm-up, by least squares: where it starts and ends
# over that window, and how much it rises a minute.
fit() {
  awk -v column="$1" -v warm="$warm" 'NR > 1 && $1 > warm {
      n++; x += $1; y += $column; xx += $1 * $1; xy += $1 * $column; first = first == "" ? $1 : first; last = $1
    }
    END {
      if (n < 3) { print "none"; exit }
      slope = (n * xy - x * y) / (n * xx - x * x); at = (y - slope * x) / n
      printf "%.0f %.0f %.2f\n", at + slope * first, at + slope * last, slope
    }' "$work/samples"
}
failed=0
{
  echo "## Soak: $name"
  echo
  if [ "$stopped" != no ]; then
    echo "It stopped in minute $stopped, before the $minutes it should have run:"
    echo
    echo '```'
    tail -20 "$work/said"
    echo '```'
    failed=1
  fi
  for what in "resident memory, KB:2" "live blocks:3"; do
    read -r start end slope <<< "$(fit "${what##*:}")"
    if [ "$start" = none ]; then
      echo "- ${what%:*}: too few samples after the $warm-minute warm-up to fit"
      failed=1
      continue
    fi
    # Past the budget: a rise over the window of more than `rise` percent of where it started.
    over=$(awk -v s="$start" -v e="$end" -v r="$rise" 'BEGIN { print (e - s > s * r / 100 && e - s > 0) ? 1 : 0 }')
    verdict=$([ "$over" -eq 1 ] && echo "rising past the $rise% budget" || echo "level")
    echo "- ${what%:*}: $start at minute $((warm + 1)), $end at the end, $slope a minute: $verdict"
    [ "$over" -eq 1 ] && failed=1
  done
  echo
  echo "Every minute's sample, resident memory in KB and live blocks:"
  echo
  echo '```'
  cat "$work/samples"
  echo '```'
} >> "$report"
[ "$failed" -eq 0 ] || echo "::error::the soak of $name failed: see its report"
exit "$failed"
