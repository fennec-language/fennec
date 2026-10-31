#!/usr/bin/env python3
"""One change at a time: each set of emitter flags on every kernel, per-function -O3 with copies.
Results against the twin with overflow checks, instructions first since they survive load."""
import hashlib, json, math, os, sys, time
import measure as m
import wd

CONFIGS = {
    "base": ["--abi", "checked"],
    "guard": ["--abi", "checked", "--guard"],
    "guard+weights": ["--abi", "checked", "--guard", "--weights"],
    "guard+weights+nofail": ["--abi", "checked", "--guard", "--weights", "--nofail"],
    "reg": ["--abi", "reg", "--guard", "--weights", "--nofail"],
    "fastcc": ["--abi", "checked", "--guard", "--weights", "--nofail", "--cc", "fastcc"],
    "preserve_none": ["--abi", "checked", "--guard", "--weights", "--nofail", "--cc", "preserve_nonecc"],
    "plain": ["--abi", "plain"],
}


def main(names, tier="O3", import_="40", runs=5):
    rt = m.rt_object()
    base = {}
    for line in open(m.RESULTS / "baseline.jsonl"):
        r = json.loads(line)
        base[(r["what"], r.get("kernel"))] = r
    latest = {json.loads(l)["kernel"]: json.loads(l)["best"] for l in open(m.RESULTS / "cycles.jsonl")}
    rows = {}
    for name in names:
        rows[name] = {}
        for kernel in m.KERNELS:
            source = m.ROOT / f"bench/kernels/{kernel}.fen"
            out = m.WORK / "steps" / f"{kernel}_{name}"
            m.fresh(out)
            m.must(wd.run([m.PROTO / "llvm-spike", source, out, "--import", import_] + CONFIGS[name]))
            passes, cg, isel = m.TIERS[tier]
            objs = out / "objs"
            m.fresh(objs)
            m.driver_all(sorted((out / "fn").glob("*.ll")), passes, cg, isel, objs)
            exe = objs / "exe"
            m.link(exe, sorted(objs.glob("*.o")) + [rt])
            r = m.run_exe([exe], runs=runs)
            b = latest[kernel]
            assert r["stdout"].strip() and hashlib.sha256(r["stdout"].encode()).hexdigest()[:16] == base[("twin_plain", kernel)]["out"], (name, kernel, r["stdout"])
            empty_i, empty_c = b["empty_c"]["instructions"], b["empty_c"]["cycles"]
            ti = b["twin_checked"]["instructions"] - b["empty_rs"]["instructions"]
            tc = b["twin_checked"]["cycles"] - b["empty_rs"]["cycles"]
            pi = ti / (r["instructions_min"] - empty_i) * 100
            pc = tc / (r["cycles_min"] - empty_c) * 100
            rows[name][kernel] = (pi, pc)
            with open(m.RESULTS / "steps.jsonl", "a") as f:
                f.write(json.dumps({"config": name, "tier": tier, "import": import_, "kernel": kernel, "instructions": r["instructions_min"], "cycles": r["cycles_min"], "pct_instructions": pi, "pct_cycles": pc, "load": os.getloadavg()[0], "at": time.strftime("%H:%M:%S")}) + "\n")
    gm = lambda xs: math.exp(sum(map(math.log, xs)) / len(xs))
    print(f"{'':24}" + "".join(f"{k[:9]:>10}" for k in m.KERNELS) + f"{'geomean':>10}")
    for name, row in rows.items():
        print(f"{name:24}" + "".join(f"{row[k][0]:5.0f}/{row[k][1]:<4.0f}" for k in m.KERNELS) + f"{gm([row[k][0] for k in m.KERNELS]):5.0f}/{gm([row[k][1] for k in m.KERNELS]):<4.0f}")


if __name__ == "__main__":
    main(sys.argv[1:] or list(CONFIGS))
