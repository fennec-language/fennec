#!/usr/bin/env bash
# The checks a commit passes on this machine before it lands. It tests the working tree, so whatever
# runs it has to be sure that's the commit in question.
#
# The quick checks come first: formatting, the lints, the ratchets and the pinned counts, which
# take a minute or two where the rest take an hour. One that fails stops the gate there, since
# what it found is a fix to make before an hour of tests is worth spending.
#
# After them, every step runs, whatever an earlier one did, so a failure main already has never
# hides one this change brings: tests run with `--no-fail-fast`, and the examples step goes
# through every program. Only the steps that need the build are left out when it fails,
# and a step that didn't run fails the gate. A step's failures are named: a test by its exact name, a program by
# its path. The gate passes when every step ran and failed on nothing but what
# `.github/known-failures.txt` names, the failures main has, each with the change that broke it.
# It ends with a line for every step: whether it ran, and what it found.
set -u
# Each of these has tests rewrite what they expect from what they get, and pass: set by a shell
# that pinned outputs before, they'd hold the gate to whatever this commit does.
unset FENNEC_PIN_OUTPUTS FENNEC_UPDATE_UNCHECKED
root="$(git rev-parse --show-toplevel)" || exit 1
cd "$root" || exit 1
# One gate a worktree: a second one here builds and tests in the same target folder as the first,
# and each corrupts the other (#2294). Run by open-pr.sh, which holds the lock for it, it goes on
# under that. Run on its own, it takes the lock, and lets it go when it ends.
holder=${FENNEC_GATE_LOCK_HOLDER:-$$}
"$root/.github/scripts/gate-lock.sh" take "$holder" || exit 1
let_go() {
  [ "$holder" != "$$" ] || "$root/.github/scripts/gate-lock.sh" release "$$"
}
trap let_go EXIT
# How much the disk it builds on has free, said loudly first when it's under 40 GB, and again by
# its summary: a gate builds 7 to 13 GB in its worktree, and a build that runs out of room part way
# leaves broken objects behind, as one lane's did with 12 GB free on 2026-11-17. It still runs.
# Under the mark, it first deletes the builds of the worktrees nothing has built in for two hours,
# as the lanes' two-hourly sweep does, since the warning alone left the disk to fill on 2026-11-18.
# GATE_LEAST_FREE_GB sets the mark.
least_free=${GATE_LEAST_FREE_GB:-40}
free_gb=$(df -Pk "$root" 2> /dev/null | awk 'NR == 2 { print int($4 / 1048576) }')
if [[ $free_gb =~ ^[0-9]+$ ]] && [ "$free_gb" -lt "$least_free" ]; then
  echo "gate: only $free_gb GB free, so the builds idle for two hours go first" >&2
  "$root/.github/scripts/sweep-builds.sh" --hours 2 >&2 || echo "gate: the sweep of idle builds failed" >&2
  free_gb=$(df -Pk "$root" 2> /dev/null | awk 'NR == 2 { print int($4 / 1048576) }')
fi
low_disk=''
if [[ $free_gb =~ ^[0-9]+$ ]] && [ "$free_gb" -lt "$least_free" ]; then
  low_disk="WARNING: only $free_gb GB free on the disk this builds on, under $least_free GB: a build that runs out part way leaves broken objects. Free some first: remove the worktrees of merged pull requests, their target folders first."
  printf '\n%s\n%s\n%s\n\n' '!!!!!!!!' "$low_disk" '!!!!!!!!' >&2
fi
logs="$(mktemp -d)"
# Its logs, and the programs and Fennec folder it makes, go however it ends: what a failed step
# printed is shown before then.
trap 'rm -rf "$logs"; let_go' EXIT
log="$logs/gate.log"
: > "$log"
# LLVM is the main backend, so its tests have to run, not skip.
export FENNEC_EXPECT_LLVM=1
# Everything builds in its own process, as a first build and CI do, never through a build daemon
# kept between commands: what's checked and timed is the build on its own.
export FENNEC_NO_DAEMON=1
# Every executable a build makes as a new file, a line each, which macOS may hold at its first
# launch: counted for each step at the end.
export FENNEC_NEW_EXECUTABLES="$logs/new-executables.txt"
: > "$FENNEC_NEW_EXECUTABLES"
# How many each step made, as `name|count`.
made=()

