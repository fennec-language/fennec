#!/usr/bin/env bash
# A real program run for a long time under its own scripted load, to see that what it holds levels
# off: its resident memory, the runtime's count of live blocks, the most any running task's list
# of what it made holds (`task held`, #1794), and the freed blocks every thread keeps for reuse are
# read every second, the lowest of each minute kept, and a line fitted through those after a
# warm-up has to rise by less than a small part of where it started. The pages of the runtime's
# tables of blocks are read too and shown, as they rise and fall between looks for empty ones. The lowest, because a reading in the middle of a round holds what that round has in
# flight, which comes and goes: what a leak leaves is the floor between rounds creeping up.
#
#   .github/scripts/soak.sh target/release/fennec bench/realworld/game_server.fen soak.md [minutes]
#
# The program's `main` runs its load once and ends, so it's built as a copy that runs the original's
# over and over, with the same capabilities, until it's stopped: one process, round after round,
# as a server lives. With SOAK_IN=task, the default, every round runs inside one launched task that
# lives the whole time, as a server's sessions live in tasks: what a task holds until it ends builds
# up there, where tasks that end with each round give it back every few seconds (#1794). With
# SOAK_IN=main, the rounds run on the program's own thread. A round that fails ends the soak,
# saying why. It's built for release, as it ships.
#
# SOAK_WAITING names the issue a soak is known to fail on, like `#1796`: its failure is then
# reported as waiting on that issue rather than failing the night, and its passing fails it, so the
# name comes out as soon as the fix lands.
#
# Each round prints what the program prints, so the program's output growing is its rounds
# finishing: a minute in which it doesn't grow is a program that's stopped making progress, which
# fails the soak too, as a hung program would otherwise hold level. The live count is what
# `FENNEC_LIVE_REPORT` writes a few times a second (see the runtime's `live`), which says what's
# live rather than what's cached for reuse, as the process's memory can't.
#
# It writes the curve's start, end and slope for each to `soak.md`, and exits 1 when either rises
# past its budget after the warm-up, or the program stops before the time is up. SOAK_WARM is the
# warm-up, 10 minutes unless set, and SOAK_RISE the most either may rise past its start over the
# rest, as a percentage, 10 unless set. Never less than 16 MB of memory or 64 blocks, though: the
# floor of live blocks between rounds is near nothing, where a percentage is no budget at all, and
# a leak of a block a round is thousands of blocks in an hour.
set -u
fennec=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
program=$2
report=$3
minutes=${4:-60}
warm=${SOAK_WARM:-10}
rise=${SOAK_RISE:-10}
where=${SOAK_IN:-task}
waiting=${SOAK_WAITING:-}
name=$(basename "$program" .fen)
case "$where" in
  task | main) ;;
  *) echo "soak: SOAK_IN is task or main, not $where" >&2; exit 1 ;;
esac
work=$(mktemp -d)
trap 'kill "$pid" 2> /dev/null; rm -rf "$work"' EXIT
pid=

