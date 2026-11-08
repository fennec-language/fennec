#!/usr/bin/env python3
"""Soak test: runs the soak server, built natively, under steady load for a set time, and says
whether it stayed leak-free: memory flat after warm-up, latency steady, no descriptors leaked,
the server never stopped, and every crash kept inside its own request.

    python3 bench/soak/soak.py SERVER_EXECUTABLE [--minutes 10] [--clients 8] [--out report.json]

About 1 request in 100 crashes on purpose, through an overflow, and others fail: a note that's
there already, a request that isn't HTTP, and a client that hangs up halfway. Not a required check:
run it by hand or nightly, under a watchdog.
"""

import argparse
import http.client
import json
import os
import random
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time

# What each client sends, by weight.
MIX = [
    ("count", 40),
    ("note", 22),
    ("duplicate", 3),
    ("file", 20),
    ("stats", 10),
    ("crash", 1),
    ("garbage", 2),
    ("hangup", 2),
]

# How long a request may take before it counts as stuck.
REQUEST_TIMEOUT = 10.0


class Totals:
    def __init__(self):
        self.lock = threading.Lock()
        self.latencies = []  # (time sent, seconds taken, kind)
        self.counts = {}
        self.unexpected = []

    def add(self, kind, outcome, sent=None, took=None):
        with self.lock:
            self.counts[(kind, outcome)] = self.counts.get((kind, outcome), 0) + 1
            if took is not None:
                self.latencies.append((sent, took, kind))

    def wrong(self, what):
        with self.lock:
            if len(self.unexpected) < 50:
                self.unexpected.append(what)
            self.counts[("unexpected", "")] = self.counts.get(("unexpected", ""), 0) + 1


def pick(rng):
    total = sum(weight for _, weight in MIX)
    at = rng.uniform(0, total)
    for kind, weight in MIX:
        at -= weight
        if at <= 0:
            return kind
    return MIX[0][0]


def reset_on_close(raw):
    """Closes with a reset, so a short connection leaves no TIME_WAIT behind on this side: tens of
    thousands of them would use up the client's ports, which isn't the server's doing."""
    raw.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))


def client(port, until, totals, seed, notes):
    rng = random.Random(seed)
    conn = None
    while time.time() < until:
        kind = pick(rng)
        if kind == "garbage":
            # Not HTTP: the server answers 400 without calling the handler.
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=REQUEST_TIMEOUT) as raw:
                    reset_on_close(raw)
                    raw.sendall(b"NOT HTTP AT ALL\r\n\r\n")
                    back = raw.recv(64)
                totals.add(kind, "4xx" if back.startswith(b"HTTP/1.1 4") else "other")
            except OSError as err:
                totals.wrong(f"garbage: {err}")
            continue
        if kind == "hangup":
            # Half a request, then gone.
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=REQUEST_TIMEOUT) as raw:
                    reset_on_close(raw)
                    raw.sendall(b"POST /note?n=1 HTTP/1.1\r\ncontent-length: 100\r\n\r\nhalf")
                totals.add(kind, "sent")
            except OSError as err:
                totals.wrong(f"hangup: {err}")
            continue
        if conn is None:
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=REQUEST_TIMEOUT)
        if kind == "count":
            method, path, body = "GET", "/count", None
        elif kind == "note":
            n = next(notes)
            method, path, body = "POST", f"/note?n={n}", f"note {n} " + "x" * rng.randint(10, 400)
        elif kind == "duplicate":
            method, path, body = "POST", "/note?n=-1", "the same note again"
        elif kind == "file":
            n = rng.randint(0, 1 << 30)
            method, path, body = "POST", f"/file?n={n}", "y" * rng.randint(100, 8000)
        elif kind == "stats":
            method, path, body = "GET", "/stats", None
        else:
            method, path, body = "GET", f"/crash?n={rng.randint(1, 100)}", None
        sent = time.time()
        try:
            conn.request(method, path, body=body)
            response = conn.getresponse()
            data = response.read()
            took = time.time() - sent
        except (OSError, http.client.HTTPException) as err:
            conn.close()
            conn = None
            if kind == "crash":
                # The crash ends its connection, and only that.
                totals.add(kind, "closed")
            else:
                totals.wrong(f"{kind}: {type(err).__name__}: {err}")
            continue
        status = response.status
        if kind == "crash":
            totals.wrong(f"crash answered {status}: {data[:80]!r}")
        elif kind == "duplicate":
            totals.add(kind, str(status), sent, took)
            if status != 500 and not data.isdigit():
                totals.wrong(f"duplicate answered {status}: {data[:80]!r}")
        elif status != 200:
            totals.wrong(f"{kind} answered {status}: {data[:80]!r}")
        else:
            totals.add(kind, "200", sent, took)
    if conn is not None:
        conn.close()


def rss_kb(pid):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout
    return int(out.strip() or 0)


def open_fds(pid):
    out = subprocess.run(["lsof", "-n", "-P", "-p", str(pid)], capture_output=True, text=True).stdout
    # Only descriptors: numbered FD column, not the program's text, cwd or mapped files.
    count = 0
    for line in out.splitlines()[1:]:
        parts = line.split()
        if len(parts) > 3 and parts[3][:1].isdigit():
            count += 1
    return count


