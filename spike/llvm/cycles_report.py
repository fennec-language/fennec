#!/usr/bin/env python3
"""The kernels side by side: instructions / cycles as % of a Rust twin, work against work.
The twin with overflow checks is the main target; the unchecked twin is for reference."""
import json, math
from pathlib import Path
rows = [json.loads(l) for l in open(Path(__file__).parent / "results/cycles.jsonl")]
names = ["jit", "L1d_import", "O1_import", "O3_split", "O3_import", "O3_whole", "orc_O3", "twin"]
gm = lambda x: math.exp(sum(map(math.log, x)) / len(x))
for label, twin in [("against the twin WITH overflow checks (main)", "twin_checked"), ("against the unchecked twin (reference)", "twin")]:
    print(label)
    print(f"{'':14}" + "".join(f"{n[:10]:>11}" for n in names))
    g = {n: [] for n in names}; gi = {n: [] for n in names}
    for r in rows:
        b = r["best"]
        tc = b[twin]["cycles"] - b["empty_rs"]["cycles"]; ti = b[twin]["instructions"] - b["empty_rs"]["instructions"]
        cells = []
        for n in names:
            start = "jit_start" if n == "jit" else "empty_rs" if n.startswith("twin") else f"{n}_start" if n.startswith("orc") else "empty_c"
            pc = tc / (b[n]["cycles"] - b[start]["cycles"]) * 100; pi = ti / (b[n]["instructions"] - b[start]["instructions"]) * 100
            g[n].append(pc); gi[n].append(pi); cells.append(f"{pi:.0f}/{pc:.0f}")
        print(f"{r['kernel']:14}" + "".join(f"{c:>11}" for c in cells), f" load {r['load_min']:.0f}-{r['load_max']:.0f}")
    print(f"{'geomean':14}" + "".join(f"{gm(gi[n]):.0f}/{gm(g[n]):.0f}".rjust(11) for n in names))
    print(f"{'min':14}" + "".join(f"{min(gi[n]):.0f}/{min(g[n]):.0f}".rjust(11) for n in names))
