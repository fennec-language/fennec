#!/usr/bin/env python3
"""The run-speed re-run: twins, the JIT and LLVM exes take turns, 15 rounds, keeping each one's
minimum, so they share whatever the machine is doing. Work is the run minus an empty start."""
import json, os, time
from pathlib import Path
import measure as m
import wd

WORK = m.WORK
env = dict(os.environ, FENNEC_HOME=str(WORK / "fh_base"))
out = open(m.RESULTS / "cycles.jsonl", "a")
for k in m.KERNELS:
    m.quiet(limit=10, wait=120)
    src = m.ROOT / f"bench/kernels/{k}.fen"
    progs = {
        "empty_c": [WORK / "twins/empty_c"], "empty_rs": [WORK / "twins/empty_rs"],
        "twin": [WORK / f"twins/{k}_plain"], "twin_checked": [WORK / f"twins/{k}_checked"],
        "jit": [m.FENNEC, "run", src], "jit_start": [m.FENNEC, "dev", "startup", src],
    }
    for mode in ["whole", "split", "import"]:
        for tier in ["L1d", "O1", "O3"]:
            progs[f"{tier}_{mode}"] = [WORK / f"speed/{k}_checked_{mode}/{tier}/exe"]
    best = {}
    loads = []
    for _ in range(15):
        loads.append(os.getloadavg()[0])
        for name, cmd in progs.items():
            r = wd.run(cmd, env=env)
            assert r["rc"] == 0, r["stderr"]
            b = best.setdefault(name, {"cycles": r["cycles"], "wall": r["wall_ms_outer"], "instructions": r["instructions"]})
            b["cycles"] = min(b["cycles"], r["cycles"])
            b["wall"] = min(b["wall"], r["wall_ms_outer"])
            b["instructions"] = min(b["instructions"], r["instructions"])
    out.write(json.dumps({"kernel": k, "best": best, "load_min": min(loads), "load_max": max(loads), "at": time.strftime("%H:%M:%S")}) + "\n")
    out.flush()