def percentile(values, fraction):
    if not values:
        return 0.0
    values = sorted(values)
    return values[min(len(values) - 1, int(fraction * len(values)))]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("server")
    parser.add_argument("--minutes", type=float, default=10.0)
    parser.add_argument("--clients", type=int, default=8)
    parser.add_argument("--sample", type=float, default=5.0, help="seconds between samples")
    parser.add_argument("--out", default=None, help="where to write the report as JSON")
    parser.add_argument(
        "--threads",
        type=int,
        default=4,
        help="the server's workers; each warms caches up to a bound, so fewer reach it sooner",
    )
    parser.add_argument("--only", default=None, help="send only these kinds, like count,file, to find a leak")
    args = parser.parse_args()
    if args.only:
        wanted = set(args.only.split(","))
        MIX[:] = [(kind, weight) for kind, weight in MIX if kind in wanted]

    data = tempfile.mkdtemp(prefix="fennec-soak-")
    server = subprocess.Popen(
        [os.path.abspath(args.server), data],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        cwd=data,
        env={**os.environ, "FENNEC_THREADS": str(args.threads)},
    )
    port = int(server.stdout.readline())
    # What it says on standard error is each crashed request's report: read so it never blocks.
    errors = []
    threading.Thread(target=lambda: errors.extend(server.stderr), daemon=True).start()

    started = time.time()
    until = started + args.minutes * 60
    totals = Totals()
    lock = threading.Lock()
    counter = iter(range(1, 1 << 62))

    def notes():
        while True:
            with lock:
                yield next(counter)

    clients = [
        threading.Thread(target=client, args=(port, until, totals, seed, notes()), daemon=True)
        for seed in range(args.clients)
    ]
    for each in clients:
        each.start()

    samples = []
    stopped = None
    while time.time() < until:
        time.sleep(args.sample)
        if server.poll() is not None:
            stopped = server.returncode
            break
        samples.append((time.time() - started, rss_kb(server.pid), open_fds(server.pid)))
    for each in clients:
        each.join(timeout=REQUEST_TIMEOUT * 2)
    alive = server.poll() is None
    server.kill()
    server.wait()

    # Each worker's caches, its runtime's spare blocks and the allocator's pages, fill up to a
    # bound as it serves: that's the warm-up, the first 40% of the run. After it, memory and
    # descriptors have to stay flat.
    warm = [s for s in samples if s[0] >= args.minutes * 60 * 0.4]
    rss_after = [s[1] for s in warm]
    half = len(rss_after) // 2
    early = statistics.mean(rss_after[:half]) if half else 0
    late = statistics.mean(rss_after[half:]) if half else 0
    growth_kb = late - early
    fds_after = [s[2] for s in warm]

    # Latency, minute by minute.
    by_minute = {}
    for sent, took, _ in totals.latencies:
        by_minute.setdefault(int((sent - started) // 60), []).append(took)
    # Only whole minutes after the first, which warms up, count for whether it's steady.
    minutes = sorted(m for m in by_minute if (m + 1) * 60 <= args.minutes * 60)
    p99s = [percentile(by_minute[m], 0.99) for m in minutes]
    every = [took for _, took, _ in totals.latencies]
    answered = len(every)
    seconds = args.minutes * 60

    crashes = totals.counts.get(("crash", "closed"), 0)
    crash_reports = sum(1 for line in errors if "overflow" in line or "crash" in line.lower())
    report = {
        "minutes": args.minutes,
        "clients": args.clients,
        "threads": args.threads,
        "server_alive_at_end": alive,
        "server_stopped_with": stopped,
        "answered": answered,
        "throughput_per_s": round(answered / seconds, 1),
        "p50_ms": round(percentile(every, 0.5) * 1000, 2),
        "p99_ms": round(percentile(every, 0.99) * 1000, 2),
        "p99_ms_by_minute": [round(p * 1000, 2) for p in p99s],
        "rss_mb_start_end": [round(samples[0][1] / 1024, 1), round(samples[-1][1] / 1024, 1)] if samples else [],
        "rss_mb_after_warmup_min_max": [round(min(rss_after) / 1024, 1), round(max(rss_after) / 1024, 1)] if rss_after else [],
        "rss_growth_after_warmup_mb": round(growth_kb / 1024, 2),
        "fds_after_warmup_min_max": [min(fds_after), max(fds_after)] if fds_after else [],
        "crashes_sent": crashes,
        "crash_reports_on_stderr": crash_reports,
        "outcomes": {f"{kind} {outcome}".strip(): n for (kind, outcome), n in sorted(totals.counts.items())},
        "unexpected": totals.unexpected,
        "samples": samples,
    }
    # The verdict.
    problems = []
    if not alive:
        problems.append(f"the server stopped (exit {stopped})")
    if growth_kb > max(2048, 0.03 * early):
        problems.append(f"memory grew {growth_kb / 1024:.1f} MB after warm-up")
    if fds_after and max(fds_after) - min(fds_after) > 16:
        problems.append(f"descriptors went from {min(fds_after)} to {max(fds_after)}")
    if p99s and len(p99s) > 2 and max(p99s[1:]) > 5 * statistics.median(p99s[1:]) + 0.005:
        problems.append(f"latency wasn't steady: p99 by minute {report['p99_ms_by_minute']}")
    if totals.counts.get(("unexpected", ""), 0):
        problems.append(f"{totals.counts[('unexpected', '')]} requests went wrong that shouldn't have")
    if crashes == 0 and any(kind == "crash" for kind, _ in MIX):
        problems.append("no request crashed, so crashes weren't tested")
    report["problems"] = problems
    report["passed"] = not problems

    text = json.dumps(report, indent=1)
    if args.out:
        with open(args.out, "w") as out:
            out.write(text)
    shown = {key: value for key, value in report.items() if key != "samples"}
    print(json.dumps(shown, indent=1))
    print("PASSED" if not problems else "FAILED: " + "; ".join(problems))
    sys.exit(0 if not problems else 1)


if __name__ == "__main__":
    main()
