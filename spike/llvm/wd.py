#!/usr/bin/env python3
"""Runs one command under a watchdog (4 GB, 60 s by default), niced, via /usr/bin/time -l.

Importable: run(cmd, ...) returns wall ms, max RSS, peak footprint, instructions, stdout.
"""
import os, re, signal, subprocess, sys, tempfile, time

LIMIT_BYTES = 4 << 30
LIMIT_SECS = 60


def _rss_of_group(pgid):
    out = subprocess.run(["ps", "-A", "-o", "pgid=,rss="], capture_output=True, text=True).stdout
    total = 0
    for line in out.splitlines():
        a, b = line.split()
        if int(a) == pgid:
            total += int(b) * 1024
    return total


def run(cmd, limit_secs=LIMIT_SECS, limit_bytes=LIMIT_BYTES, env=None, cwd=None, stdout_to=None):
    with tempfile.NamedTemporaryFile("r", suffix=".time") as tf, tempfile.TemporaryFile("w+") as so:
        full = ["nice", "-n", "10", "/usr/bin/time", "-l", "-o", tf.name] + list(cmd)
        proc = subprocess.Popen(full, stdout=so, stderr=subprocess.PIPE, text=True, env=env, cwd=cwd, start_new_session=True)
        start = time.monotonic()
        next_rss = start + 0.25
        killed = None
        while proc.poll() is None:
            time.sleep(0.002)
            now = time.monotonic()
            if now - start > limit_secs:
                killed = "time"
            elif now > next_rss:
                next_rss = now + 0.25
                if _rss_of_group(proc.pid) > limit_bytes:
                    killed = "memory"
            if killed:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
                break
        wall = (time.monotonic() - start) * 1000
        err = proc.stderr.read()
        so.seek(0)
        out = so.read()
        text = tf.read()
    r = {"cmd": " ".join(map(str, cmd)), "rc": proc.returncode, "killed": killed, "wall_ms_outer": wall, "stdout": out, "stderr": err}
    m = re.search(r"([\d.]+) real", text)
    r["real_ms"] = float(m.group(1)) * 1000 if m else None
    for key, pat in [("rss", r"(\d+)\s+maximum resident set size"), ("footprint", r"(\d+)\s+peak memory footprint"), ("instructions", r"(\d+)\s+instructions retired"), ("cycles", r"(\d+)\s+cycles elapsed")]:
        m = re.search(pat, text)
        r[key] = int(m.group(1)) if m else None
    return r


if __name__ == "__main__":
    r = run(sys.argv[1:])
    sys.stdout.write(r["stdout"])
    sys.stderr.write(r["stderr"])
    print({k: v for k, v in r.items() if k not in ("stdout", "stderr")}, file=sys.stderr)
    sys.exit(r["rc"] or 0)
