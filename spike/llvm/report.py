#!/usr/bin/env python3
"""Turns results/*.jsonl into tables: speed per kernel and tier, against the twins."""
import json, math, sys
from collections import defaultdict
from pathlib import Path

R = Path(__file__).parent / "results"
base = {}
for line in open(R / "baseline.jsonl"):
    r = json.loads(line)
    base[(r["what"], r.get("kernel"))] = r
ec, er = base[("empty_c", None)], base[("empty_rs", None)]
KERNELS = ["fib", "sieve", "mandelbrot", "nbody", "spectral_norm", "fannkuch"]
pinned = {}
for line in open(Path(__file__).parents[2] / "bench/targets/aarch64.txt"):
    if line.strip() and not line.startswith("#"):
        p = line.split()
        pinned[p[0]] = int(p[1])


def gm(xs):
    xs = [x for x in xs if x and x > 0]
    return math.exp(sum(map(math.log, xs)) / len(xs)) if xs else 0


def scores(k, instr, cycles, start_i, start_c):
    twin = base[("twin_plain", k)]
    twin_ck = base[("twin_checked", k)]
    wi, wc = instr - start_i, cycles - start_c
    return {
        "i": pinned[k] / wi,
        "c": (twin["cycles"]) / wc,
        "ick": twin_ck["instructions"] / wi,
        "cck": twin_ck["cycles"] / wc,
    }


rows = defaultdict(dict)
for k in KERNELS:
    j = base[("jit", k)]
    rows[("JIT", "checked", "-")][k] = scores(k, j["instructions"], j["cycles"], j["startup_instructions"], j["startup_cycles"])
bad = []
for line in open(R / "speed.jsonl"):
    r = json.loads(line)
    if "failed" in r:
        bad.append(r)
        continue
    if r["out"] != base[("twin_plain", r["kernel"])]["out"]:
        bad.append({"wrong output": r})
    rows[(r["tier"], r["abi"], r["mode"])][r["kernel"]] = {
        **scores(r["kernel"], r["instructions"], r["cycles"], ec["instructions"], ec["cycles"]),
        "row": r,
    }
order = ["L0", "L0m", "L1a", "L1b", "L1c", "L1d", "L1dg", "O1", "O1g", "O1c0", "O2c1", "O2", "O3"]


def key(item):
    t, a, m = item[0]
    return (t != "JIT", order.index(t) if t in order else 50, a, ["-", "whole", "split", "import"].index(m))


measure = sys.argv[1] if len(sys.argv) > 1 else "c"
print(f"speed, % of target, measure {measure} (i: pinned instructions; c: local twin's cycles; *ck: checked twin)")
print(f"{'tier':6}{'abi':8}{'mode':7}" + "".join(f"{k[:9]:>10}" for k in KERNELS) + f"{'geomean':>9}{'min':>6}")
for (t, a, m), v in sorted(rows.items(), key=key):
    vals = [v[k][measure] if k in v else None for k in KERNELS]
    cells = "".join(f"{x * 100:10.0f}" if x else f"{'-':>10}" for x in vals)
    print(f"{t:6}{a:8}{m:7}{cells}{gm(vals) * 100:9.0f}{min(x for x in vals if x) * 100:6.0f}")
if bad:
    print("problems:", json.dumps(bad)[:800])
