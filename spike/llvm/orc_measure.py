#!/usr/bin/env python3
"""The follow-up: kernels through ORC next to everything else, edits to running, a live swap and
memory. One program at a time, under the watchdog in wd.py."""
import json, os, re, subprocess, sys, time
from pathlib import Path
import measure as m
import wd

WORK = m.WORK
PROTO = m.PROTO
OUT = m.RESULTS / "orc.jsonl"
NOMEM = dict(os.environ, ORC_NO_MEMORY="1")


def note(row):
    row["load"] = os.getloadavg()[0]
    row["at"] = time.strftime("%H:%M:%S")
    with open(OUT, "a") as f:
        f.write(json.dumps(row) + "\n")


def emit(source, out, import_=40):
    m.fresh(out)
    r = m.must(wd.run([PROTO / "llvm-spike", source, out, "--indirect", "--import", str(import_)]))
    return json.loads(r["stdout"])


def build(emitted, tier):
    passes, cg, isel = m.TIERS[tier]
    objs = emitted / f"objs_{tier}"
    m.fresh(objs)
    stats, r = m.driver_all(sorted((emitted / "fn").glob("*.ll")), passes, cg, isel, objs)
    return objs, stats, r


def changed_list(base, edited):
    names = []
    for path in sorted((edited / "fn").glob("*.ll")):
        if (base / "fn" / path.name).read_bytes() != path.read_bytes():
            names.append(path.name)
    (edited / "changed.txt").write_text("\n".join(names) + "\n")


def merged(objs):
    listing = objs / "merged.objs"
    listing.write_text("\n".join(str(p) for p in sorted(objs.glob("*.o"))) + "\n")
    m.must(wd.run(["/usr/bin/ld", "-r", "-arch", "arm64", "-o", objs / "merged.obj", "-filelist", listing]))


def host(emitted, objs, tier, *flags, env=None, limit=60):
    passes, cg, _ = m.TIERS[tier]
    cmd = [PROTO / "orc", "--dir", emitted, "--objs", objs, "--rt", WORK / "rt.o", "--passes", passes, "--cg", str(cg)] + list(flags)
    return m.must(wd.run(cmd, env=env, limit_secs=limit))


def rows_of(text):
    return [json.loads(line) for line in text.splitlines() if line.startswith("{")]


def kernels():
    m.rt_object()
    env = dict(os.environ, FENNEC_HOME=str(WORK / "fh_base"))
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
        emitted = WORK / "orc" / k
        emit(src, emitted)
        for tier in ["L1d", "O1", "O3"]:
            objs, _, _ = build(emitted, tier)
            passes, cg, _ = m.TIERS[tier]
            base = [PROTO / "orc", "--dir", emitted, "--objs", objs, "--rt", WORK / "rt.o", "--passes", passes, "--cg", str(cg)]
            progs[f"orc_{tier}"] = base
            progs[f"orc_{tier}_start"] = base + ["--no-run"]
        expected = None
        best = {}
        loads = []
        for _ in range(15):
            loads.append(os.getloadavg()[0])
            for name, cmd in progs.items():
                r = wd.run(cmd, env=dict(env, ORC_NO_MEMORY="1"))
                assert r["rc"] == 0, r["stderr"]
                printed = "".join(line + "\n" for line in r["stdout"].splitlines() if not line.startswith("{"))
                if name == "twin":
                    expected = printed
                if expected and not name.startswith("empty") and not name.endswith(("_start", "start")):
                    assert printed == expected, (name, printed, expected)
                b = best.setdefault(name, {"cycles": r["cycles"], "wall": r["wall_ms_outer"], "instructions": r["instructions"]})
                b["cycles"] = min(b["cycles"], r["cycles"])
                b["wall"] = min(b["wall"], r["wall_ms_outer"])
                b["instructions"] = min(b["instructions"], r["instructions"])
        with open(m.RESULTS / "cycles.jsonl", "a") as f:
            f.write(json.dumps({"kernel": k, "best": best, "load_min": min(loads), "load_max": max(loads), "at": time.strftime("%H:%M:%S")}) + "\n")


def edit_text(text, at, which, rep=0):
    body = text[at:at + 2000]
    if which == "wide":
        place = at + body.index("x * 3 + 7") + 4
        digit = str(rep + 4)
    elif which == "small":
        place = at + body.index("0.5 *") + 2
        digit = str(rep + 6)
    else:
        found = re.search(r"\d+\.\d+|\d{2,}", body[body.index("\n"):])
        place = at + body.index("\n") + found.start()
        digit = str(rep + 3) if text[place] != str(rep + 3) else "9"
    return text[:place] + digit + text[place + 1:]


