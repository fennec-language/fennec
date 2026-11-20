#!/usr/bin/env bash
# memory-held.sh LIMIT COMMAND...: runs COMMAND in a control group of its own whose memory is held
# to LIMIT, as `12G`, with no swap. A process under it that allocates without end is stopped by the
# kernel there, and not by the runner running out: then the runner goes, its job is lost with no
# logs, and nothing says why. Exits as COMMAND does, saying first how many processes the limit
# stopped, on the job's summary page too. Needs Linux's cgroup v2 and sudo, as GitHub's runners
# have.
set -euo pipefail
limit=$1
shift
group=/sys/fs/cgroup/memory-held-$$
sudo mkdir "$group"
echo "$limit" | sudo tee "$group/memory.max" > /dev/null
echo 0 | sudo tee "$group/memory.swap.max" > /dev/null
# This shell goes in first, so what it starts is in from its first allocation.
echo $$ | sudo tee "$group/cgroup.procs" > /dev/null
status=0
"$@" || status=$?
stopped=$(awk '$1 == "oom_kill" { print $2 }' "$group/memory.events")
said="The $limit memory limit stopped ${stopped:-0} process(es) that went past it."
echo "$said"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  echo "$said" >> "$GITHUB_STEP_SUMMARY"
fi
exit "$status"
