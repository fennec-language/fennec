#!/usr/bin/env perl
# Writes soak's copy of a program, read from standard input, to standard output: the original's
# `main` renamed `soak_round`, a `soak_rounds` that runs rounds of it one after another until the
# soak stops it, and a `main` with the same parameters and failures that runs them, in a task with
# `SOAK_IN=task`, on the program's own thread with `SOAK_IN=main`. `soak.sh` runs it, and a test
# of Fennec's checks its copy of every program the soak runs, both ways, against the language as
# it is now, since what it writes is Fennec.
use strict;
use warnings;
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
