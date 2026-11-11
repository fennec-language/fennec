#!/usr/bin/env bash
# Installs Ubuntu packages on a runner, updating the package lists first:
#
#   .github/scripts/apt.sh valgrind musl-tools
#
# A runner's DNS sometimes can't resolve the archive for a moment ("Temporary failure resolving"),
# which once turned main red, so each step tries three times, waiting longer each time, and apt
# itself retries each download three times.
set -u
tries=3

# Runs apt-get with its arguments, as root, until it works or it's failed `tries` times.
again() {
  local try wait=5
  for try in $(seq "$tries"); do
    sudo apt-get -o Acquire::Retries=3 "$@" && return 0
    [ "$try" -lt "$tries" ] || break
    echo "apt-get $1 failed; trying again in ${wait}s" >&2
    sleep "$wait"
    wait=$((wait * 3))
  done
  echo "::error::apt-get $1 failed $tries times" >&2
  return 1
}

again update -qq && again install -y -qq "$@"