# The copy: the original's `main` renamed, rounds of it run one after another, in a task or on the
# program's own thread, and a `main` with the same parameters and failures that runs them. Its
# parameters are the lines between `def main(` and the line that closes it, a parameter a line, as
# `fennec fmt` writes a long signature. A round's failure is said on the program's `stdout`.
SOAK_IN=$where perl -e '
  my ($text) = do { local $/; <STDIN> };
  $text =~ s/^def main\((.*?)\)( fails [^:]+)?:\n/def soak_round($1)$2:\n/ms or die "no main\n";
  my ($params, $fails) = ($1, $2 // "");
  my @names = map { /^\s*(\w+)\s*:/ ? $1 : () } split /,/, $params;
  die "its main takes no stdout to say a failed round on\n" unless grep { $_ eq "stdout" } @names;
  my $args = join ", ", @names;
  my $all = $fails =~ /\bCrashed\b/ ? $fails : ($fails ? "$fails | Crashed" : " fails Crashed");
  print $text, "\n\n# Every round, one after another, until the soak stops it.\n",
    "def soak_rounds($params):\n    while True:\n        soak_round($args) except why:\n",
    "            stdout.print(f\"soak: a round failed: {why}\")\n            return\n\n";
  if ($ENV{SOAK_IN} eq "task") {
    print "def main($params)$all:\n    with launch(() => soak_rounds($args))?:\n        pass\n    return\n";
  } else {
    print "def main($params):\n    soak_rounds($args)\n    return\n";
  }
' < "$program" > "$work/$name.fen" || exit 1
if ! "$fennec" build --release "$work/$name.fen" -o "$work/$name" > "$work/built" 2>&1; then
  echo "::error::$name didn't build for the soak"
  cat "$work/built"
  exit 1
fi

# First, the program as it is, one round run to its end under `FENNEC_COUNTS`: its counts line says
# how many waits blocked their thread rather than parking, as `blocked_waits N` when there were
# any: a network wait outside a task, or a task's blocking work that couldn't park and ran where it
# was (#1815). A server should have none, and one that blocks blocks every round, so one shows it.
if ! "$fennec" build --release "$program" -o "$work/once" > "$work/built" 2>&1; then
  echo "::error::$name didn't build as it is"
  cat "$work/built"
  exit 1
fi
FENNEC_COUNTS=1 TMPDIR="$work" "$work/once" > /dev/null 2> "$work/once-said" || true
counted=$(grep -E ' live [0-9]+$' "$work/once-said" | tail -n 1)
blocked=$(sed -nE 's/.* blocked_waits ([0-9]+).*/\1/p' <<< "$counted")

# Started in a folder of its own, its output thrown away, and read every minute.
FENNEC_LIVE_REPORT="$work/live" TMPDIR="$work" "$work/$name" > "$work/printed" 2> "$work/said" &
pid=$!
echo "minute rss_kb live_blocks printed_lines task_held spare_words table_pages" > "$work/samples"
stopped=no
stalled=no
printed=0
for minute in $(seq 1 "$minutes"); do
  rss='' live='' held='' spare='' pages=''
  for _ in $(seq 1 60); do
    sleep 1
    kill -0 "$pid" 2> /dev/null || break
    now=$(ps -o rss= -p "$pid" | tr -d ' ')
    [ -n "$now" ] && { [ -z "$rss" ] || [ "$now" -lt "$rss" ]; } && rss=$now
    now=$(sed -n 's/^blocks //p' "$work/live" 2> /dev/null)
    [ -n "$now" ] && { [ -z "$live" ] || [ "$now" -lt "$live" ]; } && live=$now
    # The most any running task's list of what it made holds, where the runtime says (#1794).
    now=$(sed -n 's/^task held //p' "$work/live" 2> /dev/null)
    [ -n "$now" ] && { [ -z "$held" ] || [ "$now" -lt "$held" ]; } && held=$now
    # What every thread's heap keeps for reuse, and the pages of its tables of blocks: caches,
    # bounded by design, so a rise in the floor of either says which one holds what the memory
    # does.
    now=$(sed -n 's/^cached spare //p' "$work/live" 2> /dev/null)
    [ -n "$now" ] && { [ -z "$spare" ] || [ "$now" -lt "$spare" ]; } && spare=$now
    now=$(sed -n 's/^cached pages //p' "$work/live" 2> /dev/null)
    [ -n "$now" ] && { [ -z "$pages" ] || [ "$now" -lt "$pages" ]; } && pages=$now
  done
  if ! kill -0 "$pid" 2> /dev/null; then
    stopped=$minute
    break
  fi
  # Its rounds finishing, by what it's printed.
  now=$(wc -l < "$work/printed" | tr -d ' ')
  [ "$now" -gt "$printed" ] || [ "$stalled" != no ] || stalled=$minute
  printed=$now
  echo "$minute ${rss:-0} ${live:-0} $printed ${held:-0} ${spare:-0} ${pages:-0}" >> "$work/samples"
  echo "$name $([ "$where" = task ] && echo "in a task" || echo "on its own thread"), minute $minute: at least ${rss:-?} KB resident and ${live:-?} live blocks, $printed lines printed"
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
  echo "## Soak: $name, its rounds in $([ "$where" = task ] && echo "one long task" || echo "its own thread")"
  echo
  if [ -z "$counted" ]; then
    echo "One round run to its end said no counts, so whether its waits blocked can't be told."
    echo
    failed=1
  elif [ "${blocked:-0}" -gt 0 ]; then
    echo "One round blocked a thread $blocked times rather than parking: \`$counted\`."
    echo
    failed=1
  else
    echo "One round run to its end blocked no thread: \`$counted\`."
    echo
  fi
  if [ "$stalled" != no ]; then
    echo "No round finished in minute $stalled: it stopped making progress, so its curves say nothing."
    echo
    failed=1
  fi
  if [ "$stopped" != no ]; then
    echo "It stopped in minute $stopped, before the $minutes it should have run:"
    echo
    echo '```'
    grep 'soak: a round failed' "$work/printed" | tail -1
    tail -20 "$work/said"
    echo '```'
    failed=1
  fi
  # Each measure, its column and the least rise that counts against it.
  # A task's list of what it made, held to a bound since #1794, which a leak in it passes.
  # What's kept for reuse, in words, may rise by a megabyte's worth: it fills as a program warms up
  # and then has to stay within its bounds.
  for what in "resident memory, KB:2:16384" "live blocks:3:64" "a task's list of what it made:5:4096" "freed blocks kept for reuse, words:6:131072"; do
    least=${what##*:}
    what=${what%:*}
    read -r start end slope <<< "$(fit "${what##*:}")"
    if [ "$start" = none ]; then
      echo "- ${what%:*}: too few samples after the $warm-minute warm-up to fit"
      failed=1
      continue
    fi
    # Past the budget: a rise over the window of more than `rise` percent of where it started, and
    # more than the least that counts.
    over=$(awk -v s="$start" -v e="$end" -v r="$rise" -v l="$least" 'BEGIN { b = s * r / 100; if (b < l) b = l; print (e - s > b) ? 1 : 0 }')
    verdict=$([ "$over" -eq 1 ] && echo "rising past its budget, $rise% or $least" || echo "level")
    echo "- ${what%:*}: $start at minute $((warm + 1)), $end at the end, $slope a minute: $verdict"
    [ "$over" -eq 1 ] && failed=1
  done
  echo
  echo "Each minute's lowest reading, resident memory in KB, live blocks, the lines it had printed, the most a task's list held, the words of freed blocks kept for reuse, and the pages of the tables of blocks:"
  echo
  echo '```'
  cat "$work/samples"
  echo '```'
} >> "$report"
if [ -n "$waiting" ]; then
  if [ "$failed" -eq 1 ]; then
    echo "Known: it fails until $waiting is fixed, so this doesn't fail the night." >> "$report"
    echo "::warning::the soak of $name $where fails, as it's known to until $waiting is fixed"
    exit 0
  fi
  echo "It passes now: $waiting looks fixed, so take SOAK_WAITING out of its job." >> "$report"
  echo "::error::the soak of $name $where passes now: take SOAK_WAITING=$waiting out of its job"
  exit 1
fi
[ "$failed" -eq 0 ] || echo "::error::the soak of $name failed: see its report"
exit "$failed"