# Each step's outcome, in order, as `name|outcome`: passed, known, failed or didn't run.
outcomes=()
# What main is known to fail, one name a line, from `.github/known-failures.txt`.
known="$logs/known.txt"
.github/scripts/known-failures.sh names > "$known" || exit 1

# Runs a step and keeps its outcome. A step may name its failures in `$names`, one a line: a step
# that fails naming only what main is known to fail is "known"; one that fails naming nothing, or
# anything else, has failed.
names="$logs/names.txt"
step() {
  local name=$1 started=$SECONDS status=0 before
  shift
  : > "$names"
  before=$(wc -l < "$FENNEC_NEW_EXECUTABLES")
  "$@" > "$logs/step.log" 2>&1 || status=$?
  local took=$((SECONDS - started))
  made+=("$name|$(($(wc -l < "$FENNEC_NEW_EXECUTABLES") - before))")
  if [ "$status" -eq 0 ]; then
    echo "${took}s  $name"
    outcomes+=("$name|passed")
    return 0
  fi
  local unknown
  unknown=$(grep -vxF -f "$known" "$names" || true)
  if [ -s "$names" ] && [ -z "$unknown" ]; then
    echo "${took}s  $name: failed only on what main is known to fail:"
    sed 's/^/  /' "$names"
    outcomes+=("$name|known")
    return 0
  fi
  echo "FAILED after ${took}s: $name"
  if [ -n "$unknown" ]; then
    echo "  failures main isn't known to have:"
    printf '%s\n' "$unknown" | sed 's/^/    /'
  fi
  kept_failure "$name"
  tail -n 60 "$logs/step.log"
  grep gate-watch "$log"
  # A corrupt build isn't the code, nor a flake: it's said plainly, with what mends it, and isn't
  # kept for the ledger.
  if .github/scripts/known-failures.sh corrupt-build "$logs/step.log"; then
    echo "  this reads as a corrupt build: undefined \`….llvm.<number>\` symbols, from objects a stopped"
    echo "  cargo left half written. Run .github/scripts/clean-workspace.sh, then the gate again."
  else
    # Kept under the code it tested: if that passes later, this was a flake, and goes in the
    # ledger. See `flakes.sh`.
    .github/scripts/flakes.sh failed "$name" "$logs/step.log"
  fi
  outcomes+=("$name|failed")
  return 1
}

# What a failed step said, kept whole where it outlasts the gate's folder: its whole log, each
# failing test's panic in full, which the 60 lines printed after can cut off, and any temporary
# folder a panic names that's still there. Printed too: each failing test in a line, then each
# panic whole.
kept_failure() {
  local name=$1 kept panics folder
  kept="${FENNEC_HOME:-$HOME/.fennec}/gate-logs/$(date '+%Y-%m-%d-%H%M%S')-$(git rev-parse --short HEAD)-$(printf '%s' "$1" | tr -c 'a-zA-Z0-9' '-' | cut -c1-40)"
  mkdir -p "$kept" || return 0
  cp "$logs/step.log" "$kept/step.log"
  .github/scripts/failed-tests.sh "$logs/step.log" > "$kept/failed-tests.txt"
  # Each test's panic, from its `---- name stdout ----` to the next one, or the list of failures.
  panics="$kept/panics.txt"
  awk '/^---- .* stdout ----$/ { on = 1 } /^failures:$/ && on { on = 0 } on' "$logs/step.log" > "$panics"
  if [ -s "$kept/failed-tests.txt" ]; then
    echo "  each test that failed:"
    sed 's/^/    /' "$kept/failed-tests.txt"
  fi
  if [ -s "$panics" ]; then
    echo "  what each said, whole:"
    sed 's/^/    /' "$panics"
  fi
  # A temporary folder a failing test kept, or its panic names: what a program it built printed,
  # its source, its recording. Moved here, so the temporary folder is left as tidy as a pass leaves
  # it.
  grep -oE '(/private)?/(var/folders|tmp)/[^] ":)]*fennec-[A-Za-z0-9_.-]+' "$panics" 2> /dev/null | sort -u \
    | while read -r folder; do
      [ -d "$folder" ] || continue
      mv "$folder" "$kept/" 2> /dev/null && echo "  kept the folder $folder"
    done
  echo "  all of it is kept in $kept"
}

