#!/usr/bin/env bash
# mutants-tests.sh PACKAGE...: the tests a mutant in these packages runs, as two lines: the
# packages whose tests are built, for cargo-mutants' `--test-package`, then the nextest filter that
# picks the tests. That's each package's own tests, and a small set of `fennec`'s that run
# programs end to end:
# - `newcomers::`, `syntax_errors::` and `hints::`: what the checker says, through the command;
# - `executables::`: programs built, run and killed at their limits;
# - the error examples, each failing as it says;
# - a sample of `llvm::`, programs built through LLVM and run: a ninth of them, those whose name
#   ends in g, h, n, o, w or x, which picks as a hash would;
# - and by name, `llvm::` tests outside the sample that are the only ones to catch a mutant the
#   sample missed (2026-11-21): a copy that mustn't see a change, a put under a lock that keeps
#   nothing more, and an index into a big array; and a failed lock's block putting back what it
#   changed, of every kind, an enum's and two values' at once (2026-11-22);
# - `fennec`'s own unit tests. For a mutant in `fennec` itself, that's its set: its other tests
#   take 19 minutes.
#
# A mutant used to run every test in the workspace, which takes CI three jobs of 17 to 28
# minutes. Every one timed out at 420 seconds, so for weeks no mutant was caught or missed
# (2026-11-20). Timed then on a runner under nextest, as mutants run them: fennec-check's own
# tests took 138 s, fennec-ir's 167 s, fennec-llvm's 88 s and fennec-rt's 8 s. The set above is
# 135 tests and 262 s of test time, none over 17 s. All of `llvm::` is 630 tests and 2,300 s.
#
#   .github/scripts/mutants-tests.sh fennec-ir
#   .github/scripts/mutants-tests.sh --diff changes.diff
#
# With `--diff`, the packages are those whose files the diff changes.
set -euo pipefail
here="$(cd "$(dirname "$0")/../.." && pwd)"
if [ "${1:-}" = --diff ]; then
  # A package is a folder under `crates/` with a `Cargo.toml`, named by its `name =`.
  packages=()
  while read -r folder; do
    manifest="$here/crates/$folder/Cargo.toml"
    [ -f "$manifest" ] || continue
    packages+=("$(sed -n 's/^name = "\(.*\)"$/\1/p' "$manifest" | head -n 1)")
  done < <(sed -n 's#^+++ b/crates/\([^/]*\)/.*#\1#p' "$2" | sort -u)
  set -- ${packages[@]+"${packages[@]}"}
fi
end_to_end='test(/^(newcomers|syntax_errors|hints|executables)::/)'
end_to_end+=' | test(=examples::error_examples_the_checker_catches_fail_with_their_expected_message)'
end_to_end+=' | test(/^llvm::.*[ghnowx]$/)'
end_to_end+=' | test(=llvm::changing_an_item_doesnt_show_through_a_copy)'
end_to_end+=' | test(=llvm::a_put_where_a_lookup_found_the_key_holds_nothing_more_kept_or_undone)'
end_to_end+=' | test(=llvm::a_big_array_behaves_as_a_small_one)'
end_to_end+=' | test(=llvm::a_failure_out_of_a_lock_puts_back_every_kind_of_write)'
end_to_end+=' | test(=llvm::a_failure_out_of_a_lock_puts_back_every_kind_of_field)'
end_to_end+=' | test(=llvm::a_failure_out_of_a_lock_puts_back_an_enum_field)'
end_to_end+=' | test(=llvm::a_failure_out_of_a_lock_puts_back_each_word_of_an_enum_holding_only_numbers)'
end_to_end+=' | test(=llvm::a_crash_in_a_lock_of_two_values_puts_both_back_whatever_their_order)'
filter="(package(fennec) & (kind(lib) | $end_to_end))"
built=fennec
for package in "$@"; do
  [ "$package" != fennec ] || continue
  filter="package($package) | $filter"
  built="$package,$built"
done
echo "$built"
echo "$filter"
