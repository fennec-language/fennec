#!/usr/bin/env bash
# Says which machine a job got: its processor, its cores and its memory. GitHub's runners of one
# label come in different speeds (on 2026-11-17 one Arm Mac built and tested at under half the
# speed of another), so a job's time means something only beside the machine it ran on.
#
# It only reports, so a value it can't read is said to be unknown rather than failing the job.
set -uo pipefail
cpu='' cores='' bytes=''
case "$(uname -s)" in
  Darwin)
    cpu=$(sysctl -n machdep.cpu.brand_string 2> /dev/null)
    cores=$(sysctl -n hw.ncpu 2> /dev/null)
    bytes=$(sysctl -n hw.memsize 2> /dev/null)
    ;;
  Linux)
    cpu=$(lscpu 2> /dev/null | sed -n 's/^Model name: *//p' | head -n 1)
    cores=$(nproc 2> /dev/null)
    kilobytes=$(sed -n 's/^MemTotal: *\([0-9]*\) kB$/\1/p' /proc/meminfo 2> /dev/null)
    bytes=${kilobytes:+$((kilobytes * 1024))}
    ;;
  *)
    cpu=${PROCESSOR_IDENTIFIER:-}
    cores=${NUMBER_OF_PROCESSORS:-}
    bytes=$(powershell -NoProfile -Command '(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory' 2> /dev/null | tr -d '\r')
    ;;
esac
memory='unknown memory'
[[ $bytes =~ ^[0-9]+$ ]] && memory="$((bytes / 1073741824)) GB"
echo "${cpu:-an unknown processor}, ${cores:-unknown} cores, $memory"