# A step left out, which fails the gate as surely as one that failed.
skipped() {
  echo "didn't run: $1, $2"
  outcomes+=("$1|didn't run")
}

# A runtime library built a little at a time has come out without parts of what it uses, which a
# program then fails to link with, as if the program were at fault. So each one is checked, and one
# missing parts is built again from nothing, which makes it whole; if it still isn't, the gate stops.
#
#   whole_runtime <library> <target folder> <what cargo builds it with>...
whole_runtime() {
  local library=$1 folder=$2 manifest
  shift 2
  .github/scripts/whole-archive.sh "$library" && return 0
  echo "gate: $library was missing parts, so it's built again from nothing" >&2
  local crates=()
  for manifest in crates/*/Cargo.toml; do
    crates+=(-p "$(sed -n 's/^name = "\(.*\)"$/\1/p' "$manifest" | sed -n 1p)")
  done
  cargo clean --target-dir "$folder" "${crates[@]}" || return 1
  cargo build --locked --target-dir "$folder" "$@" || return 1
  .github/scripts/whole-archive.sh "$library"
}

runtime() {
  cargo build --locked -p fennec -p fennec-runtime || return 1
  whole_runtime target/debug/libfennec_runtime.a target -p fennec -p fennec-runtime || return 1
  # Every variant `fennec build` may take, fresh, so none is left over from another commit.
  for features in threads audit threads,audit; do
    dir="target/$(echo "$features" | tr , -)"
    cargo build --locked -p fennec-runtime --features "$features" --target-dir "$dir" || return 1
    whole_runtime "$dir/debug/libfennec_runtime.a" "$dir" -p fennec-runtime --features "$features" || return 1
    cp "$dir/debug/libfennec_runtime.a" "target/debug/libfennec_runtime_$(echo "$features" | tr , _).a"
  done
}

# `fennec` with these arguments, within its limit, saying what was run and how it ended if it
# fails: a program killed by a signal prints nothing of its own.
fennec_ok() {
  local status=0
  .github/scripts/within.sh 120 ./target/debug/fennec "$@" || status=$?
  [ "$status" -eq 0 ] && return 0
  if [ "$status" -gt 128 ] && [ "$status" -ne 124 ]; then
    echo "\`fennec $*\` was killed by signal $((status - 128))" >&2
  else
    echo "\`fennec $*\` gave $status" >&2
  fi
  return 1
}

# Whether a program run with `FENNEC_COUNTS=1`, which said what's in `$1`, ended holding no block:
# its counts line, the last thing it writes, says `live 0`. One left live is a block nothing freed.
none_live() {
  if grep -qE ' live [1-9][0-9]*$' "$1"; then
    echo "it ended with blocks still live: $(grep -E ' live [0-9]+$' "$1" | tail -n 1)" >&2
    return 1
  fi
}

# Every example and kit through the command, as the first thing a person would do with them, each
# counting what it holds, so one that ends with a block nothing freed fails too. Each one is
# tried, and each that fails is named by its path.
examples() {
  local printed="$logs/printed.txt" said="$logs/said.txt" program kit failed=0
  for program in examples/*.fen; do
    if ! { FENNEC_COUNTS=1 fennec_ok run "$program" > "$printed" 2> "$said" \
      && none_live "$said" \
      && diff --strip-trailing-cr "${program%.fen}.out" "$printed" \
      && fennec_ok test "$program" > /dev/null; }; then
      cat "$said" >&2
      echo "$program" >> "$names"
      failed=1
    fi
  done
  for kit in examples/projects/*/; do
    [ -f "$kit/kit.toml" ] || continue
    if ! { FENNEC_COUNTS=1 fennec_ok run "$kit" > "$printed" 2> "$said" \
      && none_live "$said" \
      && diff --strip-trailing-cr "${kit%/}.out" "$printed" \
      && fennec_ok test "$kit" > /dev/null; }; then
      cat "$said" >&2
      echo "${kit%/}" >> "$names"
      failed=1
    fi
  done
  # Every workers fixture with a pinned output, as CI's jobs on Linux run them: built natively with
  # the threads runtime, run with no arguments at 1 and 4 threads, and holding to its `.out`, sorted,
  # or the hash in its `.sorted.sha256`. A fixture with neither is driven by its own test. Without
  # this, one that ran only with an argument passed the gate and failed three of CI's jobs (#1769).
  local fixture wanted threads
  for fixture in crates/fennec/tests/fixtures/workers/*.fen; do
    if [ -f "${fixture%.fen}.out" ]; then
      wanted=$(LC_ALL=C sort "${fixture%.fen}.out" | shasum -a 256 | cut -d' ' -f1)
    elif [ -f "${fixture%.fen}.sorted.sha256" ]; then
      wanted=$(cut -d' ' -f1 "${fixture%.fen}.sorted.sha256")
    else
      continue
    fi
    if ! FENNEC_THREADS_RUNTIME=1 fennec_ok build "$fixture" -o "$logs/worker" > /dev/null 2> "$said"; then
      cat "$said" >&2
      echo "workers:$fixture" >> "$names"
      failed=1
      continue
    fi
    for threads in 1 4; do
      if ! FENNEC_THREADS=$threads .github/scripts/within.sh 120 "$logs/worker" < /dev/null > "$printed" 2> "$said" \
        || [ "$(LC_ALL=C sort "$printed" | shasum -a 256 | cut -d' ' -f1)" != "$wanted" ]; then
        echo "$fixture at $threads threads printed something other than it should, or failed:" >&2
        cat "$said" >&2
        echo "workers:$fixture@$threads" >> "$names"
        failed=1
      fi
    done
  done
  # The oracle (crates/oracle) runs each example a second way, from its source. One it runs has
  # to print what the example's `.out` says and end with 0; one it doesn't model, which it says
  # by ending with 3, is skipped. A difference is named `oracle:<path>`.
  cargo build --locked --quiet -p fennec-oracle || return 1
  for program in examples/*.fen; do
    if ! oracle_agrees "$program" "${program%.fen}.out"; then
      echo "oracle:$program" >> "$names"
      failed=1
    fi
  done
  return "$failed"
}

# Whether the oracle, run on the program `$1`, agrees with the output `$2` it should print: it
# prints that and ends with 0, or it doesn't model the program and ends with 3.
oracle_agrees() {
  local status=0 printed="$logs/oracle.txt"
  .github/scripts/within.sh 120 ./target/debug/fennec-oracle run "$1" > "$printed" 2> "$logs/oracle-said.txt" \
    || status=$?
  [ "$status" -eq 3 ] && return 0
  if [ "$status" -ne 0 ]; then
    echo "the oracle ran $1 and it ended with $status:" >&2
    cat "$logs/oracle-said.txt" >&2
    return 1
  fi
  diff --strip-trailing-cr "$2" "$printed" >&2 || { echo "the oracle ran $1 and printed something else" >&2; return 1; }
}

# What takes a minute or two, rather than the hour the rest takes, and has failed gates an hour
# in: each check runs, and what fails is named.
# - The Fennec code is formatted: the examples, the standard library and the benchmarks.
# - The examples pass the linter.
# - The uses of `unsafe` are what `.github/unsafe-allowed.txt` lists.
# - The ratchet of unchecked runtime calls, `crates/llvm/unchecked_runtime_calls.txt`, holds.
# - The crates from outside Fennec the runtime names are what `.github/runtime-crates.txt` lists.
# - Each benchmark kernel makes exactly the copies `bench/copies.txt` says.
# - The ThreadSanitizer suppressions keep to their rules, and the known failures' file to its form.
# - A hello world and a program using only `fs` link no SQLite, TLS or cryptography beyond what
#   `.github/linkage-known.txt` lists.
# - The Rust docs build as CI's lint job builds them, with any warning an error: a public item's
#   doc that links to a private one fails there.
# - The workflows pass actionlint and zizmor, where they're installed.
# - The landing scripts' own tests pass on their throwaway repositories, with no git settings but
#   git's own, as on CI's runners.
quick() {
  local failed=0 program said="$logs/quick.txt"
  if ! ./target/debug/fennec fmt --check examples crates/syntax/src/stdlib bench; then
    echo "fennec fmt --check" >> "$names"
    failed=1
  fi
  for program in examples/*.fen examples/projects/*/; do
    [ -f "$program" ] || [ -f "$program/kit.toml" ] || continue
    if ! .github/scripts/within.sh 60 ./target/debug/fennec lint "$program" > /dev/null; then
      echo "lint:$program" >> "$names"
      failed=1
    fi
  done
  # As CI's lint job counts them: every use outside a comment, by file, in one perl.
  git ls-files -z '*.rs' | perl -e 'my @files = split /\0/, do { local $/; <STDIN> }; my %c;
    for my $file (@files) { open(my $f, "<", $file) or die "$file: $!";
      while (<$f>) { next if m{^\s*//}; $c{$file} += () = /\bunsafe\b/g } }
    printf "%s: %d\n", $_, $c{$_} for grep { $c{$_} } sort keys %c' > "$logs/unsafe.txt"
  if ! diff -u <(grep -v '^#' .github/unsafe-allowed.txt) "$logs/unsafe.txt"; then
    echo "the uses of unsafe changed: update .github/unsafe-allowed.txt, and say why" >&2
    echo "the uses of unsafe" >> "$names"
    failed=1
  fi
  if ! cargo test --locked -p fennec-llvm --lib -- failure_checks:: > "$said" 2>&1 \
    || ! cargo test --locked -p fennec --test fennec -- --exact examples::benchmark_kernels_copy_what_was_pinned >> "$said" 2>&1; then
    cat "$said"
    .github/scripts/known-failures.sh tests-failed "$said" >> "$names"
    failed=1
  fi
  .github/scripts/runtime-crates.sh || { echo "runtime-crates" >> "$names"; failed=1; }
  .github/scripts/tsan-suppressions-check.sh || { echo "tsan-suppressions-check" >> "$names"; failed=1; }
  .github/scripts/test-known-failures.sh > /dev/null || { echo "test-known-failures" >> "$names"; failed=1; }
  .github/scripts/linkage-check.sh target/debug/fennec || { echo "linkage-check" >> "$names"; failed=1; }
  # With no git settings but git's own, as CI's runners have: this Mac's git names a new
  # repository's first branch `main`, which hid two tests that needed it, and CI found them.
  local script
  for script in test-fetch test-open-pr test-landing test-sweep-worktrees test-flakes test-nightly-report test-mutants test-gate-lock; do
    if ! GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null ".github/scripts/$script.sh" > "$said" 2>&1; then
      cat "$said"
      echo "$script" >> "$names"
      failed=1
    fi
  done
  if ! RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked > "$said" 2>&1; then
    cat "$said"
    echo "cargo doc" >> "$names"
    failed=1
  fi
  if command -v actionlint > /dev/null && ! actionlint; then
    echo "actionlint" >> "$names"
    failed=1
  fi
  if command -v zizmor > /dev/null && ! zizmor --offline --min-severity low -q .github/workflows; then
    echo "zizmor" >> "$names"
    failed=1
  fi
  return "$failed"
}

# Every test, with `--no-fail-fast`, so a test binary that fails doesn't stop the rest. Each test
# that fails is named exactly, as `cargo test` says it; a binary that fails without naming one,
# by not building or by crashing, is named as that, which nothing known matches.
#
# In CI's three parts, with CI's commands (see "Test" in `ci.yml`), so the two can't disagree about
# what's built with what. A `cargo test` of the whole workspace builds every package against
# `fennec` with `std-private`, which only `fennec`'s own tests turn on, through its dev-dependency on
# itself; CI's other packages get `fennec` without it. So #2353 passed the gate, the fuzzer's
# `postgres` reader built against std's private names, and broke CI's "others" on every system.
# Each part runs even after one fails, as each is a job of its own in CI.
tests() {
  local said="$logs/tests.txt" status=0
  : > "$said"
  # A failing test keeps its temporary folder and says where (see `fennec-temp`), for the step's
  # failure to keep with its log.
  FENNEC_KEEP_FAILED=1 cargo test --locked --no-fail-fast -p fennec --test fennec 'llvm::' >> "$said" 2>&1 \
    || status=$?
  FENNEC_KEEP_FAILED=1 cargo test --locked --no-fail-fast -p fennec -- --skip 'llvm::' >> "$said" 2>&1 \
    || status=$?
  { cargo build --locked -p fennec --features std-private \
    && FENNEC_KEEP_FAILED=1 cargo test --locked --no-fail-fast --workspace --exclude fennec; } >> "$said" 2>&1 \
    || status=$?
  cat "$said"
  [ "$status" -eq 0 ] && return 0
  .github/scripts/known-failures.sh tests-failed "$said" >> "$names"
  return "$status"
}

# x86-64 is CI's to check, on its native x86-64 Linux and macOS runners, not this Mac's under
# Rosetta: every gate built every example as a new x86-64 program, and Rosetta keeps a translation of
# each one it runs, in a folder no cleanup here reaches.
watched() {
  "$@" &
  local pid=$!
  .github/scripts/gate-watch.sh "$pid" "$log" "" "" "" "$logs/holds.txt" &
  local watcher=$!
  wait "$pid"
  local status=$?
  wait "$watcher" 2> /dev/null
  return "$status"
}

started=$SECONDS
# A quick check that fails, on anything main isn't known to fail, stops the gate after it.
stopped=0
step "format" cargo fmt --check || stopped=1
built=1
step "build the command and the runtimes" runtime || built=0
if [ "$built" -eq 1 ]; then
  step "the quick checks" quick || stopped=1
else
  skipped "the quick checks" "since the build failed"
fi
for name in "clippy" "the workspace and its tests compile on Linux" "the workspace and its tests compile for Windows" \
  "tests" "examples and kits"; do
  if [ "$stopped" -eq 1 ]; then
    skipped "$name" "since a quick check failed: fix it, and run the gate again"
    continue
  fi
  case "$name" in
    # With the runtime's test-only fault hook too, whose reading of `FENNEC_FAULTS` is built only
    # with `faults`.
    clippy) step "$name" bash -c 'cargo clippy --all-targets --locked -- -D warnings &&
      cargo clippy --locked -p fennec-runtime --features faults -- -D warnings' ;;
    *Linux) step "$name" .github/scripts/linux-check.sh ;;
    *Windows) step "$name" .github/scripts/windows-check.sh ;;
    *)
      if [ "$built" -eq 0 ]; then
        skipped "$name" "since the build failed"
        continue
      fi
      case "$name" in
        tests) step "$name" watched tests ;;
        *) step "$name" watched examples ;;
      esac
      ;;
  esac