def scale(n):
    m.rt_object()
    big = WORK / f"orcbig{n}"
    big.mkdir(parents=True, exist_ok=True)
    for label, wide in [("normal", False)]:
        args = ["python3", Path(__file__).parent / "gen_big.py", str(n)] + (["wide"] if wide else [])
        text = subprocess.run(args, capture_output=True, text=True).stdout
        source = big / f"{label}.fen"
        source.write_text(text)
        edits = {"wide": text.index("def helper(")} if wide else {"medium": text.index(f"def f{n // 2}(n: int) -> int:"), "small": text.index("def energy_0(p: P0) -> float:")}
        # The JIT on the same edits, and what it prints, to compare with.
        home = big / f"fh_{label}"
        env = dict(os.environ, FENNEC_HOME=str(home))
        expected = m.must(wd.run([m.FENNEC, "run", source], env=env))["stdout"]
        for which, at in edits.items():
            for rep in range(3):
                source.write_text(text)
                m.must(wd.run([m.FENNEC, "dev", "startup", source], env=env))
                source.write_text(edit_text(text, at, which, rep))
                r = m.must(wd.run([m.FENNEC, "dev", "startup", source], env=env))
                note({"what": "jit_edit", "program": label, "edit": which, "wall": r["wall_ms_outer"], "rss": r["rss"], "footprint": r["footprint"]})
        source.write_text(text)
        for import_ in [40]:
            emitted = big / f"{label}_i{import_}"
            info = emit(source, emitted, import_)
            note({"what": "front", "program": label, "import": import_, **info})
            edit_dirs = {}
            for which, at in edits.items():
                edited_source = big / f"{label}_{which}.fen"
                edited_source.write_text(edit_text(text, at, which))
                out = big / f"{label}_i{import_}_{which}"
                info2 = emit(edited_source, out, import_)
                note({"what": "front_edit", "program": label, "import": import_, "edit": which, **info2})
                changed_list(emitted, out)
                edit_dirs[which] = out
            for tier in ["O3"]:
                m.quiet(limit=10, wait=120)
                objs, stats, cold = build(emitted, tier)
                merged(objs)
                note({"what": "cold", "program": label, "import": import_, "tier": tier, "wall": cold["wall_ms_outer"], "rss": cold["rss"], "footprint": cold["footprint"], "obj_bytes": stats["obj_bytes"], "parse_ms": stats["parse_ms"], "opt_ms": stats["opt_ms"], "cg_ms": stats["cg_ms"]})
                flags = []
                for out in edit_dirs.values():
                    flags += ["--edit", out]
                r = host(emitted, objs, tier, *flags, "--reps", "5")
                note({"what": "host", "program": label, "import": import_, "tier": tier, "peak_rss": r["rss"], "peak_footprint": r["footprint"], "rows": rows_of(r["stdout"]), "matches_jit": r["stdout"].splitlines()[-1] == expected.splitlines()[-1]})
                r = host(emitted, objs, tier, env=NOMEM)
                note({"what": "host_run", "program": label, "import": import_, "tier": tier, "wall": r["wall_ms_outer"], "peak_rss": r["rss"], "peak_footprint": r["footprint"]})


def live():
    m.rt_object()
    source = Path(__file__).parent / "hot.fen"
    text = source.read_text()
    edited = WORK / "hot_edit.fen"
    edited.write_text(text.replace("i * 2 + 1", "i * 2 + 5"))
    for import_ in [0, 40]:
        emitted = WORK / f"hot_i{import_}"
        out = WORK / f"hot_i{import_}_edit"
        emit(source, emitted, import_)
        emit(edited, out, import_)
        changed_list(emitted, out)
        for tier in ["O1", "O3"]:
            objs, _, _ = build(emitted, tier)
            r = host(emitted, objs, tier, "--live", out, "--after", "150", env=NOMEM)
            values = [line for line in r["stdout"].splitlines() if not line.startswith("{")]
            changes = sum(1 for a, b in zip(values, values[1:]) if a != b)
            note({"what": "live", "import": import_, "tier": tier, "rounds": len(values), "first": values[0], "last": values[-1], "times_changed": changes, "switched_at_round": next((i for i, v in enumerate(values) if v != values[0]), None), "swap": r["stderr"].strip(), "wall": r["wall_ms_outer"]})


def llvm_idle():
    m.rt_object()
    r = wd.run([PROTO / "orc", "--no-load", "--rt", WORK / "rt.o"])
    note({"what": "llvm_only", "rows": rows_of(r["stdout"]), "peak_rss": r["rss"], "peak_footprint": r["footprint"]})


if __name__ == "__main__":
    for step in sys.argv[1:]:
        if step.startswith("scale"):
            scale(int(step[5:]))
        else:
            globals()[step]()