done
if [ -s "$log" ]; then
  echo "the gate stopped test programs:"
  cat "$log"
  .github/scripts/flakes.sh failed "test programs the gate stopped" "$log"
  outcomes+=("test programs the gate stopped|failed")
fi

# How many new executables each step made.
echo "new executables made: $(wc -l < "$FENNEC_NEW_EXECUTABLES" | tr -d ' ')"
for each in "${made[@]}"; do
  [ "${each##*|}" -gt 0 ] && printf '  %6d  %s\n' "${each##*|}" "${each%|*}"
done
# How long macOS held brand-new programs at launch, which counts against no limit, but is time
# the gate waited.
if [ -s "$logs/holds.txt" ]; then
  sort -rn "$logs/holds.txt" | awk '
    { count++; total += $1; if (NR <= 5) longest[NR] = $0 }
    END {
      printf "%d test programs were held at launch, %ds altogether; the longest:\n", count, total
      for (at = 1; at <= NR && at <= 5; at++) printf "  %ss  %s\n", substr(longest[at], 1, index(longest[at], " ") - 1), substr(longest[at], index(longest[at], " ") + 1, 120)
    }'
fi

# A line for every step, and the verdict: every step ran, and failed on nothing main isn't known
# to fail.
echo
[ -z "$low_disk" ] || echo "$low_disk"
echo "the gate's steps:"
passed=1
for outcome in "${outcomes[@]}"; do
  printf '  %-12s %s\n' "${outcome##*|}" "${outcome%|*}"
  case "${outcome##*|}" in
    passed | known) ;;
    *) passed=0 ;;
  esac
done
echo "$((SECONDS - started))s  altogether"
if [ "$passed" -eq 0 ]; then
  # What gate-watch stopped, and what each was doing, printed above, kept beyond the gate's own
  # folder, which goes when it ends.
  if [ -s "$log" ]; then
    kept="${FENNEC_HOME:-$HOME/.fennec}/gate-logs"
    mkdir -p "$kept"
    kept="$kept/$(date '+%Y-%m-%d-%H%M%S')-$(git rev-parse --short HEAD).log"
    cp "$log" "$kept"
    echo "what gate-watch stopped, and what each was doing, is kept in $kept"
  fi
  echo "the gate failed"
  exit 1
fi
# Whatever failed before on this same code passed now, so each is a flake, written in the ledger.
.github/scripts/flakes.sh passed
